//! Which way a client's packets go to the daemon: through the carrier, or
//! straight over UDP, and when to try the other.
//!
//! The client's QUIC connection always believes the daemon is at one
//! address; this decides, underneath, how packets for it travel. The
//! daemon follows: QUIC moves its side of the connection to wherever the
//! newest packet came from. QUIC only lets a client move, so the client
//! can't follow a daemon to a new address itself.
//!
//! Once the handshake is done, the client tries UDP, sending every packet
//! both ways so that nothing waits on an answer that may never come. The
//! first packet back over UDP settles it. While on UDP, packets that go
//! unanswered for long enough send it back to the carrier, if the carrier
//! is still open, and it tries UDP again later. Once the carrier has
//! closed, as when the client's network changed under SSH, UDP is all
//! there is, if it has ever worked; if it hasn't, the connection is lost,
//! as it would be with no UDP at all.
//!
//! No I/O happens here: the driver says what happened, and does what it's
//! told.

use std::time::{Duration, Instant};

/// How long a try at UDP waits for an answer.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long packets over UDP may go unanswered before a ping asks for an
/// answer. What's sent may not need one, such as an acknowledgement.
const SUSPECT_AFTER: Duration = Duration::from_secs(1);

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
pub(crate) enum Send {
    Carrier,
    Udp,
    /// Both, while trying UDP.
    Both,
}

/// What the driver must do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Nothing,
    /// Send something that needs an answer, to find out whether UDP works.
    Ping,
    /// Give up: there's no way left to reach the daemon.
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    /// Through the carrier, trying UDP again at `retry`, if ever.
    Carrier { retry: Option<Instant> },
    /// Both ways, since `since`, until something comes back over UDP.
    Trying { since: Instant },
    /// Over UDP. `unanswered`: when the first packet since anything last
    /// came back went out.
    Udp {
        unanswered: Option<Instant>,
        pinged: bool,
    },
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
}

impl Path {
    /// Starting out through the carrier, which is how the handshake goes.
    pub(crate) fn new(udp: bool) -> Self {
        Self {
            way: Way::Carrier { retry: None },
            udp,
            udp_worked: false,
            carrier_open: true,
        }
    }

    /// How the client's packets reach the daemon, as far as it knows.
    pub(crate) fn route(&self) -> Route {
        match self.way {
            Way::Carrier { .. } | Way::Trying { .. } => Route::Ssh,
            Way::Udp { .. } => Route::Udp,
        }
    }

    /// Where the next packet goes.
    pub(crate) fn send(&self) -> Send {
        match self.way {
            Way::Carrier { .. } => Send::Carrier,
            Way::Trying { .. } if self.carrier_open => Send::Both,
            Way::Trying { .. } | Way::Udp { .. } => Send::Udp,
        }
    }

    /// The handshake is done: time to try UDP.
    pub(crate) fn connected(&mut self, now: Instant) -> Action {
        if self.udp {
            self.way = Way::Trying { since: now };
            Action::Ping
        } else {
            Action::Nothing
        }
    }

    /// A packet went out over UDP.
    pub(crate) fn sent_over_udp(&mut self, now: Instant) {
        if let Way::Udp {
            unanswered: unanswered @ None,
            ..
        } = &mut self.way
        {
            *unanswered = Some(now);
        }
    }

    /// A packet came from the daemon over UDP.
    pub(crate) fn received_over_udp(&mut self) {
        self.udp_worked = true;
        self.way = Way::Udp {
            unanswered: None,
            pinged: false,
        };
    }

    /// The carrier closed.
    pub(crate) fn carrier_closed(&mut self, now: Instant) -> Action {
        self.carrier_open = false;
        if !self.udp_worked {
            return Action::Lost;
        }
        match self.way {
            Way::Carrier { .. } => {
                self.way = Way::Trying { since: now };
                Action::Ping
            }
            Way::Trying { .. } | Way::Udp { .. } => Action::Nothing,
        }
    }

    /// Acts on whatever has come due by `now`.
    pub(crate) fn tick(&mut self, now: Instant) -> Action {
        match self.way {
            Way::Carrier { retry: Some(retry) } if now >= retry => {
                self.way = Way::Trying { since: now };
                Action::Ping
            }
            Way::Trying { since } if now >= since + PROBE_TIMEOUT => {
                if self.carrier_open {
                    self.way = Way::Carrier { retry: None };
                    Action::Nothing
                } else {
                    // UDP is all there is: keep asking.
                    self.way = Way::Trying { since: now };
                    Action::Ping
                }
            }
            Way::Udp {
                unanswered: Some(since),
                ..
            } if self.carrier_open && now >= since + GIVE_UP_AFTER => {
                self.way = Way::Carrier {
                    retry: Some(now + RETRY_AFTER),
                };
                Action::Nothing
            }
            Way::Udp {
                unanswered: Some(since),
                pinged: false,
            } if now >= since + SUSPECT_AFTER => {
                self.way = Way::Udp {
                    unanswered: Some(since),
                    pinged: true,
                };
                Action::Ping
            }
            _ => Action::Nothing,
        }
    }

    /// When [`tick`](Self::tick) next has something to do.
    pub(crate) fn next_tick(&self) -> Option<Instant> {
        match self.way {
            Way::Carrier { retry } => retry,
            Way::Trying { since } => Some(since + PROBE_TIMEOUT),
            Way::Udp {
                unanswered: Some(since),
                pinged: false,
            } => Some(since + SUSPECT_AFTER),
            Way::Udp {
                unanswered: Some(since),
                pinged: true,
            } if self.carrier_open => Some(since + GIVE_UP_AFTER),
            Way::Udp { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_udp_everything_goes_through_the_carrier() {
        let start = Instant::now();
        let mut path = Path::new(false);
        assert_eq!(path.connected(start), Action::Nothing);
        assert_eq!((path.send(), path.route()), (Send::Carrier, Route::Ssh));
        assert_eq!(path.next_tick(), None);
        assert_eq!(path.carrier_closed(start), Action::Lost);
    }

    #[test]
    fn udp_is_tried_both_ways_and_an_answer_settles_it() {
        let start = Instant::now();
        let mut path = Path::new(true);
        assert_eq!(path.send(), Send::Carrier, "the handshake goes through SSH");
        assert_eq!(path.connected(start), Action::Ping);
        assert_eq!((path.send(), path.route()), (Send::Both, Route::Ssh));
        path.received_over_udp();
        assert_eq!((path.send(), path.route()), (Send::Udp, Route::Udp));
        assert_eq!(path.next_tick(), None, "nothing to wait for");
    }

    #[test]
    fn a_try_that_goes_unanswered_stays_with_the_carrier() {
        let start = Instant::now();
        let mut path = Path::new(true);
        path.connected(start);
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Nothing);
        assert_eq!(path.next_tick(), Some(start + PROBE_TIMEOUT));
        assert_eq!(path.tick(start + PROBE_TIMEOUT), Action::Nothing);
        assert_eq!(path.send(), Send::Carrier);
        assert_eq!(path.next_tick(), None, "UDP is blocked: not tried again");
    }

    #[test]
    fn silence_on_udp_brings_a_ping_then_the_carrier_back_then_another_try() {
        let start = Instant::now();
        let mut path = Path::new(true);
        path.connected(start);
        path.received_over_udp();

        path.sent_over_udp(start);
        // An answer in time clears it.
        path.sent_over_udp(start + Duration::from_millis(500));
        assert_eq!(path.next_tick(), Some(start + SUSPECT_AFTER));
        path.received_over_udp();
        assert_eq!(path.next_tick(), None);

        let sent = start + Duration::from_secs(5);
        path.sent_over_udp(sent);
        assert_eq!(path.tick(sent + SUSPECT_AFTER), Action::Ping);
        assert_eq!(path.tick(sent + SUSPECT_AFTER), Action::Nothing, "once");
        assert_eq!(path.next_tick(), Some(sent + GIVE_UP_AFTER));
        let gave_up = sent + GIVE_UP_AFTER;
        assert_eq!(path.tick(gave_up), Action::Nothing);
        assert_eq!((path.send(), path.route()), (Send::Carrier, Route::Ssh));

        assert_eq!(path.next_tick(), Some(gave_up + RETRY_AFTER));
        assert_eq!(path.tick(gave_up + RETRY_AFTER), Action::Ping);
        assert_eq!(path.send(), Send::Both);
    }

    #[test]
    fn once_the_carrier_closes_udp_is_all_there_is() {
        let start = Instant::now();

        // On UDP: it stays, however long the silence.
        let mut path = Path::new(true);
        path.connected(start);
        path.received_over_udp();
        assert_eq!(path.carrier_closed(start), Action::Nothing);
        path.sent_over_udp(start);
        assert_eq!(path.tick(start + SUSPECT_AFTER), Action::Ping);
        assert_eq!(path.tick(start + GIVE_UP_AFTER * 6), Action::Nothing);
        assert_eq!(path.send(), Send::Udp);
        assert_eq!(path.next_tick(), None);

        // Back through the carrier after UDP went quiet: UDP is tried, and
        // kept trying.
        let mut path = Path::new(true);
        path.connected(start);
        path.received_over_udp();
        path.sent_over_udp(start);
        path.tick(start + GIVE_UP_AFTER);
        assert_eq!(path.send(), Send::Carrier);
        assert_eq!(path.carrier_closed(start), Action::Ping);
        assert_eq!(path.send(), Send::Udp);
        assert_eq!(path.tick(start + PROBE_TIMEOUT), Action::Ping);
        assert_eq!(path.send(), Send::Udp);

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
