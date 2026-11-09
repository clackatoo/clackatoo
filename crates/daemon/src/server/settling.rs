//! Working out agents' changes to big files off the daemon's loop.
//!
//! When an agent's tools have made a file not open as they said, the edit
//! log and the review take in the change from the text before to the text
//! on disk (see `agent_files`). Working it out (the edit between the
//! two, the lines it changed, and the text after as a rope) takes as long
//! as the file: half a second for a file of a few megabytes in a debug
//! build, which would hold up every terminal and window. So for a big file
//! it's worked out on a thread of its own, and the loop takes it in once
//! it's done. Small files are worked out at once, as before.
//!
//! A change to a file open in a buffer is checked and merged into the
//! buffer's text the same way (CLACKA-201, CLACKA-222): the worker reads
//! the file, makes what the agent's tools said they changed of the text
//! saved, and compares the two; if the tools made the file, it merges the
//! change into a copy of the text, as it was at one version (ropes share
//! what they have in common, so the copy is cheap), and works out what the
//! review needs of it. The loop makes the merged edits only if the buffer
//! is still at that version, with the file as it was, so the merge's promise never to mix
//! the agent's change into lines changed and not saved still holds.
//! Otherwise, as when you typed meanwhile, or undid something, it's merged
//! again from the buffer as it is now, and after a few tries, on the loop.
//! A buffer closed meanwhile, or opened again as another, has the change
//! taken in as one to a file not open, from the text the buffer had saved.
//!
//! What's taken in is in order. While a file's change is being worked out
//! (see [`Diffing`]), what more the same chat's tools do to it waits, and
//! so does the end of the chat's turn. Anything else about the file, such
//! as another chat's write, opening it, or undoing or reviewing a turn,
//! first waits on the loop for the change to be worked out and takes it
//! in: a rare stall, as long as the worker's work, that keeps the edit
//! log and the review in the order the changes were made. A change that
//! spans turns, as one read only after the chat's next turn wrote the
//! file in place, is given the worker as one change for each turn,
//! together, so each turn's is undone on its own.
//!
//! A change the worker fails to work out, as if it panicked, is noted as
//! unknown, as one whose text before wasn't seen; a worker that's gone
//! fails every change it had, and the next is given a new one. A change
//! to a buffer that fails is merged again on the loop instead, as all it
//! needs is still at hand.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use clackatoo_protocol::TerminalId;

use clackatoo_protocol::BufferId;
use clackatoo_text::Rope;

use super::agent_files::{Checked, Checking, Reading, Settled};
use super::edit_log::{Diffed, Maker};
use crate::buffer::DiskStamp;

/// The most bytes of text, before and after together, worked out on the
/// loop. A change to a bigger file is worked out on the worker.
pub(super) const ON_LOOP: usize = 256 * 1024;

/// The most bytes of text given the worker at once, before and after
/// together. Past it, a change is worked out on the loop, rather than
/// holding more in memory, or making whatever waits for the worker wait
/// long: about a second's work for it in a debug build. A change past it
/// alone, with nothing else in flight, is still the worker's: the loop
/// never takes one for its size.
const IN_FLIGHT: usize = 32 * 1024 * 1024;

/// A change to a file not open being worked out off the loop, and what
/// waits for it.
#[derive(Debug)]
pub(super) struct Diffing {
    pub(super) chat: TerminalId,
    pub(super) path: PathBuf,
    /// Who made it, one for each turn whose change it holds, in order, by
    /// the turn's last tool, to note each unknown if it can't be worked
    /// out.
    pub(super) makers: Vec<Maker>,
    /// The chat's tools said more of the file meanwhile: it's settled
    /// again once this is taken in.
    pub(super) again: bool,
    /// The settling again is the turn's last.
    pub(super) last: bool,
    /// For a change to a file open in a buffer, being merged into it.
    pub(super) open: Option<Open>,
}

/// What a change being merged into buffer `id` is merged again from, if
/// it can't be made as merged.
#[derive(Clone, Debug)]
pub(super) struct Open {
    pub(super) id: BufferId,
    pub(super) source: Source,
    /// How many times it's been merged again.
    pub(super) tries: u32,
    /// The buffer's text saved as it was merged into, and whether the file
    /// was there then: what the file held before the change, if the
    /// worker is lost with the change, and the buffer has closed.
    pub(super) saved: Rope,
    pub(super) existed: bool,
}

/// Where a change to a buffer's file came from.
#[derive(Clone, Copy, Debug)]
pub(super) enum Source {
    /// What the chat's tools said they changed, as its turn ends if
    /// `last`.
    Tools { last: bool },
    /// The chat's commands, in turn `turn`.
    Commands { turn: crate::chat::TurnUid },
}

impl Diffing {
    pub(super) fn chat(&self) -> TerminalId {
        self.chat
    }
}

/// A change given the worker.
#[derive(Debug)]
pub(super) enum Job {
    /// To a file not open: the edit to work out, for each of the turns
    /// that made it, in order, one at least.
    Closed(Vec<Settled>),
    /// To a file open in a buffer: to check against the file, and merge
    /// into the buffer.
    Open(Checking),
    /// To a big file not open: to read, for the loop to check.
    Read(Reading),
}

/// A change the worker has done with: worked out, or `None` if it
/// couldn't be; or read.
#[derive(Debug)]
pub(super) enum Worked {
    Closed(Vec<(Settled, Option<Diffed>)>),
    Open(Checking, Option<Checked>),
    Read(Reading, std::io::Result<(String, DiskStamp)>),
}

impl Job {
    /// The text it holds, or will, in bytes.
    fn bytes(&self) -> usize {
        match self {
            Self::Closed(turns) => bytes(turns),
            Self::Open(checking) => checking.bytes(),
            Self::Read(reading) => reading.size,
        }
    }

    /// Works it out, as the worker does, having `first` run first, in the
    /// same guard against a panic.
    fn work(self, first: &dyn Fn()) -> Worked {
        match self {
            // Each turn's on its own: one that fails is noted unknown, and
            // the rest are still known.
            Self::Closed(turns) => Worked::Closed(
                (turns.into_iter())
                    .map(|settled| {
                        let diffed = catch_unwind(AssertUnwindSafe(|| {
                            first();
                            let (before, disk) = settled.texts();
                            Diffed::between(before, disk)
                        }));
                        if let Err(panic) = &diffed {
                            log::warn!("settling: working out a change panicked: {panic:?}");
                        }
                        (settled, diffed.ok())
                    })
                    .collect(),
            ),
            Self::Open(checking) => {
                let checked = catch_unwind(AssertUnwindSafe(|| {
                    first();
                    checking.check()
                }));
                if let Err(panic) = &checked {
                    log::warn!("settling: merging a change panicked: {panic:?}");
                }
                Worked::Open(checking, checked.ok())
            }
            // A test's panic is for what's worked out, not for reading.
            Self::Read(mut reading) => {
                let read = catch_unwind(AssertUnwindSafe(|| reading.read()));
                let read = read.unwrap_or_else(|panic| {
                    log::warn!("settling: reading a file panicked: {panic:?}");
                    Err(std::io::Error::other("reading it failed"))
                });
                Worked::Read(reading, read)
            }
        }
    }
}

impl Worked {
    /// The text the change given the worker held, in bytes.
    fn bytes(&self) -> usize {
        match self {
            Self::Closed(turns) => {
                let turns: Vec<&Settled> = turns.iter().map(|(settled, _)| settled).collect();
                bytes(&turns)
            }
            Self::Open(checking, _) => checking.bytes(),
            Self::Read(reading, _) => reading.size,
        }
    }
}

/// Where the worker is given changes, and gives them back.
type Worker = (mpsc::Sender<Job>, mpsc::Receiver<Worked>);

/// The worker, started when first needed.
#[derive(Debug, Default)]
pub(super) struct Settler {
    worker: Option<Worker>,
    /// The bytes of text given the worker and not yet given back.
    in_flight: usize,
    /// The worker was found gone as it was given a change: what it had
    /// is to be failed (see [`lost`](Self::lost)).
    gone: bool,
    /// Makes the worker panic as it works out the next change, if set, as
    /// a test's way of seeing what a panic does: [`CAUGHT`] in its guard,
    /// [`LATER`] in the guard after, or [`GONE`], outside it, so the
    /// worker is lost.
    #[cfg(test)]
    pub(super) panic: Arc<std::sync::atomic::AtomicU8>,
    /// What the worker waits on before each change it's given, as a test
    /// says: its way of having it read a file late.
    #[cfg(test)]
    pub(super) gate: Arc<Gate>,
}

/// Lets the worker take on its changes freely, or, while a test holds it,
/// only as many as it lets through.
#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct Gate {
    /// How many more it may take on, while held.
    left: std::sync::Mutex<Option<usize>>,
    changed: std::sync::Condvar,
}

#[cfg(test)]
impl Gate {
    /// Lets nothing more through.
    pub(super) fn hold(&self) {
        *self.left.lock().unwrap() = Some(0);
    }

    /// Lets `count` more changes through, while held.
    pub(super) fn let_through(&self, count: usize) {
        if let Some(left) = self.left.lock().unwrap().as_mut() {
            *left += count;
        }
        self.changed.notify_all();
    }

    /// Lets everything through.
    pub(super) fn open(&self) {
        *self.left.lock().unwrap() = None;
        self.changed.notify_all();
    }

    /// Waits until the next change may be taken on.
    fn pass(&self) {
        let mut left = self
            .left
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match left.as_mut() {
                None => return,
                Some(0) => {
                    left = (self.changed.wait(left))
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                Some(count) => {
                    *count -= 1;
                    return;
                }
            }
        }
    }
}

/// What the worker has given back since last asked.
#[derive(Debug, Default)]
pub(super) struct Taken {
    /// The changes it's done with, in the order they were given it.
    pub(super) worked: Vec<Worked>,
    /// It's gone, with whatever else it was given.
    pub(super) lost: bool,
}

#[cfg(test)]
pub(super) const CAUGHT: u8 = 1;
#[cfg(test)]
pub(super) const GONE: u8 = 2;
#[cfg(test)]
pub(super) const LATER: u8 = 3;

/// Panics as `panic` asks of the change being worked out, as
/// [`Settler::panic`] says.
#[cfg(test)]
fn panic_as_asked(panic: &std::sync::atomic::AtomicU8) {
    match panic.swap(0, std::sync::atomic::Ordering::SeqCst) {
        CAUGHT => panic!("working out a change failed, as a test asked"),
        LATER => panic.store(CAUGHT, std::sync::atomic::Ordering::SeqCst),
        _ => {}
    }
}

/// The text `turns`, each turn's change to a file in order, hold, in
/// bytes: each text once, as each turn's goes from the text the one
/// before left, which they share.
pub(super) fn bytes(turns: &[impl std::borrow::Borrow<Settled>]) -> usize {
    let len = |text: &Option<Arc<String>>| text.as_ref().map_or(0, |text| text.len());
    let before = turns.first().map_or(0, |first| len(&first.borrow().before));
    before
        + turns
            .iter()
            .map(|settled| len(&settled.borrow().disk))
            .sum::<usize>()
}

impl Settler {
    /// Works out `job` on the worker, waking the loop with `waker` once
    /// it's done. Gives `job` back if the worker can't take it, as when
    /// too much is in flight already, for the loop to work it out itself;
    /// never for its size alone, with nothing else in flight, as the loop
    /// would then take the biggest changes.
    pub(super) fn submit(&mut self, job: Job, waker: &Arc<mio::Waker>) -> Option<Job> {
        let size = job.bytes();
        if self.in_flight > 0 && self.in_flight + size > IN_FLIGHT {
            return Some(job);
        }
        if self.worker.is_none() {
            let (send, jobs) = mpsc::channel::<Job>();
            let (done, receive) = mpsc::channel();
            let waker = Arc::clone(waker);
            #[cfg(test)]
            let panic = Arc::clone(&self.panic);
            #[cfg(test)]
            let gate = Arc::clone(&self.gate);
            let spawned = std::thread::Builder::new()
                .name("settling".into())
                .spawn(move || {
                    while let Ok(job) = jobs.recv() {
                        #[cfg(test)]
                        gate.pass();
                        #[cfg(test)]
                        if panic.load(std::sync::atomic::Ordering::SeqCst) == GONE {
                            panic.store(0, std::sync::atomic::Ordering::SeqCst);
                            panic!("the settling worker is lost, as a test asked");
                        }
                        let worked = job.work(&|| {
                            #[cfg(test)]
                            panic_as_asked(&panic);
                        });
                        if done.send(worked).is_err() {
                            break;
                        }
                        let _ = waker.wake();
                    }
                });
            if spawned.is_err() {
                return Some(job);
            }
            self.worker = Some((send, receive));
        }
        let (send, _) = self.worker.as_ref()?;
        match send.send(job) {
            Ok(()) => {
                self.in_flight += size;
                None
            }
            Err(mpsc::SendError(job)) => {
                self.worker = None;
                self.in_flight = 0;
                self.gone = true;
                Some(job)
            }
        }
    }

    /// Whether the worker was found gone as it was given a change, since
    /// last asked: what it had is to be failed.
    pub(super) fn lost(&mut self) -> bool {
        std::mem::take(&mut self.gone)
    }

    /// What the worker has done with since last asked, without waiting.
    pub(super) fn take(&mut self) -> Taken {
        let mut taken = Taken::default();
        let Some((_, receive)) = &self.worker else {
            return taken;
        };
        loop {
            match receive.try_recv() {
                Ok(worked) => taken.worked.push(worked),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    taken.lost = true;
                    break;
                }
            }
        }
        self.gave_back(&taken);
        taken
    }

    /// The next change the worker is done with, waiting for it: `None` if
    /// the worker's gone, with whatever it had.
    pub(super) fn wait(&mut self) -> Option<Worked> {
        let (_, receive) = self.worker.as_ref()?;
        let worked = receive.recv().ok();
        let taken = match worked {
            Some(worked) => Taken {
                worked: vec![worked],
                lost: false,
            },
            None => Taken {
                worked: Vec::new(),
                lost: true,
            },
        };
        self.gave_back(&taken);
        taken.worked.into_iter().next()
    }

    /// Counts what `taken` gave back as no longer in flight, and lets go
    /// of a worker that's gone.
    fn gave_back(&mut self, taken: &Taken) {
        for worked in &taken.worked {
            self.in_flight = self.in_flight.saturating_sub(worked.bytes());
        }
        if taken.lost {
            self.worker = None;
            self.in_flight = 0;
        }
    }
}

/// Changes to big files open in buffers, merged off the loop, and what
/// may happen to the buffer meanwhile (CLACKA-201).
#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Author, BufferId, TerminalId};

    use serde_json::json;

    use crate::chat::{RawDiff, Wrote};
    use crate::server::agent_files::tests::start_tool;
    use crate::server::disk::tests::{finish_command, says, start_command};
    use crate::server::turn_undo::tests::{World, big_file, change_big_file};

    /// `big.txt`, open, with a change to its first line by the chat's
    /// agent being merged into it off the loop. Returns the buffer, and
    /// the file's text before the change and after.
    fn merging(world: &mut World) -> (BufferId, String, String) {
        let first = big_file(world);
        let id = world.open("big.txt");
        let second = change_big_file(world, ("t1", 1), &first);
        assert_eq!(world.text(id), first, "not made yet");
        (id, first, second)
    }

    /// Writes `text` over `big.txt` as `sed -i`, git and most editors
    /// write a file: beside it, then renamed over it. What's being checked
    /// was opened before, and is read as it was.
    fn write_over(world: &World, text: &str) {
        write_over_named(world, "big.txt", text);
    }

    /// As [`write_over`], over the file `name`.
    fn write_over_named(world: &World, name: &str, text: &str) {
        let new = world.path(&format!("{name}.new"));
        std::fs::write(&new, text).unwrap();
        std::fs::rename(new, world.path(name)).unwrap();
    }

    /// The buffer's text saved.
    fn saved(world: &World, id: BufferId) -> String {
        world.daemon.buffers[&id].buffer.saved().to_string()
    }

    /// A change past the cap of what's in flight is still the worker's
    /// with nothing else in flight, as the loop would take the biggest
    /// changes; with something in flight, one past the cap is given back
    /// for the loop to work out, until that's done.
    #[test]
    fn a_change_past_the_cap_is_given_back_only_with_another_in_flight() {
        use std::sync::Arc;

        use super::{IN_FLIGHT, Job, Settler, Worked};
        use crate::server::agent_files::tests::reading_of;

        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(mio::Waker::new(poll.registry(), mio::Token(0)).unwrap());
        let mut settler = Settler::default();
        settler.gate.hold();
        let past = settler.submit(Job::Read(reading_of(IN_FLIGHT + 1)), &waker);
        assert!(past.is_none(), "taken, with nothing else in flight");
        let small = settler.submit(Job::Read(reading_of(1)), &waker);
        assert!(matches!(small, Some(Job::Read(_))), "given back");
        settler.gate.open();
        assert!(matches!(settler.wait(), Some(Worked::Read(_, Err(_)))));
        assert!(settler.submit(Job::Read(reading_of(1)), &waker).is_none());
    }

    #[test]
    fn a_big_change_to_an_open_file_is_merged_off_the_loop_and_undone_by_turn() {
        let mut world = World::new();
        let (id, first, second) = merging(&mut world);
        world.daemon.settle_all_pending();
        assert!(!world.daemon.agent_files.diffing(world.chat));
        assert_eq!(
            (world.text(id), saved(&world, id)),
            (second.clone(), second)
        );
        assert!(!world.daemon.buffers[&id].agents.is_empty(), "reviewed");
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), first);
    }

    #[test]
    fn typing_while_a_big_change_is_merged_has_it_merged_again() {
        let mut world = World::new();
        let (id, first, second) = merging(&mut world);
        world.type_in(id, first.len(), "", "typed\n");
        world.daemon.settle_all_pending();
        assert_eq!(world.text(id), format!("{second}typed\n"));
        assert_eq!(saved(&world, id), second);
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), format!("{first}typed\n"));
    }

    #[test]
    fn typing_on_the_lines_a_big_change_touches_meanwhile_refuses_it() {
        let mut world = World::new();
        let (id, first, _) = merging(&mut world);
        world.type_in(id, 0, "line 1\n", "LINE 1\n");
        world.daemon.settle_all_pending();
        // Never mixed in: yours is as you left it, and still refuses to be
        // written over the agent's file unless forced.
        assert_eq!(world.text(id), first.replacen("line 1\n", "LINE 1\n", 1));
        let notice = world.chat_node().notice.unwrap_or_default();
        assert!(notice.contains("where you have unsaved edits"), "{notice}");
        let buffer = &world.daemon.buffers[&id].buffer;
        assert!(buffer.changed_on_disk().unwrap().is_some());
        // Saved only if forced, over the agent's file.
        assert!(world.daemon.save_buffer(id, false).is_err());
        world.daemon.save_buffer(id, true).unwrap();
        assert_eq!(world.disk("big.txt"), Some(world.text(id)));
    }

    #[test]
    fn typing_on_while_a_big_change_is_merged_again_has_it_merged_on_the_loop_at_last() {
        let mut world = World::new();
        let (id, first, second) = merging(&mut world);
        let mut typed = String::new();
        for (key, pending) in [("a\n", true), ("b\n", true), ("c\n", false)] {
            world.type_in(id, first.len() + typed.len(), "", key);
            typed.push_str(key);
            world.daemon.take_one_settled();
            assert_eq!(
                world.daemon.agent_files.diffing(world.chat),
                pending,
                "{key}"
            );
        }
        assert_eq!(world.text(id), format!("{second}{typed}"));
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), format!("{first}{typed}"));
    }

    #[test]
    fn your_own_undo_after_a_big_change_merged_again_leaves_it_be() {
        let mut world = World::new();
        let (id, first, second) = merging(&mut world);
        world.type_in(id, first.len(), "", "typed\n");
        world.daemon.settle_all_pending();
        assert_eq!(world.text(id), format!("{second}typed\n"));
        let buffer = &mut world.daemon.buffers.get_mut(&id).unwrap().buffer;
        let made = buffer.reverse(Author::User, false).unwrap().unwrap();
        world.daemon.made_for_user(id, made);
        assert_eq!(world.text(id), second);
        let buffer = &mut world.daemon.buffers.get_mut(&id).unwrap().buffer;
        assert!(buffer.reverse(Author::User, false).unwrap().is_none());
    }

    #[test]
    fn what_the_agents_command_changes_while_a_big_change_is_merged_is_its_too() {
        let mut world = World::new();
        let (id, first, second) = merging(&mut world);
        let chat = world.chat;
        let done = json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
        });
        says(&mut world.daemon, chat, done);
        // A command, as `sed -i`, changes it too, before the change is
        // made.
        start_command(&mut world.daemon, chat);
        let third = second.replacen("line 5\n", "line #5\n", 1);
        write_over(&world, &third);
        world.daemon.disk_maybe_changed(id);
        world.daemon.check_disk();
        finish_command(&mut world.daemon, chat);
        // The loop looks at what's changed on every pass, the command's
        // finish forgotten by the next.
        world.daemon.check_disk();
        world.daemon.check_disk();
        world.daemon.settle_all_pending();
        // Heard of again once the tool's change is made, as when the
        // file's directory is watched again, having been without inotify:
        // still the command's.
        world.daemon.disk_maybe_changed(id);
        world.daemon.check_disk();
        world.daemon.settle_all_pending();
        assert!(!world.daemon.buffers[&id].stale());
        assert_eq!(world.text(id), third);
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), first);
    }

    /// Another chat, numbered `id`, on session `s{id}`, in the same
    /// directory, its agent answering its first message.
    fn other_chat(world: &mut World, id: u32) -> clackatoo_protocol::TerminalId {
        use clackatoo_protocol::ChatAction;

        use crate::agent::Opened;
        use crate::chat::HostedChat;

        let agent = crate::AgentConfig::default().find("claude").unwrap();
        let size = clackatoo_protocol::Size::new(40, 10).unwrap();
        let cwd = world.root.display().to_string();
        let tools = clackatoo_protocol::AgentTools::Ask;
        let mut chat = HostedChat::new(TerminalId(id), size, cwd, &agent, tools, true, false);
        chat.connected(Opened {
            session: format!("s{id}"),
            settings: None,
        });
        chat.act(ChatAction::Send("go".into()));
        world.daemon.add_chat(chat);
        TerminalId(id)
    }

    /// `big.txt` being merged, and changed meanwhile by a command of the
    /// chat's agent, which has finished, then by `other`, which writes
    /// line 9. Returns the buffer, and the file's text with the change
    /// being merged.
    fn contested(world: &mut World, other: impl FnOnce(&mut World)) -> (BufferId, String) {
        let (id, _, second) = merging(world);
        let chat = world.chat;
        let done = json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
        });
        says(&mut world.daemon, chat, done);
        start_command(&mut world.daemon, chat);
        let third = second.replacen("line 5\n", "line #5\n", 1);
        write_over(world, &third);
        world.daemon.disk_maybe_changed(id);
        world.daemon.check_disk();
        finish_command(&mut world.daemon, chat);
        world.daemon.check_disk();
        world.daemon.check_disk();
        other(world);
        write_over(world, &third.replacen("line 9\n", "line #9\n", 1));
        world.daemon.disk_maybe_changed(id);
        world.daemon.check_disk();
        (id, second)
    }

    #[test]
    fn the_agents_own_write_heard_again_while_a_big_change_is_merged_is_still_its() {
        let mut world = World::new();
        let (id, _, second) = merging(&mut world);
        // Heard of while its tool runs, then again once it's long done, as
        // inotify says one write more than once.
        world.daemon.disk_maybe_changed(id);
        world.daemon.check_disk();
        let done = json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
        });
        says(&mut world.daemon, world.chat, done);
        world.daemon.check_disk();
        world.daemon.check_disk();
        world.daemon.disk_maybe_changed(id);
        world.daemon.check_disk();
        world.daemon.settle_all_pending();
        world.daemon.check_disk();
        assert_eq!(world.text(id), second);
        assert!(!world.daemon.buffers[&id].stale());
    }

    #[test]
    fn another_chats_change_while_a_big_change_is_merged_is_from_outside() {
        let mut world = World::new();
        let (id, second) = contested(&mut world, |world| {
            let other = other_chat(world, 2);
            start_command(&mut world.daemon, other);
        });
        assert!(!world.daemon.buffers[&id].stale());
        world.daemon.settle_all_pending();
        world.daemon.check_disk();
        world.daemon.settle_all_pending();
        // Neither chat's: the agent's tool's change is made as its own,
        // and what's on disk besides is from outside.
        assert_eq!(world.text(id), second);
        assert!(world.daemon.buffers[&id].stale());
        assert!(
            world
                .daemon
                .agent_files
                .log
                .turn(
                    TerminalId(2),
                    crate::server::agent_files::tests::first_uid(&world.daemon, TerminalId(2))
                )
                .is_none()
        );
    }

    #[test]
    fn a_change_from_outside_after_a_command_while_a_big_change_is_merged_isnt_the_agents() {
        let mut world = World::new();
        let (id, second) = contested(&mut world, |_| {});
        world.daemon.settle_all_pending();
        world.daemon.check_disk();
        world.daemon.settle_all_pending();
        assert_eq!(world.text(id), second);
        assert!(world.daemon.buffers[&id].stale());
    }

    #[test]
    fn a_file_changed_from_outside_while_a_big_change_is_merged_says_so_once_made() {
        let mut world = World::new();
        let (id, _, second) = merging(&mut world);
        world.end_turn();
        let third = second.replacen("line 7\n", "line #7\n", 1);
        write_over(&world, &third);
        world.daemon.disk_maybe_changed(id);
        world.daemon.check_disk();
        assert!(!world.daemon.buffers[&id].stale());
        world.daemon.settle_all_pending();
        world.daemon.check_disk();
        // The agent's change is made as its own; the other isn't taken.
        assert_eq!(world.text(id), second);
        assert!(world.daemon.buffers[&id].stale());
        assert!(
            world
                .daemon
                .agent_files
                .log
                .turn(world.chat, world.first())
                .is_some()
        );
    }

    #[test]
    fn undoing_while_a_big_change_is_merged_has_it_merged_again() {
        let mut world = World::new();
        let first = big_file(&world);
        let id = world.open("big.txt");
        world.type_in(id, first.len(), "", "typed\n");
        let second = change_big_file(&mut world, ("t1", 1), &first);
        let buffer = &mut world.daemon.buffers.get_mut(&id).unwrap().buffer;
        let made = buffer.reverse(Author::User, false).unwrap().unwrap();
        world.daemon.made_for_user(id, made);
        world.daemon.settle_all_pending();
        assert_eq!(world.text(id), second);
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), first);
    }

    #[test]
    fn a_second_change_while_a_big_one_is_merged_waits_for_it() {
        let mut world = World::new();
        let (id, first, second) = merging(&mut world);
        // By another of the chat's tools, then through the daemon.
        let third = change_big_file(&mut world, ("t2", 2), &second);
        world.daemon.settle_all_pending();
        assert_eq!(world.text(id), third);
        world.write("big.txt", "small\n");
        assert_eq!(world.text(id), "small\n");
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), first);
    }

    #[test]
    fn a_write_through_the_daemon_takes_in_a_big_change_being_merged_first() {
        let mut world = World::new();
        let (id, first, _) = merging(&mut world);
        world.write("big.txt", "small\n");
        assert!(!world.daemon.agent_files.diffing(world.chat));
        assert_eq!(world.text(id), "small\n");
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), first);
    }

    #[test]
    fn a_buffer_closed_while_a_big_change_is_merged_has_it_as_a_file_not_open() {
        let mut world = World::new();
        let (id, first, second) = merging(&mut world);
        world.daemon.close_if_unwanted(id);
        assert!(!world.daemon.buffers.contains_key(&id));
        world.daemon.settle_all_pending();
        assert!(world.daemon.reviews.keeps_closed(&world.path("big.txt")));
        assert_eq!(world.disk("big.txt"), Some(second));
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.disk("big.txt"), Some(first));
    }

    #[test]
    fn a_file_opened_again_while_a_big_change_is_merged_takes_it_in_first() {
        let mut world = World::new();
        let (id, first, second) = merging(&mut world);
        world.daemon.close_if_unwanted(id);
        let again = world.open("big.txt");
        assert_ne!(again, id);
        assert!(!world.daemon.agent_files.diffing(world.chat));
        assert_eq!(world.text(again), second);
        assert!(
            !world.daemon.buffers[&again].agents.is_empty(),
            "its hunk came along"
        );
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(again), first);
    }

    #[test]
    fn a_buffer_whose_big_change_is_merged_isnt_taken_as_changed_from_outside() {
        let mut world = World::new();
        let (id, first, second) = merging(&mut world);
        // With no tool running, a change found on disk would be from
        // outside, and a window would read it again.
        world.end_turn();
        world.daemon.disk_maybe_changed(id);
        world.daemon.check_disk();
        assert!(!world.daemon.buffers[&id].stale());
        assert!(world.daemon.reload_buffer(id).is_ok());
        assert_eq!(world.text(id), first, "nothing taken yet");
        world.daemon.settle_all_pending();
        assert_eq!(world.text(id), second);
        world.daemon.check_disk();
        assert!(!world.daemon.buffers[&id].stale());
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), first);
    }

    #[test]
    fn saving_while_a_big_change_is_merged_takes_it_in_first() {
        let mut world = World::new();
        let (id, _, second) = merging(&mut world);
        world.daemon.save_buffer(id, false).unwrap();
        assert!(!world.daemon.agent_files.diffing(world.chat));
        assert_eq!(world.text(id), second);
        assert_eq!(world.disk("big.txt"), Some(second));
    }

    #[test]
    fn a_handover_takes_in_a_big_change_being_merged_first() {
        let mut world = World::new();
        let (id, _, second) = merging(&mut world);
        let uid = world.first();
        drop(world.daemon.hand_over(std::time::Instant::now()).unwrap());
        assert!(!world.daemon.agent_files.diffing(world.chat));
        assert_eq!(world.text(id), second);
        assert!(world.daemon.agent_files.log.turn(world.chat, uid).is_some());
    }

    #[test]
    fn a_big_change_the_worker_fails_to_merge_is_merged_on_the_loop() {
        use super::{CAUGHT, GONE};
        let mut world = World::new();
        let first = big_file(&world);
        let id = world.open("big.txt");
        world.daemon.agent_files.panic_next(CAUGHT);
        let second = change_big_file(&mut world, ("t1", 1), &first);
        world.daemon.settle_all_pending();
        assert_eq!(world.text(id), second);
        // The worker itself goes.
        world.daemon.agent_files.panic_next(GONE);
        let third = change_big_file(&mut world, ("t2", 2), &second);
        world.daemon.settle_all_pending();
        assert_eq!(world.text(id), third);
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), first);
    }

    #[test]
    fn a_big_change_given_a_worker_lost_with_its_buffer_closed_is_still_taken_in() {
        use super::GONE;
        let mut world = World::new();
        let first = big_file(&world);
        let id = world.open("big.txt");
        world.daemon.agent_files.panic_next(GONE);
        let second = change_big_file(&mut world, ("t1", 1), &first);
        world.daemon.close_if_unwanted(id);
        world.daemon.settle_all_pending();
        assert!(!world.daemon.agent_files.diffing(world.chat));
        assert!(world.daemon.reviews.keeps_closed(&world.path("big.txt")));
        assert_eq!(world.disk("big.txt"), Some(second));
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.disk("big.txt"), Some(first));
    }

    /// A change heard of while an agent's change to the file is checked
    /// off the loop, that no chat made, isn't lost if the agent's isn't
    /// made, as when its tool's changes don't make the file: it's taken
    /// as from outside once the check is done.
    #[test]
    fn a_change_from_outside_while_a_big_change_is_checked_is_seen_if_it_isnt_made() {
        let mut world = World::new();
        let first = big_file(&world);
        let id = world.open("big.txt");
        let chat = world.chat;
        let path = world.path("big.txt").display().to_string();
        start_tool(&mut world.daemon, chat, "t1");
        // The tool says it changed line 2, but changed line 1.
        write_over(&world, &first.replacen("line 1\n", "line #1\n", 1));
        let said = Wrote {
            done: true,
            turn: world.first(),
            tool: "t1".into(),
            order: 0,
            paths: vec![path.clone()],
            diffs: vec![RawDiff {
                path,
                old: Some("line 2\n".into()),
                new: "line #2\n".into(),
            }],
        };
        world.daemon.take_written(chat, &said);
        assert!(
            world.daemon.agent_files.diffing(chat),
            "checked off the loop"
        );
        let done = json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
        });
        says(&mut world.daemon, chat, done);
        world.daemon.check_disk();
        // Something else changes it, while no tool runs.
        write_over(&world, &first.replacen("line 9\n", "line #9\n", 1));
        world.daemon.disk_maybe_changed(id);
        world.daemon.check_disk();
        assert!(!world.daemon.buffers[&id].stale(), "not yet");
        world.daemon.settle_all_pending();
        world.daemon.check_disk();
        assert_eq!(world.text(id), first);
        assert!(world.daemon.buffers[&id].stale());
    }

    /// A small file open, which the agent's tool makes big, is read and
    /// checked off the loop: what counts is the file as it is now.
    #[test]
    fn a_small_file_made_big_is_checked_off_the_loop() {
        let mut world = World::new();
        std::fs::write(world.path("small.txt"), "small\n").unwrap();
        let id = world.open("small.txt");
        let chat = world.chat;
        let path = world.path("small.txt").display().to_string();
        start_tool(&mut world.daemon, chat, "t1");
        let mut big = String::new();
        for n in 1..=100_000 {
            use std::fmt::Write as _;
            writeln!(big, "line {n}").unwrap();
        }
        write_over_named(&world, "small.txt", &big);
        let said = Wrote {
            done: true,
            turn: world.first(),
            tool: "t1".into(),
            order: 0,
            paths: vec![path.clone()],
            diffs: vec![RawDiff {
                path,
                old: Some("small\n".into()),
                new: big.as_str().into(),
            }],
        };
        world.daemon.take_written(chat, &said);
        assert!(
            world.daemon.agent_files.diffing(chat),
            "checked off the loop"
        );
        world.daemon.settle_all_pending();
        assert_eq!(world.text(id), big);
    }

    #[test]
    fn a_big_change_by_the_agents_commands_is_merged_off_the_loop() {
        let mut world = World::new();
        let first = big_file(&world);
        let id = world.open("big.txt");
        let second = first.replacen("line 1\n", "line #1\n", 1);
        std::fs::write(world.path("big.txt"), &second).unwrap();
        let uid = world.first();
        world.daemon.take_from_commands(world.chat, uid, id);
        assert!(world.daemon.agent_files.diffing(world.chat));
        world.daemon.settle_all_pending();
        assert_eq!(world.text(id), second);
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), first);
    }
}
