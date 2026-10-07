//! Which way a client's packets go to the daemon: through the carrier, or
//! straight over UDP, and when to try the other.
//!
//! The client's QUIC connection always believes the daemon is at one
//! address; this decides, underneath, how packets for it travel. The
//! daemon follows: QUIC moves its side of the connection to wherever the
//! newest packet came from. QUIC only lets a client move, so the client
//! can't follow a daemon to a new address itself.
//!
//! Once the handshake is done, the client tries UDP with probes: datagrams
//! that aren't QUIC, which the daemon sends straight back. Meanwhile the
//! connection carries on through the carrier, so nothing waits on them,
//! and the daemon never sees its packets arrive two ways at once. The
//! first probe back settles it, and every packet goes over UDP from then
//! on: the daemon moves once, when the first arrives. While on UDP, packets that go
//! unanswered for long enough send it back to the carrier, if the carrier
//! is still open, and it tries UDP again later. Once the carrier has
//! closed, as when the client's network changed under SSH, UDP is all
//! there is, if it has ever worked; if it hasn't, the connection is lost,
//! as it would be with no UDP at all.
//!
//! Whatever the way, packets that go unanswered for a second bring a ping,
//! and if that goes unanswered too, the daemon is out of contact until
//! something comes back, which the window shows.
//!
//! No I/O happens here: the driver says what happened, and does what it's
//! told.

use std::time::{Duration, Instant};

/// How long a try at UDP waits for a probe to come back.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How often a try at UDP sends a probe, in case one is lost.
const PROBE_INTERVAL: Duration = Duration::from_millis(500);

/// How long packets may go unanswered before a ping asks for an answer.
/// What's sent may not need one, such as an acknowledgement.
const SUSPECT_AFTER: Duration = Duration::from_secs(1);

/// How long packets may go unanswered, despite the ping, before the daemon
/// counts as out of contact.
const SILENT_AFTER: Duration = Duration::from_secs(3);

/// How long packets over UDP may go unanswered, despite the ping, before
/// the carrier takes over again.
const GIVE_UP_AFTER: Duration = Duration::from_secs(10);

/// How long after giving up on UDP it's tried again.
const RETRY_AFTER: Duration = Duration::from_mins(1);

/// How the client's packets are reaching the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Through SSH.
    Ssh,
    /// Straight over UDP.
    Udp,
}

/// Where packets go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sending {
    Carrier,
    Udp,
}

/// What the driver must do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Nothing,
    /// Send something that needs an answer, to find out whether the daemon
    /// is still there.
    Ping,
    /// Send a probe over UDP, to find out whether it gets through.
    Probe,
    /// Give up: there's no way left to reach the daemon.
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    /// Through the carrier, trying UDP again at `retry`, if ever.
    Carrier { retry: Option<Instant> },
    /// Through the carrier, if it's open, probing UDP since `since`, with
    /// the next probe at `next_probe`, until one comes back.
    Trying { since: Instant, next_probe: Instant },
    /// Over UDP.
    Udp,
}

/// Whether the daemon is answering what the client sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answers {
    /// Everything sent has been answered, as far as anyone knows.
    Answered,
    /// Nothing has come back since a packet went out at `since`.
    Unanswered { since: Instant },
    /// Nor since a ping asked for an answer.
    Pinged { since: Instant },
    /// Nor for long enough after: the daemon is out of contact.
    Silent { since: Instant },
}

impl Answers {
    /// When the first unanswered packet went out, if any did.
    fn since(self) -> Option<Instant> {
        match self {
            Self::Answered => None,
            Self::Unanswered { since } | Self::Pinged { since } | Self::Silent { since } => {
                Some(since)
            }
        }
    }
}

/// The way a client's packets go, and what happened to get there.
#[derive(Debug)]
pub(crate) struct Path {
    way: Way,
    /// Whether the daemon can be reached over UDP at all: it has a port,
    /// and the client knows its address.
    udp: bool,
    /// Whether anything has ever come back over UDP.
    udp_worked: bool,
    carrier_open: bool,
    answers: Answers,
}

impl Path {
    /// Starting out through the carrier, which is how the handshake goes.
    pub(crate) fn new(udp: bool) -> Self {
        Self {
            way: Way::Carrier { retry: None },
            udp,
            udp_worked: false,
            carrier_open: true,
            answers: Answers::Answered,
        }
    }

    /// How the client's packets reach the daemon, as far as it knows.
    pub(crate) fn route(&self) -> Route {
        match self.way {
            Way::Carrier { .. } | Way::Trying { .. } => Route::Ssh,
            Way::Udp => Route::Udp,
        }
    }

    /// Where the next packet goes.
    pub(crate) fn send(&self) -> Sending {
        match self.way {
            Way::Carrier { .. } => Sending::Carrier,
            Way::Trying { .. } if self.carrier_open => Sending::Carrier,
            Way::Trying { .. } | Way::Udp => Sending::Udp,
        }
    }

    /// The handshake is done: time to try UDP.
    pub(crate) fn connected(&mut self, now: Instant) -> Action {
        if self.udp {
            self.try_udp(now)
        } else {
            Action::Nothing
        }
    }

    /// A packet went out, whichever way.
    pub(crate) fn sent(&mut self, now: Instant) {
        if self.answers == Answers::Answered {
            self.answers = Answers::Unanswered { since: now };
        }
    }

    /// Starts trying UDP.
    fn try_udp(&mut self, now: Instant) -> Action {
        self.way = Way::Trying {
            since: now,
            next_probe: now + PROBE_INTERVAL,
        };
        Action::Probe
    }

    /// A packet came from the daemon, over UDP or through the carrier, or a
    /// probe came back.
    pub(crate) fn received(&mut self, over_udp: bool) {
        self.answers = Answers::Answered;
        if over_udp {
            self.udp_worked = true;
            self.way = Way::Udp;
        }
    }

    /// Since when the daemon has been out of contact, as of the last
    /// [`tick`](Self::tick), if it is.
    pub(crate) fn silent_since(&self) -> Option<Instant> {
        match self.answers {
            Answers::Silent { since } => Some(since),
            _ => None,
        }
    }

    /// The carrier closed.
    pub(crate) fn carrier_closed(&mut self, now: Instant) -> Action {
        self.carrier_open = false;
        if !self.udp_worked {
            return Action::Lost;
        }
        match self.way {
            Way::Carrier { .. } => self.try_udp(now),
            Way::Trying { .. } | Way::Udp => Action::Nothing,
        }
    }

    /// Acts on whatever has come due by `now`.
    /// One thing at a time: anything else due is still due, and
    /// [`next_tick`](Self::next_tick) says so.
    pub(crate) fn tick(&mut self, now: Instant) -> Action {
        let way = self.tick_way(now);
        if way != Action::Nothing {
            return way;
        }
        match self.answers {
            Answers::Unanswered { since } if now >= since + SUSPECT_AFTER => {
                self.answers = Answers::Pinged { since };
                Action::Ping
            }
            Answers::Pinged { since } if now >= since + SILENT_AFTER => {
                self.answers = Answers::Silent { since };
                Action::Nothing
            }
            _ => Action::Nothing,
        }
    }

    /// Moves between ways, as whatever has come due by `now` says.
    fn tick_way(&mut self, now: Instant) -> Action {
        match self.way {
            Way::Carrier { retry: Some(retry) } if now >= retry => self.try_udp(now),
            Way::Trying { since, .. } if now >= since + PROBE_TIMEOUT => {
                if self.carrier_open {
                    self.way = Way::Carrier { retry: None };
                    Action::Nothing
                } else {
                    // UDP is all there is: keep asking.
                    self.try_udp(now)
                }
            }
            Way::Trying { since, next_probe } if now >= next_probe => {
                self.way = Way::Trying {
                    since,
                    next_probe: now + PROBE_INTERVAL,
                };
                Action::Probe
            }
            Way::Udp
                if self.carrier_open
                    && self
                        .answers
                        .since()
                        .is_some_and(|since| now >= since + GIVE_UP_AFTER) =>
            {
                self.way = Way::Carrier {
                    retry: Some(now + RETRY_AFTER),
                };
                Action::Nothing
            }
            _ => Action::Nothing,
        }
    }

    /// When [`tick`](Self::tick) next has something to do, or the daemon
    /// next goes out of contact.
    pub(crate) fn next_tick(&self) -> Option<Instant> {
        let way = match self.way {
            Way::Carrier { retry } => retry,
            Way::Trying { since, next_probe } => Some(next_probe.min(since + PROBE_TIMEOUT)),
            Way::Udp if self.carrier_open => {
                self.answers.since().map(|since| since + GIVE_UP_AFTER)
            }
            Way::Udp => None,
        };
        let contact = match self.answers {
            Answers::Unanswered { since } => Some(since + SUSPECT_AFTER),
            Answers::Pinged { since } => Some(since + SILENT_AFTER),
            Answers::Answered | Answers::Silent { .. } => None,
        };
        way.into_iter().chain(contact).min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Times `ms` milliseconds after `start`.
    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn without_udp_everything_goes_through_the_carrier() {
        let start = Instant::now();
        let mut path = Path::new(false);
        assert_eq!(path.connected(start), Action::Nothing);
        assert_eq!((path.send(), path.route()), (Sending::Carrier, Route::Ssh));
        assert_eq!(path.next_tick(), None);
        assert_eq!(path.carrier_closed(start), Action::Lost);
    }

    #[test]
    fn udp_is_tried_both_ways_and_an_answer_settles_it() {
        let start = Instant::now();
        let mut path = Path::new(true);
        assert_eq!(
            path.send(),
            Sending::Carrier,
            "the handshake goes through SSH"
        );
        assert_eq!(path.connected(start), Action::Probe);
        // Nothing waits on the probes.
        assert_eq!((path.send(), path.route()), (Sending::Carrier, Route::Ssh));
        assert_eq!(path.next_tick(), Some(start + PROBE_INTERVAL));
        assert_eq!(
            path.tick(start + PROBE_INTERVAL),
            Action::Probe,
            "in case of loss"
        );
        path.received(false);
        assert_eq!(
            path.route(),
            Route::Ssh,
            "the carrier's answer settles nothing"
        );
        path.received(true);
        assert_eq!((path.send(), path.route()), (Sending::Udp, Route::Udp));
        assert_eq!(path.next_tick(), None, "nothing to wait for");
    }

    #[test]
    fn a_try_that_goes_unanswered_stays_with_the_carrier() {
        let start = Instant::now();
        let mut path = Path::new(true);
        path.connected(start);
        let mut probes = 1;
        while path.next_tick() < Some(start + PROBE_TIMEOUT) {
            let due = path.next_tick().unwrap();
            assert_eq!(path.tick(due), Action::Probe);
            probes += 1;
        }
        assert_eq!(probes, 4);
        assert_eq!(path.tick(start + PROBE_TIMEOUT), Action::Nothing);
        assert_eq!(path.send(), Sending::Carrier);
        assert_eq!(path.next_tick(), None, "UDP is blocked: not tried again");
    }

    #[test]
    fn silence_brings_a_ping_then_counts_as_out_of_contact() {
        let start = Instant::now();
        let mut path = Path::new(false);
        path.sent(start);
        // More packets don't move when the silence began.
        path.sent(at(start, 500));
        assert_eq!(path.next_tick(), Some(start + SUSPECT_AFTER));
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Ping);
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Nothing, "once");
        path.tick(at(start, 2999));
        assert_eq!(path.silent_since(), None);
        assert_eq!(path.next_tick(), Some(start + SILENT_AFTER));
        path.tick(start + SILENT_AFTER);
        assert_eq!(path.silent_since(), Some(start));
        assert_eq!(path.next_tick(), None, "nothing more to wait for");
        // Anything back, whichever way, and it's over.
        path.received(false);
        assert_eq!(path.silent_since(), None);
        assert_eq!(path.next_tick(), None);
    }

    #[test]
    fn a_ping_due_with_a_probe_still_goes() {
        let start = Instant::now();
        let mut path = Path::new(true);
        path.connected(start);
        path.sent(start);
        // Both due at once: the probe first, then the ping, at once.
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Probe);
        assert!(path.next_tick() <= Some(start + SUSPECT_AFTER));
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Ping);
    }

    #[test]
    fn silence_on_udp_brings_the_carrier_back_then_another_try() {
        let start = Instant::now();
        let mut path = Path::new(true);
        path.connected(start);
        path.received(true);

        let sent = at(start, 5000);
        path.sent(sent);
        assert_eq!(path.tick(sent + SUSPECT_AFTER), Action::Ping);
        assert_eq!(path.next_tick(), Some(sent + SILENT_AFTER));
        let gave_up = sent + GIVE_UP_AFTER;
        assert_eq!(path.tick(gave_up), Action::Nothing);
        assert_eq!((path.send(), path.route()), (Sending::Carrier, Route::Ssh));
        // The carrier answers.
        path.received(false);
        assert_eq!(path.next_tick(), Some(gave_up + RETRY_AFTER));
        assert_eq!(path.tick(gave_up + RETRY_AFTER), Action::Probe);
        assert_eq!(path.send(), Sending::Carrier);
    }

    #[test]
    fn once_the_carrier_closes_udp_is_all_there_is() {
        let start = Instant::now();

        // On UDP: it stays, however long the silence.
        let mut path = Path::new(true);
        path.connected(start);
        path.received(true);
        assert_eq!(path.carrier_closed(start), Action::Nothing);
        path.sent(start);
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Ping);
        assert_eq!(path.tick(start + GIVE_UP_AFTER * 6), Action::Nothing);
        assert_eq!(path.send(), Sending::Udp);
        assert_eq!(path.silent_since(), Some(start));

        // Back through the carrier after UDP went quiet: UDP is tried, and
        // kept trying.
        let mut path = Path::new(true);
        path.connected(start);
        path.received(true);
        path.sent(start);
        path.tick(start + SUSPECT_AFTER);
        path.tick(start + GIVE_UP_AFTER);
        assert_eq!(path.send(), Sending::Carrier);
        assert_eq!(path.carrier_closed(start), Action::Probe);
        assert_eq!(path.send(), Sending::Udp);
        assert_eq!(path.tick(start + PROBE_TIMEOUT), Action::Probe);
        assert_eq!(path.send(), Sending::Udp);

        // UDP never worked: the connection is lost, trying or not.
        let mut path = Path::new(true);
        path.connected(start);
        assert_eq!(path.carrier_closed(start), Action::Lost);
        let mut path = Path::new(true);
        path.connected(start);
        path.tick(start + PROBE_TIMEOUT);
        assert_eq!(path.carrier_closed(start), Action::Lost);
    }
}
