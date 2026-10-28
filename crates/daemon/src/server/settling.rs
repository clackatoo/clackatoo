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
//! What's taken in is in order. While a file's change is being worked out
//! (see [`Diffing`]), what more the same chat's tools do to it waits, and
//! so does the end of the chat's turn. Anything else about the file, such
//! as another chat's write, opening it, or undoing or reviewing a turn,
//! first waits on the loop for the change to be worked out and takes it
//! in: a rare stall, as long as the worker's work, that keeps the edit
//! log and the review in the order the changes were made.
//!
//! A change the worker fails to work out, as if it panicked, is noted as
//! unknown, as one whose text before wasn't seen; a worker that's gone
//! fails every change it had, and the next is given a new one.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use clackatoo_protocol::TerminalId;

use super::agent_files::Settled;
use super::edit_log::{Diffed, Maker};

/// The most bytes of text, before and after together, worked out on the
/// loop. A change to a bigger file is worked out on the worker.
pub(super) const ON_LOOP: usize = 256 * 1024;

/// The most bytes of text given the worker at once, before and after
/// together. Past it, a change is worked out on the loop, rather than
/// holding more in memory.
const IN_FLIGHT: usize = 256 * 1024 * 1024;

/// A change to a file not open being worked out off the loop, and what
/// waits for it.
#[derive(Debug)]
pub(super) struct Diffing {
    pub(super) path: PathBuf,
    /// Who made it, to note it unknown if it can't be worked out.
    pub(super) maker: Maker,
    /// The chat's tools said more of the file meanwhile: it's settled
    /// again once this is taken in.
    pub(super) again: bool,
    /// The settling again is the turn's last.
    pub(super) last: bool,
}

impl Diffing {
    pub(super) fn chat(&self) -> TerminalId {
        self.maker.chat
    }
}

/// A change the worker has done with: worked out, or `None` if it
/// couldn't be.
pub(super) type Worked = (Settled, Option<Diffed>);

/// Where the worker is given changes, and gives them back.
type Worker = (mpsc::Sender<Settled>, mpsc::Receiver<Worked>);

/// The worker, started when first needed.
#[derive(Debug, Default)]
pub(super) struct Settler {
    worker: Option<Worker>,
    /// The bytes of text given the worker and not yet given back.
    in_flight: usize,
    /// Makes the worker panic as it works out the next change, if set, as
    /// a test's way of seeing what a panic does: [`CAUGHT`] in its guard,
    /// or [`GONE`], outside it, so the worker is lost.
    #[cfg(test)]
    pub(super) panic: Arc<std::sync::atomic::AtomicU8>,
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

/// The text `settled` holds, in bytes.
fn bytes(settled: &Settled) -> usize {
    settled.before.as_ref().map_or(0, String::len) + settled.disk.as_ref().map_or(0, String::len)
}

impl Settler {
    /// Works out `settled`'s change on the worker, waking the loop with
    /// `waker` once it's done. Gives `settled` back if the worker can't
    /// take it, as when too much is in flight already, for the loop to
    /// work it out itself.
    pub(super) fn submit(&mut self, settled: Settled, waker: &Arc<mio::Waker>) -> Option<Settled> {
        let size = bytes(&settled);
        if self.in_flight + size > IN_FLIGHT {
            return Some(settled);
        }
        if self.worker.is_none() {
            let (send, jobs) = mpsc::channel::<Settled>();
            let (done, receive) = mpsc::channel();
            let waker = Arc::clone(waker);
            #[cfg(test)]
            let panic = Arc::clone(&self.panic);
            let spawned = std::thread::Builder::new()
                .name("settling".into())
                .spawn(move || {
                    while let Ok(settled) = jobs.recv() {
                        #[cfg(test)]
                        if panic.load(std::sync::atomic::Ordering::SeqCst) == GONE {
                            panic.store(0, std::sync::atomic::Ordering::SeqCst);
                            panic!("the settling worker is lost, as a test asked");
                        }
                        let diffed = catch_unwind(AssertUnwindSafe(|| {
                            #[cfg(test)]
                            assert_ne!(
                                panic.swap(0, std::sync::atomic::Ordering::SeqCst),
                                CAUGHT,
                                "working out a change failed, as a test asked"
                            );
                            Diffed::between(
                                settled.before.as_deref().unwrap_or_default(),
                                settled.disk.as_deref().unwrap_or_default(),
                            )
                        }));
                        if let Err(panic) = &diffed {
                            log::warn!("settling: working out a change panicked: {panic:?}");
                        }
                        if done.send((settled, diffed.ok())).is_err() {
                            break;
                        }
                        let _ = waker.wake();
                    }
                });
            if spawned.is_err() {
                return Some(settled);
            }
            self.worker = Some((send, receive));
        }
        let (send, _) = self.worker.as_ref()?;
        match send.send(settled) {
            Ok(()) => {
                self.in_flight += size;
                None
            }
            Err(mpsc::SendError(settled)) => {
                self.worker = None;
                self.in_flight = 0;
                Some(settled)
            }
        }
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
        for (settled, _) in &taken.worked {
            self.in_flight = self.in_flight.saturating_sub(bytes(settled));
        }
        if taken.lost {
            self.worker = None;
            self.in_flight = 0;
        }
    }
}
