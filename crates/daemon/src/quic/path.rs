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
//! on: the daemon moves once, when the first arrives. If no probe comes
//! back, UDP is tried again every so often: a lossy network can lose a
//! whole try.
//!
//! While on UDP, packets that go unanswered for long enough send it back
//! to the carrier, if the carrier is still open, and it tries UDP again
//! later. Long enough is longer than ssh takes to give up on a silent
//! network, so when the whole network goes away, as it usually does, ssh
//! has closed before then, and nothing moves for nothing. Once the carrier
//! has closed, UDP is all there is, if it has ever worked, until the
//! driver opens a new carrier (with ssh again): a connection only ends
//! when the carrier closes with no other way, and no new one can come.
//!
//! Whatever the way, packets that go unanswered for a second bring a ping,
//! and if that goes unanswered for long enough, the daemon is out of
//! contact until something comes back, which the window shows.
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
/// counts as out of contact: longer than the stalls of a relay over TCP,
/// such as Tailscale's DERP, which hold everything for a few seconds and
/// then let it go at once.
const SILENT_AFTER: Duration = Duration::from_secs(5);

/// How long packets over UDP may go unanswered, despite the ping, before
/// the carrier takes over again: longer than ssh's 15 s to give up on a
/// silent network (see [`remote`](crate::remote)), so that when the whole
/// network goes away, the connection doesn't move onto a carrier that's
/// about to close.
const GIVE_UP_AFTER: Duration = Duration::from_secs(20);

/// How long after a try at UDP that failed, or giving up on it, it's tried
/// again.
const RETRY_AFTER: Duration = Duration::from_secs(15);

/// How often a probe goes over UDP while the daemon is out of contact
/// there, to find out the moment the network is back.
const SILENT_PROBE_EVERY: Duration = Duration::from_secs(1);

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
#[allow(
    clippy::struct_excessive_bools,
    reason = "four separate facts, each true or not, that the ways depend on"
)]
pub(crate) struct Path {
    way: Way,
    /// Whether the daemon can be reached over UDP at all: it has a port,
    /// and the client knows its address.
    udp: bool,
    /// Whether a new carrier can be opened when one closes.
    redial: bool,
    /// Whether anything has ever come back over UDP.
    udp_worked: bool,
    carrier_open: bool,
    answers: Answers,
    /// While out of contact over UDP, when to send the next probe.
    silent_probe_at: Option<Instant>,
}

impl Path {
    /// Starting out through the carrier, which is how the handshake goes.
    /// With `udp`, the daemon can be tried over UDP; with `redial`, the
    /// driver can open a new carrier when this one closes.
    pub(crate) fn new(udp: bool, redial: bool) -> Self {
        Self {
            way: Way::Carrier { retry: None },
            udp,
            redial,
            udp_worked: false,
            carrier_open: true,
            answers: Answers::Answered,
            silent_probe_at: None,
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

    /// Whether a try at UDP is under way.
    pub(crate) fn trying(&self) -> bool {
        matches!(self.way, Way::Trying { .. })
    }

    /// The handshake is done: time to try UDP.
    pub(crate) fn connected(&mut self, now: Instant) -> Action {
        if self.udp {
            self.try_udp(now)
        } else {
            Action::Nothing
        }
    }

    /// The daemon can be tried over UDP now, as it couldn't before: a
    /// connection carried through another machine has a carrier through
    /// ssh now, which said where the machine is. Tried at once, if the
    /// handshake is done.
    pub(crate) fn udp_found(&mut self, now: Instant) -> Action {
        if self.udp {
            return Action::Nothing;
        }
        self.udp = true;
        if matches!(self.way, Way::Carrier { .. }) {
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

    /// A packet came from the daemon, over UDP or through the carrier. One
    /// over UDP settles a try at it, but doesn't bring the connection back
    /// onto UDP after giving up on it: it may only be late, and only a
    /// probe answered shows UDP gets through again.
    pub(crate) fn received(&mut self, over_udp: bool) {
        self.answers = Answers::Answered;
        self.silent_probe_at = None;
        if over_udp {
            self.udp_worked = true;
            if matches!(self.way, Way::Trying { .. }) {
                self.way = Way::Udp;
            }
        }
    }

    /// A probe came back over UDP: it gets through. Returns whether the
    /// daemon had been out of contact, so the network has just come back,
    /// with nothing from QUIC yet.
    ///
    /// QUIC waits longer and longer to send again while nothing comes back
    /// (its probe timeout doubles each time), so after an outage of 30 s,
    /// it may not try again for another 30. The driver can't hurry its
    /// timers, but it can send from a new port: the daemon sees the client
    /// move, and starts the new path afresh, at once.
    pub(crate) fn probe_answered(&mut self) -> bool {
        let recovered = matches!(self.answers, Answers::Silent { .. });
        self.answers = Answers::Answered;
        self.silent_probe_at = None;
        self.udp_worked = true;
        self.way = Way::Udp;
        recovered
    }

    /// Since when what was sent has gone unanswered, if it has.
    pub(crate) fn unanswered_since(&self) -> Option<Instant> {
        self.answers.since()
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
        if !self.udp_worked && !self.redial {
            return Action::Lost;
        }
        match self.way {
            Way::Carrier { .. } if self.udp => self.try_udp(now),
            Way::Carrier { .. } | Way::Trying { .. } | Way::Udp => Action::Nothing,
        }
    }

    /// A new carrier opened, in place of one that closed. If UDP has gone
    /// quiet meanwhile, it's tried again, through the new carrier: the
    /// network is evidently back, and UDP may be too, as it usually comes
    /// back with it, or it may be what's broken.
    pub(crate) fn carrier_opened(&mut self, now: Instant) -> Action {
        self.carrier_open = true;
        if self.way == Way::Udp && self.answers != Answers::Answered {
            self.try_udp(now)
        } else {
            Action::Nothing
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
                if self.way == Way::Udp {
                    self.silent_probe_at = Some(now);
                }
                Action::Nothing
            }
            Answers::Silent { .. }
                if self.way == Way::Udp && self.silent_probe_at.is_some_and(|at| now >= at) =>
            {
                self.silent_probe_at = Some(now + SILENT_PROBE_EVERY);
                Action::Probe
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
                    self.way = Way::Carrier {
                        retry: Some(now + RETRY_AFTER),
                    };
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
            Answers::Silent { .. } if self.way == Way::Udp => self.silent_probe_at,
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
    fn udp_found_later_is_tried_at_once() {
        let start = Instant::now();
        let mut path = Path::new(false, true);
        assert_eq!(path.connected(start), Action::Nothing);
        // A carrier through ssh, at last, says where the daemon is.
        assert_eq!(path.udp_found(at(start, 100)), Action::Probe);
        assert!(path.trying());
        assert!(path.probe_answered() || path.route() == Route::Udp);
        assert_eq!(path.route(), Route::Udp);
        // Found again, nothing new.
        assert_eq!(path.udp_found(at(start, 200)), Action::Nothing);
    }

    #[test]
    fn without_udp_everything_goes_through_the_carrier() {
        let start = Instant::now();
        let mut path = Path::new(false, false);
        assert_eq!(path.connected(start), Action::Nothing);
        assert_eq!((path.send(), path.route()), (Sending::Carrier, Route::Ssh));
        assert_eq!(path.next_tick(), None);
        assert_eq!(path.carrier_closed(start), Action::Lost);
    }

    #[test]
    fn udp_is_tried_with_probes_and_an_answer_settles_it() {
        let start = Instant::now();
        let mut path = Path::new(true, false);
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
        path.probe_answered();
        assert_eq!((path.send(), path.route()), (Sending::Udp, Route::Udp));
        assert_eq!(path.next_tick(), None, "nothing to wait for");
    }

    #[test]
    fn a_try_that_goes_unanswered_stays_with_the_carrier_and_tries_again_later() {
        let start = Instant::now();
        let mut path = Path::new(true, false);
        path.connected(start);
        let mut probes = 1;
        while path.next_tick() < Some(start + PROBE_TIMEOUT) {
            let due = path.next_tick().unwrap();
            assert_eq!(path.tick(due), Action::Probe);
            probes += 1;
        }
        assert_eq!(probes, 4);
        let failed = start + PROBE_TIMEOUT;
        assert_eq!(path.tick(failed), Action::Nothing);
        assert_eq!(path.send(), Sending::Carrier);
        // A lossy network can lose a whole try.
        assert_eq!(path.next_tick(), Some(failed + RETRY_AFTER));
        assert_eq!(path.tick(failed + RETRY_AFTER), Action::Probe);
        assert!(path.trying());
    }

    #[test]
    fn silence_brings_a_ping_then_counts_as_out_of_contact() {
        let start = Instant::now();
        let mut path = Path::new(false, false);
        path.sent(start);
        // More packets don't move when the silence began.
        path.sent(at(start, 500));
        assert_eq!(path.next_tick(), Some(start + SUSPECT_AFTER));
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Ping);
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Nothing, "once");
        // A relay's stall of a few seconds isn't a lost daemon.
        path.tick(start + SILENT_AFTER.saturating_sub(Duration::from_millis(1)));
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
        let mut path = Path::new(true, false);
        path.connected(start);
        path.sent(start);
        // Both due at once: the probe first, then the ping, at once.
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Probe);
        assert!(path.next_tick() <= Some(start + SUSPECT_AFTER));
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Ping);
    }

    #[test]
    fn silence_on_udp_brings_the_carrier_back_only_after_ssh_would_have_given_up() {
        let start = Instant::now();
        let mut path = Path::new(true, false);
        path.connected(start);
        path.probe_answered();

        let sent = at(start, 5000);
        path.sent(sent);
        assert_eq!(path.tick(sent + SUSPECT_AFTER), Action::Ping);
        assert_eq!(path.next_tick(), Some(sent + SILENT_AFTER));
        path.tick(sent + SILENT_AFTER);
        // Probes go each second meanwhile, to find the network back.
        assert_eq!(path.tick(sent + SILENT_AFTER), Action::Probe);
        assert_eq!(
            path.next_tick(),
            Some(sent + SILENT_AFTER + SILENT_PROBE_EVERY)
        );
        assert!(GIVE_UP_AFTER > Duration::from_secs(15));
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
    fn out_of_contact_over_udp_probes_find_the_network_back_at_once() {
        let start = Instant::now();
        let mut path = Path::new(true, false);
        path.connected(start);
        path.probe_answered();
        path.carrier_closed(start);
        path.sent(start);
        path.tick(start + SUSPECT_AFTER);
        let silent = start + SILENT_AFTER;
        assert_eq!(path.tick(silent), Action::Nothing);
        assert_eq!(path.next_tick(), Some(silent));
        // A probe each second, cheap, while nothing comes back.
        assert_eq!(path.tick(silent), Action::Probe);
        assert_eq!(path.next_tick(), Some(silent + SILENT_PROBE_EVERY));
        assert_eq!(path.tick(silent + SILENT_PROBE_EVERY), Action::Probe);
        // One comes back: the network is back, before QUIC knows.
        assert!(path.probe_answered());
        assert_eq!(path.silent_since(), None);
        assert_eq!(path.next_tick(), None);
        // Answered while in contact, it's nothing new.
        assert!(!path.probe_answered());
    }

    #[test]
    fn a_late_packet_over_udp_doesnt_undo_giving_up_on_it() {
        let start = Instant::now();
        let mut path = Path::new(true, false);
        path.connected(start);
        path.probe_answered();
        path.sent(start);
        path.tick(start + SUSPECT_AFTER);
        path.tick(start + GIVE_UP_AFTER);
        assert_eq!(path.route(), Route::Ssh);
        // Held up somewhere, and only now arrived.
        path.received(true);
        assert_eq!(path.route(), Route::Ssh, "no flapping");
        assert_eq!(path.silent_since(), None, "but the daemon is there");
        // A probe answered, when UDP is tried again, brings it back.
        path.tick(start + GIVE_UP_AFTER + RETRY_AFTER);
        path.probe_answered();
        assert_eq!(path.route(), Route::Udp);
    }

    #[test]
    fn once_the_carrier_closes_udp_is_all_there_is() {
        let start = Instant::now();

        // On UDP: it stays, however long the silence.
        let mut path = Path::new(true, false);
        path.connected(start);
        path.probe_answered();
        assert_eq!(path.carrier_closed(start), Action::Nothing);
        path.sent(start);
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Ping);
        assert_eq!(path.tick(start + GIVE_UP_AFTER * 6), Action::Nothing);
        assert_eq!(path.send(), Sending::Udp);
        assert_eq!(path.silent_since(), Some(start));

        // Back through the carrier after UDP went quiet: UDP is tried, and
        // kept trying.
        let mut path = Path::new(true, false);
        path.connected(start);
        path.probe_answered();
        path.sent(start);
        path.tick(start + SUSPECT_AFTER);
        path.tick(start + GIVE_UP_AFTER);
        assert_eq!(path.send(), Sending::Carrier);
        assert_eq!(path.carrier_closed(start), Action::Probe);
        assert_eq!(path.send(), Sending::Udp);
        assert_eq!(path.tick(start + PROBE_TIMEOUT), Action::Probe);
        assert_eq!(path.send(), Sending::Udp);

        // UDP never worked, and no new carrier can come: the connection
        // is lost, trying or not.
        let mut path = Path::new(true, false);
        path.connected(start);
        assert_eq!(path.carrier_closed(start), Action::Lost);
        let mut path = Path::new(true, false);
        path.connected(start);
        path.tick(start + PROBE_TIMEOUT);
        assert_eq!(path.carrier_closed(start), Action::Lost);
    }

    #[test]
    fn with_a_new_carrier_to_come_a_closed_one_is_never_the_end() {
        let start = Instant::now();

        // UDP never worked: it's tried meanwhile, and the new carrier
        // takes over.
        let mut path = Path::new(true, true);
        path.connected(start);
        path.tick(start + PROBE_TIMEOUT);
        assert_eq!(path.carrier_closed(start), Action::Probe);
        assert_eq!(path.send(), Sending::Udp);
        assert_eq!(
            path.carrier_opened(start),
            Action::Nothing,
            "already trying"
        );
        assert_eq!((path.send(), path.route()), (Sending::Carrier, Route::Ssh));
        // The try ends, and the carrier carries on.
        assert_eq!(path.tick(start + PROBE_TIMEOUT * 2), Action::Nothing);
        assert_eq!(path.send(), Sending::Carrier);

        // No UDP at all: it waits for the new carrier.
        let mut path = Path::new(false, true);
        path.connected(start);
        assert_eq!(path.carrier_closed(start), Action::Nothing);
        assert_eq!(path.carrier_opened(start), Action::Nothing);
        assert_eq!(path.send(), Sending::Carrier);

        // On UDP, a new carrier changes nothing.
        let mut path = Path::new(true, true);
        path.connected(start);
        path.probe_answered();
        path.carrier_closed(start);
        assert_eq!(path.carrier_opened(start), Action::Nothing);
        assert_eq!((path.send(), path.route()), (Sending::Udp, Route::Udp));

        // On UDP gone quiet, as when the whole network went away and the
        // new carrier is the first sign it's back: UDP is tried again,
        // through the carrier, rather than given up on.
        let mut path = Path::new(true, true);
        path.connected(start);
        path.probe_answered();
        path.carrier_closed(start);
        path.sent(start);
        path.tick(start + SILENT_AFTER);
        let back = start + GIVE_UP_AFTER * 2;
        assert_eq!(path.carrier_opened(back), Action::Probe);
        assert_eq!((path.send(), path.route()), (Sending::Carrier, Route::Ssh));
        assert_eq!(path.tick(back), Action::Nothing, "no giving up at once");
        assert!(path.trying());
        path.probe_answered();
        assert_eq!((path.send(), path.route()), (Sending::Udp, Route::Udp));
    }
}
