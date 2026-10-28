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
//! What's taken in is in order: while a file's change is being worked
//! out, what more the chat's tools do to it waits, and so does the end of
//! the chat's turn (see `Diffing`).

use std::path::PathBuf;
use std::sync::{Arc, mpsc};

use clackatoo_protocol::TerminalId;

use super::agent_files::Settled;
use super::edit_log::Diffed;

/// The most bytes of text, before and after together, worked out on the
/// loop. A change to a bigger file is worked out on the worker.
pub(super) const ON_LOOP: usize = 256 * 1024;

/// A change to a file not open being worked out off the loop, and what
/// waits for it.
#[derive(Debug)]
pub(super) struct Diffing {
    pub(super) chat: TerminalId,
    pub(super) path: PathBuf,
    /// The chat's tools said more of the file meanwhile: it's settled
    /// again once this is taken in.
    pub(super) again: bool,
    /// The settling again is the turn's last.
    pub(super) last: bool,
}

/// Where the worker is given changes, and gives them back worked out.
type Worker = (mpsc::Sender<Settled>, mpsc::Receiver<(Settled, Diffed)>);

/// The worker, started when first needed.
#[derive(Debug, Default)]
pub(super) struct Settler {
    worker: Option<Worker>,
}

impl Settler {
    /// Works out `settled`'s change on the worker, waking the loop with
    /// `waker` once it's done. Gives `settled` back if there's no worker
    /// to do it, for the loop to work it out itself.
    pub(super) fn submit(&mut self, settled: Settled, waker: &Arc<mio::Waker>) -> Option<Settled> {
        if self.worker.is_none() {
            let (send, jobs) = mpsc::channel::<Settled>();
            let (done, receive) = mpsc::channel();
            let waker = Arc::clone(waker);
            let spawned = std::thread::Builder::new()
                .name("settling".into())
                .spawn(move || {
                    while let Ok(settled) = jobs.recv() {
                        let diffed = Diffed::between(
                            settled.before.as_deref().unwrap_or_default(),
                            settled.disk.as_deref().unwrap_or_default(),
                        );
                        if done.send((settled, diffed)).is_err() {
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
            Ok(()) => None,
            Err(mpsc::SendError(settled)) => {
                self.worker = None;
                Some(settled)
            }
        }
    }

    /// The changes worked out since last asked, in the order they were
    /// submitted.
    pub(super) fn take(&self) -> Vec<(Settled, Diffed)> {
        self.worker
            .as_ref()
            .map_or_else(Vec::new, |(_, receive)| receive.try_iter().collect())
    }
}
