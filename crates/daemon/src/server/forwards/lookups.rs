//! Looking up the names forwarded connections ask for, off the loop.
//!
//! getaddrinfo blocks, and can block for a long time: a resolver that
//! doesn't answer is waited for, and tried again, for many seconds. So
//! names are looked up by a few threads, at most [`MOST_THREADS`], started
//! as names are asked for and ending once they've had nothing to do for
//! [`IDLE`], so nothing runs while nothing is forwarded. One name that
//! hangs takes one thread, and the others go on with the rest: a name is
//! looked up once at a time, however often it's asked for meanwhile, as a
//! browser trying again asks again. Each answer wakes the loop, which
//! takes it with [`Lookups::answers`].

use std::collections::{HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::Duration;

use mio::Waker;

/// The most threads looking names up at once.
const MOST_THREADS: usize = 3;

/// How long a thread waits for another name before it ends.
const IDLE: Duration = Duration::from_secs(10);

/// What looks a name up: the system's resolver, but in tests.
type Resolve = fn(&str) -> Vec<IpAddr>;

/// The threads looking names up, and their answers.
#[derive(Debug)]
pub(super) struct Lookups {
    shared: Arc<Shared>,
    found: mpsc::Sender<(String, Vec<IpAddr>)>,
    answers: mpsc::Receiver<(String, Vec<IpAddr>)>,
    waker: Arc<Waker>,
    resolve: Resolve,
    idle: Duration,
    /// The names asked for and not yet answered.
    in_flight: HashSet<String>,
}

/// What the threads share with the loop.
#[derive(Debug, Default)]
struct Shared {
    queue: Mutex<Queue>,
    asked: Condvar,
}

/// The names waiting to be looked up, and the threads.
#[derive(Debug, Default)]
struct Queue {
    names: VecDeque<String>,
    /// How many threads there are.
    threads: usize,
    /// How many of them are waiting for a name.
    idle: usize,
}

impl Lookups {
    /// Lookups whose answers wake the loop with `waker`. No thread starts
    /// until a name is asked for.
    pub(super) fn new(waker: Arc<Waker>) -> Self {
        Self::with(waker, system, IDLE)
    }

    fn with(waker: Arc<Waker>, resolve: Resolve, idle: Duration) -> Self {
        let (found, answers) = mpsc::channel();
        Self {
            shared: Arc::default(),
            found,
            answers,
            waker,
            resolve,
            idle,
            in_flight: HashSet::new(),
        }
    }

    /// Asks for `host` to be looked up, unless it's being looked up
    /// already, starting a thread for it if none is free and there's room
    /// for another.
    pub(super) fn ask(&mut self, host: String) {
        if !self.in_flight.insert(host.clone()) {
            return;
        }
        let mut queue = (self.shared.queue.lock()).unwrap_or_else(PoisonError::into_inner);
        queue.names.push_back(host);
        if queue.idle == 0 && queue.threads < MOST_THREADS && self.spawn() {
            queue.threads += 1;
        }
        drop(queue);
        self.shared.asked.notify_one();
    }

    /// Starts a thread, returning whether it started.
    fn spawn(&self) -> bool {
        let shared = Arc::clone(&self.shared);
        let found = self.found.clone();
        let waker = Arc::clone(&self.waker);
        let (resolve, idle) = (self.resolve, self.idle);
        let started = std::thread::Builder::new()
            .name("forward lookups".into())
            .spawn(move || {
                while let Some(host) = next(&shared, idle) {
                    let ips = resolve(&host);
                    if found.send((host, ips)).is_err() {
                        break;
                    }
                    let _ = waker.wake();
                }
            });
        if let Err(err) = &started {
            log::warn!("no thread to look names up: {err}");
        }
        started.is_ok()
    }

    /// The names looked up since last asked, and their addresses: none if
    /// it wasn't found.
    pub(super) fn answers(&mut self) -> Vec<(String, Vec<IpAddr>)> {
        let answers: Vec<_> = self.answers.try_iter().collect();
        for (host, _) in &answers {
            self.in_flight.remove(host);
        }
        answers
    }

    /// Whether any name is being looked up.
    pub(super) fn busy(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// How many threads there are, for tests.
    #[cfg(test)]
    fn threads(&self) -> usize {
        (self.shared.queue.lock())
            .unwrap_or_else(PoisonError::into_inner)
            .threads
    }
}

/// The next name for a thread to look up, or `None` once it has waited
/// `idle` for one, and is to end.
fn next(shared: &Shared, idle: Duration) -> Option<String> {
    let mut queue = (shared.queue.lock()).unwrap_or_else(PoisonError::into_inner);
    loop {
        if let Some(host) = queue.names.pop_front() {
            return Some(host);
        }
        queue.idle += 1;
        let (again, waited) =
            (shared.asked.wait_timeout(queue, idle)).unwrap_or_else(PoisonError::into_inner);
        queue = again;
        queue.idle -= 1;
        if waited.timed_out() && queue.names.is_empty() {
            queue.threads -= 1;
            return None;
        }
    }
}

/// The addresses the system's resolver finds for `host`.
fn system(host: &str) -> Vec<IpAddr> {
    (host, 0)
        .to_socket_addrs()
        .map(|addresses| addresses.map(|address| address.ip()).collect())
        .inspect_err(|err| log::info!("looking up {host} failed: {err}"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// Hangs on `hang`, for a few seconds, as a resolver that doesn't
    /// answer does, and finds every other name at 10.0.0.1.
    fn slow(host: &str) -> Vec<IpAddr> {
        if host == "hang" {
            std::thread::sleep(Duration::from_secs(3));
        }
        vec![IpAddr::from([10, 0, 0, 1])]
    }

    fn lookups(idle: Duration) -> (Lookups, mio::Poll) {
        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(Waker::new(poll.registry(), mio::Token(0)).unwrap());
        (Lookups::with(waker, slow, idle), poll)
    }

    /// The answers that come within `wait`.
    fn answered(lookups: &mut Lookups, count: usize, wait: Duration) -> Vec<String> {
        let until = Instant::now() + wait;
        let mut names = Vec::new();
        while names.len() < count && Instant::now() < until {
            names.extend(lookups.answers().into_iter().map(|(host, _)| host));
            std::thread::sleep(Duration::from_millis(5));
        }
        names
    }

    #[test]
    fn a_name_that_hangs_holds_up_no_other() {
        let (mut lookups, _poll) = lookups(IDLE);
        lookups.ask("hang".into());
        lookups.ask("a".into());
        lookups.ask("b".into());
        let mut names = answered(&mut lookups, 2, Duration::from_secs(2));
        names.sort();
        assert_eq!(names, ["a", "b"]);
        assert!(lookups.threads() <= MOST_THREADS);
    }

    #[test]
    fn a_name_that_hangs_asked_for_again_and_again_takes_one_thread() {
        let (mut lookups, _poll) = lookups(IDLE);
        // A browser trying again and again.
        for _ in 0..MOST_THREADS + 2 {
            lookups.ask("hang".into());
        }
        assert!(lookups.busy());
        lookups.ask("a".into());
        lookups.ask("b".into());
        let mut names = answered(&mut lookups, 2, Duration::from_secs(2));
        names.sort();
        assert_eq!(names, ["a", "b"], "the others waited behind it");
        assert!(lookups.busy(), "still looking it up");
        // Answered, it's asked for afresh next time.
        assert_eq!(answered(&mut lookups, 1, Duration::from_secs(5)), ["hang"]);
        assert!(!lookups.busy());
    }

    #[test]
    fn threads_end_once_theyve_nothing_to_do() {
        let (mut lookups, _poll) = lookups(Duration::from_millis(50));
        assert_eq!(lookups.threads(), 0, "none before a name is asked for");
        lookups.ask("a".into());
        assert_eq!(answered(&mut lookups, 1, Duration::from_secs(2)), ["a"]);
        let until = Instant::now() + Duration::from_secs(2);
        while lookups.threads() > 0 {
            assert!(Instant::now() < until, "the thread never ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        // And one starts again for the next.
        lookups.ask("b".into());
        assert_eq!(answered(&mut lookups, 1, Duration::from_secs(2)), ["b"]);
    }
}
