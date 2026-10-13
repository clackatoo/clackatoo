//! What becomes of each packet: the conditions of a link, and its state.

use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Which way a packet goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// From the client to the server.
    Up,
    /// From the server to the client.
    Down,
}

impl Direction {
    fn index(self) -> usize {
        match self {
            Self::Up => 0,
            Self::Down => 1,
        }
    }
}

/// What one direction of a link does to what passes.
#[derive(Clone, Debug, PartialEq)]
pub struct Impairment {
    /// The chance, from 0 to 1, that a datagram is lost. Streams lose
    /// nothing.
    pub loss: f64,
    /// How long everything takes to cross.
    pub delay: Duration,
    /// Up to how much longer each packet may take, chosen evenly. Packets
    /// overtake each other unless the link keeps them `in_order`.
    pub jitter: Duration,
    /// The chance, from 0 to 1, that a packet is held back by another
    /// `delay`, behind later ones.
    pub reorder: f64,
    /// How many bytes a second get through, if limited. Packets queue for
    /// their turn.
    pub rate: Option<u64>,
    /// How long the queue for a limited rate may get before datagrams are
    /// dropped, as a router's buffer fills.
    pub queue: Duration,
    /// The largest IP packet the link takes, counting IPv6 and UDP headers
    /// (48 bytes): bigger datagrams are dropped, as a tunnel that doesn't
    /// fragment drops them. Streams are cut into segments that fit.
    pub mtu: Option<usize>,
    /// Whether packets leave in the order they came, as through TCP.
    pub in_order: bool,
}

impl Default for Impairment {
    fn default() -> Self {
        Self {
            loss: 0.0,
            delay: Duration::ZERO,
            jitter: Duration::ZERO,
            reorder: 0.0,
            rate: None,
            queue: Duration::from_secs(1),
            mtu: None,
            in_order: false,
        }
    }
}

/// Now and then, everything is held for a while and then let go at once,
/// as when a relay carrying packets over TCP waits for a lost segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stalls {
    /// How long between stalls, chosen evenly from the range.
    pub every: Range<Duration>,
    /// How long each lasts, chosen evenly from the range.
    pub lasting: Range<Duration>,
}

/// What a link does each way.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conditions {
    /// From the client to the server.
    pub up: Impairment,
    /// From the server to the client.
    pub down: Impairment,
    /// Stalls, both ways at once.
    pub stalls: Option<Stalls>,
}

impl Conditions {
    /// A link that does nothing.
    #[must_use]
    pub fn clean() -> Self {
        Self::default()
    }

    /// Both ways the same.
    #[must_use]
    pub fn both(impairment: Impairment) -> Self {
        Self {
            up: impairment.clone(),
            down: impairment,
            stalls: None,
        }
    }

    /// `loss` each way, a round trip of `rtt` on average, varying by up to
    /// `jitter` either way: bad Wi-Fi.
    #[must_use]
    pub fn lossy(loss: f64, rtt: Duration, jitter: Duration) -> Self {
        // Half the jitter each way, around half the round trip.
        let each = jitter / 2;
        Self::both(Impairment {
            loss,
            delay: (rtt / 2).saturating_sub(each / 2),
            jitter: each,
            ..Impairment::default()
        })
    }

    /// Like a path relayed through one of Tailscale's DERP servers, which
    /// carries packets inside TCP: nothing lost, nothing reordered, a round
    /// trip of 100 to 250 ms, about 1.5 Mbit/s, and every 5 to 20 s, a
    /// stall of half a second to 3 s, then everything at once.
    #[must_use]
    pub fn derp() -> Self {
        Self {
            stalls: Some(Stalls {
                every: Duration::from_secs(5)..Duration::from_secs(20),
                lasting: Duration::from_millis(500)..Duration::from_secs(3),
            }),
            ..Self::both(Impairment {
                delay: Duration::from_millis(50),
                jitter: Duration::from_millis(75),
                rate: Some(1_500_000 / 8),
                queue: Duration::from_secs(10),
                mtu: Some(1280),
                in_order: true,
                ..Impairment::default()
            })
        }
    }

    /// The same, but with nothing bigger than `mtu` getting through.
    #[must_use]
    pub fn with_mtu(mut self, mtu: usize) -> Self {
        self.up.mtu = Some(mtu);
        self.down.mtu = Some(mtu);
        self
    }

    fn get(&self, direction: Direction) -> &Impairment {
        match direction {
            Direction::Up => &self.up,
            Direction::Down => &self.down,
        }
    }
}

/// How many packets a link has passed and dropped, each way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// Packets (or a stream's segments) given to the link, up and down.
    pub offered: [u64; 2],
    /// Of those, the ones it dropped.
    pub dropped: [u64; 2],
}

/// A link, shared by the relays that use it and the test that changes it.
/// Clones are handles to the same link.
#[derive(Clone, Debug)]
pub struct Link(Arc<Mutex<State>>);

#[derive(Debug)]
struct State {
    conditions: Conditions,
    blackout_until: Option<Instant>,
    /// The current or next stall.
    stall: Option<Range<Instant>>,
    /// When each direction's queue for a limited rate empties.
    free_at: [Instant; 2],
    /// When each direction last let a packet go, to keep order.
    last: [Instant; 2],
    rng: Rng,
    stats: LinkStats,
}

/// What a link decided for a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fate {
    Deliver(Instant),
    Drop,
}

/// The fixed cost of a datagram's headers, as over IPv6.
const HEADERS: usize = 48;

impl Link {
    /// A link with `conditions`, choosing at random from `seed`.
    #[must_use]
    pub fn new(conditions: Conditions, seed: u64) -> Self {
        let now = Instant::now();
        Self(Arc::new(Mutex::new(State {
            conditions,
            blackout_until: None,
            stall: None,
            free_at: [now; 2],
            last: [now; 2],
            rng: Rng::new(seed),
            stats: LinkStats::default(),
        })))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes what the link does from now on. Packets already on their
    /// way keep their fate.
    pub fn set(&self, conditions: Conditions) {
        let mut state = self.state();
        state.conditions = conditions;
        state.stall = None;
    }

    /// What the link does now.
    #[must_use]
    pub fn conditions(&self) -> Conditions {
        self.state().conditions.clone()
    }

    /// Loses every datagram, and holds every stream, for `duration` from
    /// now, as a network that goes away.
    pub fn black_out(&self, duration: Duration) {
        self.state().blackout_until = Some(Instant::now() + duration);
    }

    /// Whether the link is blacked out.
    #[must_use]
    pub fn blacked_out(&self) -> bool {
        self.state()
            .blackout_until
            .is_some_and(|until| Instant::now() < until)
    }

    /// How many packets the link has passed and dropped.
    #[must_use]
    pub fn stats(&self) -> LinkStats {
        self.state().stats
    }

    /// Decides what becomes of a datagram of `size` bytes, or with
    /// `stream`, a stream's segment, which is never lost but waits instead.
    pub(crate) fn fate(&self, direction: Direction, size: usize, stream: bool) -> Fate {
        let now = Instant::now();
        let mut state = self.state();
        let fate = state.fate(now, direction, size, stream);
        let at = direction.index();
        state.stats.offered[at] += 1;
        if fate == Fate::Drop {
            state.stats.dropped[at] += 1;
        }
        fate
    }

    /// The largest segment a stream should be cut into, going `direction`.
    pub(crate) fn segment(&self, direction: Direction) -> usize {
        let state = self.state();
        let mtu = state.conditions.get(direction).mtu;
        mtu.map_or(16 * 1024, |mtu| mtu.saturating_sub(HEADERS).max(1))
    }
}

impl State {
    fn fate(&mut self, now: Instant, direction: Direction, size: usize, stream: bool) -> Fate {
        let at = direction.index();
        let impairment = self.conditions.get(direction).clone();
        let mut earliest = now;
        if let Some(until) = self.blackout_until {
            if now < until && !stream {
                return Fate::Drop;
            }
            earliest = earliest.max(until);
        }
        if !stream {
            if impairment.mtu.is_some_and(|mtu| size + HEADERS > mtu) {
                return Fate::Drop;
            }
            if self.rng.chance(impairment.loss) {
                return Fate::Drop;
            }
        }
        // Queued behind what's already going, at the link's rate.
        let mut sent = earliest;
        if let Some(rate) = impairment.rate {
            let start = self.free_at[at].max(earliest);
            if !stream && start.saturating_duration_since(now) > impairment.queue {
                return Fate::Drop;
            }
            let bytes = u64::try_from(size + HEADERS).unwrap_or(u64::MAX);
            let micros = bytes.saturating_mul(1_000_000) / rate.max(1);
            sent = start + Duration::from_micros(micros);
            self.free_at[at] = sent;
        }
        let mut arrives = sent + impairment.delay + self.rng.up_to(impairment.jitter);
        if self.rng.chance(impairment.reorder) {
            arrives += impairment.delay.max(Duration::from_millis(1));
        }
        if let Some(stall) = self.stall_at(now)
            && (stall.contains(&arrives) || stall.contains(&now))
        {
            arrives = arrives.max(stall.end);
        }
        if impairment.in_order || stream {
            arrives = arrives.max(self.last[at]);
        }
        self.last[at] = self.last[at].max(arrives);
        Fate::Deliver(arrives)
    }

    /// The current or next stall, as of `now`, if the link stalls.
    fn stall_at(&mut self, now: Instant) -> Option<Range<Instant>> {
        let stalls = self.conditions.stalls.clone()?;
        match &self.stall {
            Some(stall) if now < stall.end => {}
            _ => {
                let start = now + self.rng.within(&stalls.every);
                let end = start + self.rng.within(&stalls.lasting);
                self.stall = Some(start..end);
            }
        }
        self.stall.clone()
    }
}

/// A small, seeded generator of random numbers (xorshift64*): good enough
/// to decide packets' fates, and the same every time for a seed.
#[derive(Debug)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Never zero, which xorshift would stay at.
        Self(seed ^ 0x9e37_79b9_7f4a_7c15 | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number from 0 up to, but not including, 1.
    fn unit(&mut self) -> f64 {
        // 53 bits, all a double holds exactly.
        #[allow(clippy::cast_precision_loss, reason = "53 bits fit a double exactly")]
        let fraction = (self.next() >> 11) as f64 / (1_u64 << 53) as f64;
        fraction
    }

    fn chance(&mut self, probability: f64) -> bool {
        probability > 0.0 && self.unit() < probability
    }

    fn up_to(&mut self, most: Duration) -> Duration {
        most.mul_f64(self.unit())
    }

    fn within(&mut self, range: &Range<Duration>) -> Duration {
        range.start + self.up_to(range.end.saturating_sub(range.start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivered(fate: Fate) -> Instant {
        match fate {
            Fate::Deliver(at) => at,
            Fate::Drop => panic!("dropped"),
        }
    }

    #[test]
    fn a_clean_link_delivers_everything_at_once() {
        let link = Link::new(Conditions::clean(), 1);
        let before = Instant::now();
        let at = delivered(link.fate(Direction::Up, 1000, false));
        assert!(at.duration_since(before) < Duration::from_millis(5));
        assert_eq!(link.stats().dropped, [0, 0]);
    }

    #[test]
    fn loss_drops_about_its_share_of_datagrams_and_none_of_a_stream() {
        let link = Link::new(Conditions::lossy(0.3, Duration::ZERO, Duration::ZERO), 2);
        let dropped = (0..10_000)
            .filter(|_| link.fate(Direction::Down, 100, false) == Fate::Drop)
            .count();
        assert!((2700..3300).contains(&dropped), "{dropped}");
        assert!((0..100).all(|_| link.fate(Direction::Down, 100, true) != Fate::Drop));
    }

    #[test]
    fn datagrams_too_big_for_the_mtu_are_dropped() {
        let link = Link::new(Conditions::clean().with_mtu(1280), 3);
        assert_ne!(link.fate(Direction::Up, 1232, false), Fate::Drop);
        assert_eq!(link.fate(Direction::Up, 1233, false), Fate::Drop);
        assert_eq!(link.segment(Direction::Up), 1232);
    }

    #[test]
    fn a_blackout_drops_datagrams_and_holds_streams_until_it_ends() {
        let link = Link::new(Conditions::clean(), 4);
        link.black_out(Duration::from_secs(5));
        assert!(link.blacked_out());
        assert_eq!(link.fate(Direction::Up, 10, false), Fate::Drop);
        let held = delivered(link.fate(Direction::Up, 10, true));
        assert!(held >= Instant::now() + Duration::from_secs(4));
    }

    #[test]
    fn a_limited_rate_queues_packets_and_drops_past_the_queue() {
        let link = Link::new(
            Conditions::both(Impairment {
                rate: Some(100_000),
                queue: Duration::from_millis(100),
                ..Impairment::default()
            }),
            5,
        );
        // 952 + 48 bytes at 100 kB/s: 10 ms each.
        let first = delivered(link.fate(Direction::Up, 952, false));
        let second = delivered(link.fate(Direction::Up, 952, false));
        let gap = second.duration_since(first);
        assert!(gap >= Duration::from_millis(9) && gap <= Duration::from_millis(11));
        let dropped = (0..20)
            .filter(|_| link.fate(Direction::Up, 952, false) == Fate::Drop)
            .count();
        assert!(dropped > 0, "the queue never filled");
    }

    #[test]
    fn in_order_links_never_let_a_packet_overtake() {
        let mut conditions = Conditions::derp();
        conditions.down.rate = None;
        let link = Link::new(conditions, 6);
        let mut last = Instant::now();
        for _ in 0..1000 {
            let at = delivered(link.fate(Direction::Down, 100, false));
            assert!(at >= last);
            last = at;
        }
    }
}
