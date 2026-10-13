//! Lets packets go when their time comes, on a thread of its own.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::link::Direction;

/// Packets waiting for their time, and the thread that lets them go.
/// Dropping it drops what's still waiting, as a network that goes away
/// loses what was on it; [`finish`](Self::finish) lets it go first.
#[derive(Debug)]
pub(crate) struct Pipe {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
struct Shared {
    queue: Mutex<Queue>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct Queue {
    waiting: BinaryHeap<Waiting>,
    /// Counts packets in, so equal times keep their order.
    count: u64,
    closed: bool,
    /// The thread ends once nothing is waiting.
    finishing: bool,
}

#[derive(Debug)]
struct Waiting {
    at: Instant,
    number: u64,
    direction: Direction,
    bytes: Vec<u8>,
}

impl PartialEq for Waiting {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Waiting {}

impl PartialOrd for Waiting {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Waiting {
    /// The soonest is the greatest, for the heap to give it first.
    fn cmp(&self, other: &Self) -> Ordering {
        (other.at, other.number).cmp(&(self.at, self.number))
    }
}

impl Pipe {
    /// Starts a thread that hands each packet to `deliver` when its time
    /// comes.
    pub(crate) fn start(mut deliver: impl FnMut(Direction, &[u8]) + Send + 'static) -> Self {
        let shared = Arc::new(Shared::default());
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("netem".into())
                .spawn(move || {
                    let mut queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
                    loop {
                        if queue.closed || (queue.finishing && queue.waiting.is_empty()) {
                            return;
                        }
                        let now = Instant::now();
                        match queue.waiting.peek() {
                            Some(next) if next.at <= now => {
                                let next = queue.waiting.pop().expect("just peeked");
                                drop(queue);
                                deliver(next.direction, &next.bytes);
                                queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
                            }
                            Some(next) => {
                                let wait = next.at - now;
                                queue = (shared.changed.wait_timeout(queue, wait))
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .0;
                            }
                            None => {
                                queue = (shared.changed.wait(queue))
                                    .unwrap_or_else(PoisonError::into_inner);
                            }
                        }
                    }
                })
                .expect("threads can be started")
        };
        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// Lets `bytes` go `direction` at `at`.
    pub(crate) fn send(&self, at: Instant, direction: Direction, bytes: Vec<u8>) {
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue.count += 1;
        let number = queue.count;
        queue.waiting.push(Waiting {
            at,
            number,
            direction,
            bytes,
        });
        self.shared.changed.notify_one();
    }

    /// Lets go of everything still waiting, each at its time, then ends.
    pub(crate) fn finish(mut self) {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .finishing = true;
        self.shared.changed.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.shared.changed.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
