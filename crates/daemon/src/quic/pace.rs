//! How fast forwarded connections may write into a QUIC connection, so a
//! download through a forward never fills a slow link's queue ahead of
//! keys and the screens they change.
//!
//! Priorities order what QUIC sends, but not what waits in a router's
//! queue once it's sent. A forward writing as fast as QUIC takes it has
//! the congestion controller fill that queue, and every key's echo then
//! waits in it: on a 20 Mbit/s link with 20 ms each way, echoes went from
//! 41 ms to 141 ms. So each end holds back how fast it writes forwarded
//! bytes into its connection, as background traffic does (LEDBAT, RFC
//! 6817). It watches the connection's round trip, which keys and views
//! share with the forwards and so see the queue they build, and keeps it
//! no more than [`TARGET`] above what it is without them. Each end
//! throttles what it sends, as only the sender's round trip sees the
//! queue its bytes are in: the daemon throttles downloads, and the window
//! uploads.
//!
//! The rate is a window of bytes for each round trip, rather than bytes a
//! second: as a queue grows, the round trip does, and the rate falls at
//! once, before the window has had time to shrink.
//!
//! The round trip without the forwards' queue is measured every
//! [`SLOWDOWN_EVERY`], by writing at half the rate for a moment: the queue
//! drains, and the round trip at the end of it is what's left. Measured
//! again and again, it follows a path that got longer, and never creeps
//! up with the queue the forwards themselves keep. A path that changes, as
//! when the connection moves between UDP and ssh, is measured again at
//! once.
//!
//! The two ends take turns, half a stretch apart, on a schedule counted
//! from when the connection opened. Each then measures with the other's
//! queue in place, takes it for part of the path, and keeps only its own
//! short: a download's acknowledgements wait in an upload's queue, so had
//! they measured together, each would have had to keep both queues short
//! together, and on a link whose upload is a tenth of its download, the
//! download was held to the upload's speed. Taking turns, each direction
//! gets its own link, and a key's echo, which crosses both queues, waits
//! up to twice [`TARGET`] while forwards go both ways.
//!
//! Three things keep this from starving forwards where the round trip
//! wanders on its own, as on bad Wi-Fi:
//!
//! - **It's QUIC's smoothed round trip**, not its samples: jitter of
//!   ±30 ms puts single samples 30 ms under the middle, which would read
//!   as a 30 ms queue that never drains.
//! - **It's measured while forwards still write**, at half the rate,
//!   rather than with the link idle, and the latest measured is the one
//!   used. A link whose packets are held up more the closer they come, as
//!   Wi-Fi's are, has a longer round trip while busy with no queue at all,
//!   and the least it ever had, when idle, would read as a queue the
//!   forwards could never drain. Each measurement, at half a rate the last
//!   allowed, finds the link busier and slower than the last did, and
//!   allows more; slowdowns come every [`FIRST_SLOWDOWN`] until one
//!   finds about what the last did, with the window no longer growing.
//!   Each may raise it by no
//!   more than [`TARGET`], so a stall during one can't hold forwards back
//!   for a stretch. Even so, such a link holds forwards under what it
//!   carries: what keys wait for there can't be told from a queue.
//! - **It never goes below [`FLOOR`]** a second.
//!
//! Nothing here runs unless a forward writes, and it wants a timer only
//! while a forward has bytes waiting that it may not write yet.

use std::time::{Duration, Instant};

use super::PACKET_SIZE;

/// How far forwards may push the round trip above what it is without
/// them: about what each key waits in the queue while a forward
/// downloads.
const TARGET: Duration = Duration::from_millis(15);

/// How much the window shrinks each round trip at most, as a fraction
/// (1 / this) of itself, and grows: by as much of itself, up to
/// [`SHARED_FROM`], and past it, of what it would be halfway to it
/// (their geometric mean). On a link with 20 ms each way, it grows from
/// the start to fill 20 Mbit/s in about a second. Faster, it overshoots,
/// as the round trip it goes by lags a round trip behind, and smoothed
/// more: a quarter had keys waiting 20 to 45 ms rather than 15 to 30.
const GAIN: i128 = 8;

/// The window past which it grows by less than its share of itself, so
/// two connections' forwards feeling the same queue come to share it:
/// high enough that a fast, long link still fills in a second or two.
const SHARED_FROM: u64 = 1024 * 1024;

/// The least forwards may write a second, however the round trip goes.
const FLOOR: u64 = 64 * 1024;

/// The window forwards start with, in bytes each round trip: a fifth of
/// a 2 Mbit/s uplink's at 20 ms each way. Starting at 64 KiB filled such
/// a link's queue at once, and held a download the other way back for
/// seconds after.
const START_WINDOW: u64 = 8 * 1024;

/// A round trip shorter than this is to this machine or the next, as
/// through `clackatoo bridge` on one machine, whose links carry far more
/// than any uplink: forwards there start at [`NEAR_START`] instead.
const NEAR: Duration = Duration::from_millis(1);

/// How fast forwards start, in bytes a second, over a round trip shorter
/// than [`NEAR`]. At [`START_WINDOW`], a download through the bridge, at
/// 40 µs a round trip, was held to 215 MB/s while the window grew.
const NEAR_START: u64 = 1 << 30;

/// The largest window, far beyond any link.
const MOST_WINDOW: u64 = 1 << 30;

/// How often the round trip without the forwards' queue is measured,
/// counted from [`FIRST_SLOWDOWN`] after the connection opened.
const SLOWDOWN_EVERY: Duration = Duration::from_secs(10);

/// When it may first be measured, after the connection opened, and how
/// often until it has settled; and how long after forwards start, or start
/// again after a pause, at the least, so the first measures them going
/// rather than the idle link.
const FIRST_SLOWDOWN: Duration = Duration::from_secs(2);

/// How much later than the daemon's the window's slowdowns come: half a
/// stretch, and an odd number of [`FIRST_SLOWDOWN`]s past it, so the two
/// ends never slow down at once.
const WINDOW_LAG: Duration = Duration::from_secs(5);

/// The longest a slowdown lasts, however long the round trip: so a link
/// whose queue has grown to seconds doesn't sit at half the floor, and
/// so one end's never runs into the other's, a second later.
const LONGEST_SLOWDOWN: Duration = Duration::from_millis(750);

/// How long each slowdown lasts at least: long enough for a queue at
/// [`TARGET`] and more to drain at half the rate in its first half, and
/// for the smoothed round trip to follow, to be measured in its second.
const SLOWDOWN_FOR: Duration = Duration::from_millis(300);

/// How long a burst may be at the rate allowed. QUIC sends what it's
/// given in bursts of its own, so on a slow link a burst is a queue: 16
/// KiB at once was 64 ms of a 2 Mbit/s uplink, which keys waited behind.
const BURST: Duration = Duration::from_millis(5);

/// What a forward held back waits to be allowed before trying again, at
/// the least, and the least burst: two packets. At a high rate it waits
/// for half a burst, so it's woken no more than a few hundred times a
/// second.
const WAKE_FOR: u64 = 2 * PACKET_SIZE as u64;

/// How long between looks at the round trip, at the least.
const LEAST_ROUND: Duration = Duration::from_millis(1);

const MICROS: u128 = 1_000_000;

/// Which end of a connection a [`Pace`] is at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// The daemon's, pacing downloads.
    Daemon,
    /// The window's, pacing uploads.
    Window,
}

/// A moment of writing at half the rate, to measure the round trip
/// without the forwards' queue.
#[derive(Debug)]
struct Slowdown {
    /// When the queue has drained, and the round trip settled.
    settled: Instant,
    until: Instant,
    /// The least round trip once settled.
    least: Duration,
}

/// How fast one QUIC connection's forwards may write into it.
#[derive(Debug)]
pub(crate) struct Pace {
    /// When the first of this end's slowdowns could be, counted from when
    /// the connection opened.
    first: Instant,
    /// Bytes allowed each round trip.
    window: u64,
    /// Bytes that may be written now.
    allowed: u64,
    /// When `allowed` was last brought up to date, and the round trip
    /// then.
    filled_at: Instant,
    rtt: Duration,
    /// When the window was last changed, or looked at.
    adjusted_at: Instant,
    /// Whether a forward had more to write than allowed since then: the
    /// window grows only while it's what holds forwards back.
    limited: bool,
    /// When one last had, if one has: a slowdown is only worth it while
    /// forwards are held back, and so may have a queue to drain.
    limited_at: Option<Instant>,
    /// Whether a forward is waiting to be allowed more.
    waiting: bool,
    /// The round trip without the forwards' queue, as the last slowdown
    /// measured it, or as it was when forwards started.
    base: Duration,
    /// Whether the last slowdown found about what the one before did,
    /// with the window about as it was then: until one has, they come
    /// every [`FIRST_SLOWDOWN`]. One measuring while the window is still
    /// growing, as at the start, measures a link less busy than it will
    /// be.
    settled: bool,
    /// The window when the last slowdown measured.
    window_measured: u64,
    /// When the next slowdown begins.
    slowdown_at: Instant,
    /// The one going on.
    slowdown: Option<Slowdown>,
}

impl Pace {
    /// Pace for the `end` of a connection opened at `opened`, whose
    /// smoothed round trip is `rtt` now, as its first forward starts.
    pub(crate) fn new(now: Instant, rtt: Duration, opened: Instant, end: End) -> Self {
        let lag = match end {
            End::Daemon => Duration::ZERO,
            End::Window => WINDOW_LAG,
        };
        let first = opened + FIRST_SLOWDOWN + lag;
        Self {
            first,
            window: if rtt < NEAR {
                START_WINDOW.max(per(NEAR_START, rtt))
            } else {
                START_WINDOW
            },
            allowed: WAKE_FOR,
            filled_at: now,
            rtt,
            adjusted_at: now,
            limited: false,
            limited_at: None,
            waiting: false,
            base: rtt,
            settled: false,
            window_measured: 0,
            slowdown_at: next_slowdown(first, now + FIRST_SLOWDOWN, FIRST_SLOWDOWN),
            slowdown: None,
        }
    }

    /// Brings what forwards may write up to `now`, with the connection's
    /// smoothed round trip at `rtt`.
    pub(crate) fn refresh(&mut self, now: Instant, rtt: Duration) {
        // Not written through for longer than a stretch, the round trip
        // has nothing of the forwards' in it.
        if now > self.filled_at + SLOWDOWN_EVERY {
            self.start_again(now, rtt);
        }
        self.adjust(now, rtt);
        let rate = self.rate(now, rtt);
        let since = now.saturating_duration_since(self.filled_at);
        let earned = u128::from(rate) * since.as_micros() / MICROS;
        let burst = WAKE_FOR.max(per(rate, BURST));
        self.allowed = u64::try_from((u128::from(self.allowed) + earned).min(u128::from(burst)))
            .unwrap_or(burst);
        self.filled_at = now;
        self.rtt = rtt;
    }

    /// The connection's path changed, as when it moved between UDP and
    /// ssh, and its round trip is `rtt` now: the one without the forwards'
    /// queue is measured again, at this end's next slot, which comes
    /// within [`FIRST_SLOWDOWN`]. Not at once: both ends see a change of
    /// route, and would measure together.
    pub(crate) fn path_changed(&mut self, now: Instant, rtt: Duration) {
        self.base = rtt;
        self.settled = false;
        self.slowdown = None;
        self.slowdown_at = self.next_slowdown(now);
    }

    /// How much of `wanted` bytes a forward may write now.
    pub(crate) fn most(&self, wanted: usize) -> usize {
        wanted.min(usize::try_from(self.allowed).unwrap_or(usize::MAX))
    }

    /// Notes that a forward wrote `written` of the `wanted` bytes it had
    /// waiting. If it wrote all it was allowed and wanted more, it waits;
    /// if QUIC took less, QUIC says when it has room.
    pub(crate) fn wrote(&mut self, written: usize, wanted: usize) {
        let allowed = self.most(wanted);
        self.allowed = (self.allowed).saturating_sub(u64::try_from(written).unwrap_or(u64::MAX));
        if allowed < wanted && written >= allowed {
            self.limited = true;
            self.limited_at = Some(self.filled_at);
            self.waiting = true;
        }
    }

    /// When forwards held back may write again, if any are.
    pub(crate) fn due(&self) -> Option<Instant> {
        if !self.waiting {
            return None;
        }
        let rate = self.rate(self.filled_at, self.rtt);
        let wanted = WAKE_FOR.max(per(rate, BURST) / 2);
        let short = wanted.saturating_sub(self.allowed);
        let micros = (u128::from(short) * MICROS).div_ceil(u128::from(rate.max(1)));
        let wait = Duration::from_micros(u64::try_from(micros).unwrap_or(u64::MAX));
        Some(self.filled_at + wait)
    }

    /// Forwards held back have been let go, to try again.
    pub(crate) fn woken(&mut self) {
        self.waiting = false;
    }

    /// The bytes a second allowed as of `now`, with the round trip at
    /// `rtt`.
    fn rate(&self, now: Instant, rtt: Duration) -> u64 {
        let micros = rtt.as_micros().max(1);
        let rate = u64::try_from(u128::from(self.window) * MICROS / micros)
            .unwrap_or(u64::MAX)
            .max(FLOOR);
        let slowed = (self.slowdown.as_ref()).is_some_and(|slowdown| now < slowdown.until);
        if slowed { rate / 2 } else { rate }
    }

    /// Grows or shrinks the window, once a round trip, by how far the
    /// round trip `rtt` is from [`TARGET`] above the round trip without
    /// the forwards' queue; and starts and ends slowdowns.
    fn adjust(&mut self, now: Instant, rtt: Duration) {
        self.slow_down(now, rtt);
        if now < self.adjusted_at + rtt.max(LEAST_ROUND) {
            return;
        }
        self.adjusted_at = now;
        let limited = std::mem::take(&mut self.limited);
        if self.slowdown.is_some() {
            return;
        }
        let queue = rtt.saturating_sub(self.base);
        let target = i128::try_from(TARGET.as_micros()).unwrap_or(i128::MAX);
        // How far under the target the queue is, from -1 to 1 of it.
        let queue = i128::try_from(queue.as_micros()).unwrap_or(i128::MAX);
        let under = target - queue.min(2 * target);
        // Grown only while it's what holds forwards back: otherwise it
        // would grow without end while a forward has little to send, to
        // flood the link once it has more.
        if under > 0 && !limited {
            return;
        }
        // Shrunk by its share of itself, but grown, past [`SHARED_FROM`],
        // by less than its share as it's larger: two connections' forwards
        // feeling the same queue come to share it, where growing and
        // shrinking alike would keep whatever split they started with.
        // Below it, growing by more than its share overshot on slow links.
        let step = if under > 0 {
            i128::from((self.window * SHARED_FROM).isqrt().min(self.window))
        } else {
            i128::from(self.window)
        };
        let window = i128::from(self.window) + step * under / (target * GAIN);
        let least = i128::from(per(FLOOR, rtt));
        self.window =
            u64::try_from(window.clamp(least, i128::from(MOST_WINDOW))).unwrap_or(MOST_WINDOW);
    }

    /// Starts a slowdown when it's time, measures the round trip `rtt`
    /// during one, and ends it.
    fn slow_down(&mut self, now: Instant, rtt: Duration) {
        match &mut self.slowdown {
            // Forwards stopped writing during it, and it measured nothing
            // they'd queued.
            Some(slowdown) if now > slowdown.until + SLOWDOWN_FOR => {
                self.slowdown = None;
                self.slowdown_at = self.next_slowdown(now);
            }
            Some(slowdown) if now >= slowdown.until => {
                let measured = slowdown.least.min(rtt);
                self.settled = measured.abs_diff(self.base) < TARGET / 2
                    && self.window < self.window_measured + self.window_measured / 4;
                self.window_measured = self.window;
                // Up by no more than the target at a time, as a stall
                // during it reads as a longer path; down at once.
                self.base = measured.min(self.base + TARGET);
                self.slowdown = None;
                self.slowdown_at = self.next_slowdown(now);
            }
            Some(slowdown) if now >= slowdown.settled => {
                slowdown.least = slowdown.least.min(rtt);
            }
            Some(slowdown) => slowdown.least = rtt,
            // Only while forwards are held back: with nothing queued
            // there's nothing to measure past.
            None if now >= self.slowdown_at => {
                let held = (self.limited_at).is_some_and(|at| now < at + rtt.max(LEAST_ROUND));
                if held {
                    let lasting = SLOWDOWN_FOR.max(rtt * 2).min(LONGEST_SLOWDOWN);
                    self.slowdown = Some(Slowdown {
                        settled: now + lasting / 2,
                        until: now + lasting,
                        least: rtt,
                    });
                } else {
                    self.slowdown_at = self.next_slowdown(now + LEAST_ROUND);
                }
            }
            None => {}
        }
    }

    /// When the next slowdown at or after `after` begins: every
    /// [`SLOWDOWN_EVERY`], or every [`FIRST_SLOWDOWN`] until the round
    /// trip they measure has settled.
    fn next_slowdown(&self, after: Instant) -> Instant {
        let every = if self.settled {
            SLOWDOWN_EVERY
        } else {
            FIRST_SLOWDOWN
        };
        next_slowdown(self.first, after, every)
    }

    /// Measures from the start, as forwards start again after a pause,
    /// with the round trip at `rtt`.
    fn start_again(&mut self, now: Instant, rtt: Duration) {
        self.base = rtt;
        self.settled = false;
        self.slowdown = None;
        self.slowdown_at = self.next_slowdown(now + FIRST_SLOWDOWN);
    }
}

/// When the first slowdown at or after `after` begins, with one every
/// `every` from `first`. As [`SLOWDOWN_EVERY`] is a multiple of
/// [`FIRST_SLOWDOWN`], and [`WINDOW_LAG`] an odd multiple of it, the
/// window's slowdowns never meet the daemon's, settled or not.
fn next_slowdown(first: Instant, after: Instant, every: Duration) -> Instant {
    let since = after.saturating_duration_since(first).as_micros();
    let stretches = u32::try_from(since.div_ceil(every.as_micros())).unwrap_or(u32::MAX);
    first + every * stretches
}

/// How many bytes go in `time` at `rate` bytes a second.
fn per(rate: u64, time: Duration) -> u64 {
    u64::try_from(u128::from(rate) * time.as_micros() / MICROS).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RTT: Duration = Duration::from_millis(40);

    /// A link of 2.5 MB/s with [`RTT`], with a queue of whatever is sent
    /// beyond that, which drains at its rate.
    struct Link {
        queue: f64,
        sent_at: Instant,
    }

    impl Link {
        const RATE: f64 = 2_500_000.0;

        fn new(now: Instant) -> Self {
            Self {
                queue: 0.0,
                sent_at: now,
            }
        }

        /// The round trip with what's queued.
        fn rtt(&self) -> Duration {
            RTT + Duration::from_secs_f64(self.queue / Self::RATE)
        }

        /// Sends `bytes` at `now`, after what has drained since last time.
        #[allow(clippy::cast_precision_loss, reason = "counts far below 2^52")]
        fn send(&mut self, now: Instant, bytes: usize) {
            let drained = (now - self.sent_at).as_secs_f64() * Self::RATE;
            self.queue = (self.queue - drained).max(0.0) + bytes as f64;
            self.sent_at = now;
        }
    }

    /// Writes as much as `pace` allows every millisecond for `time` over
    /// `link`. Returns how much was written.
    fn run_over(pace: &mut Pace, now: &mut Instant, time: Duration, link: &mut Link) -> u64 {
        let until = *now + time;
        let mut written = 0;
        while *now < until {
            *now += Duration::from_millis(1);
            pace.refresh(*now, link.rtt());
            let most = pace.most(1 << 20);
            pace.wrote(most, 1 << 20);
            link.send(*now, most);
            written += most as u64;
        }
        written
    }

    /// Writes as much as `pace` allows every millisecond for `time`, with
    /// the round trip at `rtt(now)`. Returns how much was written.
    fn run(
        pace: &mut Pace,
        now: &mut Instant,
        time: Duration,
        mut rtt: impl FnMut(Instant) -> Duration,
    ) -> u64 {
        let until = *now + time;
        let mut written = 0;
        while *now < until {
            *now += Duration::from_millis(1);
            pace.refresh(*now, rtt(*now));
            let wanted = 1 << 20;
            let most = pace.most(wanted);
            pace.wrote(most, wanted);
            written += most as u64;
        }
        written
    }

    #[test]
    fn with_no_queue_the_window_grows_only_while_it_holds_forwards_back() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now, RTT, now, End::Daemon);
        run(&mut pace, &mut now, Duration::from_secs(1), |_| RTT);
        assert!(
            pace.window > 4 * START_WINDOW,
            "grew only to {}",
            pace.window
        );
        // A forward with little to send never fills the window, so it
        // stays as it was, once the round trip it was filled in is over.
        let little = |pace: &mut Pace, now: &mut Instant| {
            for _ in 0..1000 {
                *now += Duration::from_millis(1);
                pace.refresh(*now, RTT);
                pace.wrote(100, 100);
            }
        };
        pace.woken();
        little(&mut pace, &mut now);
        let grown = pace.window;
        little(&mut pace, &mut now);
        assert_eq!(pace.window, grown);
        assert_eq!(pace.due(), None);
    }

    #[test]
    fn a_growing_queue_slows_forwards_down_to_the_floor_at_most() {
        // A short path with a queue three times the target that never
        // drains, until the first slowdown measures it.
        let short = Duration::from_millis(5);
        let mut now = Instant::now();
        let mut pace = Pace::new(now, short, now, End::Daemon);
        let queued = short + 3 * TARGET;
        run(&mut pace, &mut now, Duration::from_millis(1500), |_| queued);
        let written = run(&mut pace, &mut now, Duration::from_millis(400), |_| queued);
        assert!(
            (FLOOR * 4 / 10 * 9 / 10..FLOOR * 4 / 10 * 2).contains(&written),
            "wrote {written} B in 400 ms"
        );
    }

    #[test]
    #[allow(clippy::cast_precision_loss, reason = "counts far below 2^52")]
    fn the_rate_settles_where_the_queue_is_at_the_target() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now, RTT, now, End::Daemon);
        let mut link = Link::new(now);
        let mut queues = Vec::new();
        let mut written = 0;
        for _ in 0..20_000 {
            written += run_over(&mut pace, &mut now, Duration::from_millis(1), &mut link);
            queues.push(link.rtt().saturating_sub(RTT));
        }
        // Past the start, the queue stays near the target, and the link
        // stays busy.
        let late = &queues[5000..];
        let mean = late.iter().sum::<Duration>() / u32::try_from(late.len()).unwrap();
        assert!(
            (Duration::from_millis(3)..Duration::from_millis(30)).contains(&mean),
            "the queue held {mean:?} on average"
        );
        let rate = written as f64 / 20.0;
        assert!(rate > Link::RATE * 0.8, "only {rate} B/s");
    }

    #[test]
    fn a_round_trip_that_wanders_without_a_queue_doesnt_starve_forwards() {
        // The smoothed round trip of a link with ±30 ms jitter, wandering
        // ±5 ms with nothing queued.
        let mut now = Instant::now();
        let start = now;
        let mut pace = Pace::new(now, RTT, now, End::Daemon);
        let wander = |at: Instant| {
            let phase = (at - start).as_secs_f64() * 7.0;
            RTT + Duration::from_secs_f64(0.005 * (1.0 + phase.sin()))
        };
        run(&mut pace, &mut now, Duration::from_secs(2), wander);
        let written = run(&mut pace, &mut now, Duration::from_secs(5), wander);
        assert!(written > 5 * 2_000_000, "wrote only {written} B in 5 s");
    }

    #[test]
    fn a_link_slower_to_answer_while_busy_doesnt_starve_forwards() {
        // A link that holds packets up to 30 ms more the more closely they
        // come, queue or no queue, as Wi-Fi does, up to 300 kB/s: past the
        // target from about 150 kB/s, so a slowdown measuring at half a
        // rate the target allows never sees all of it. Each one sees more
        // than the last, and allows more, until forwards go fast.
        let mut now = Instant::now();
        let mut pace = Pace::new(now, RTT, now, End::Daemon);
        // Bytes a second, lately.
        let mut rate = 0_u64;
        let mut written = 0;
        for at in 0..30_000 {
            now += Duration::from_millis(1);
            let held = 30_000 * rate.min(300_000) / 300_000;
            pace.refresh(now, RTT + Duration::from_micros(held));
            let most = pace.most(1 << 20);
            pace.wrote(most, 1 << 20);
            rate = (rate * 15 + most as u64 * 1000) / 16;
            if at >= 25_000 {
                written += most;
            }
        }
        let rate = written / 5;
        assert!(rate > 1_000_000, "only {rate} B/s");
    }

    #[test]
    fn a_path_that_got_longer_is_learned_again_by_the_slowdowns() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now, RTT, now, End::Daemon);
        run(&mut pace, &mut now, Duration::from_secs(1), |_| RTT);
        // The connection moves to a path 60 ms longer, with no queue: at
        // first that reads as a queue, but once two slowdowns have
        // measured it the forwards go fast again.
        let longer = RTT + Duration::from_millis(60);
        run(&mut pace, &mut now, 2 * SLOWDOWN_EVERY, |_| longer);
        let written = run(&mut pace, &mut now, Duration::from_secs(2), |_| longer);
        assert!(written > 2 * 1_000_000, "wrote only {written} B in 2 s");
    }

    #[test]
    #[allow(clippy::cast_precision_loss, reason = "counts far below 2^52")]
    fn a_forward_held_back_wakes_when_due_and_goes_on_as_the_rate_changes() {
        // Forwards with plenty to send, parked whenever held back and
        // woken only when due, as the event loops do, over a link whose
        // round trip grows with what's queued.
        let start = Instant::now();
        let mut now = start;
        let mut pace = Pace::new(now, RTT, now, End::Daemon);
        let mut link = Link::new(now);
        let (mut wakes, mut written) = (0_u64, 0_u64);
        while now < start + Duration::from_secs(20) {
            pace.refresh(now, link.rtt());
            let most = pace.most(1 << 20);
            pace.wrote(most, 1 << 20);
            link.send(now, most);
            written += most as u64;
            // Held back, it sleeps until due, never later than now, and
            // never for nothing.
            let due = pace.due().expect("held back with plenty to send");
            assert!(due > now, "due at once: it would spin");
            now = due;
            pace.woken();
            wakes += 1;
        }
        let rate = written as f64 / 20.0;
        assert!(rate > Link::RATE * 0.8, "only {rate} B/s");
        // Woken for half a burst at a time, not a packet.
        assert!(wakes < 20 * 1000, "woken {wakes} times in 20 s");
    }

    #[test]
    fn the_two_ends_of_a_connection_take_turns_to_slow_down() {
        let opened = Instant::now();
        // The two ends' forwards start a moment apart, well after the
        // connection opened, each over a link of its own.
        let mut starts = Vec::new();
        for (end, after) in [(End::Daemon, 5300), (End::Window, 5340)] {
            let mut now = opened + Duration::from_millis(after);
            let mut pace = Pace::new(now, RTT, opened, end);
            let mut link = Link::new(now);
            let mut began = Vec::new();
            while now < opened + Duration::from_secs(40) {
                let slowed = pace.slowdown.is_some();
                run_over(&mut pace, &mut now, Duration::from_millis(1), &mut link);
                if !slowed && pace.slowdown.is_some() {
                    began.push(now);
                }
            }
            assert!(pace.settled);
            assert!(
                began.len() >= 3,
                "{end:?} slowed down {} times",
                began.len()
            );
            starts.push(began);
        }
        // Never together: each measures with the other's queue in place.
        for daemon in &starts[0] {
            for window in &starts[1] {
                let apart = daemon.max(window).duration_since(*daemon.min(window));
                assert!(apart >= Duration::from_millis(900), "{apart:?} apart");
            }
        }
    }

    /// Writes over `link` until a slowdown begins, within `within`.
    fn until_slowed(pace: &mut Pace, now: &mut Instant, link: &mut Link, within: Duration) {
        let from = *now;
        while pace.slowdown.is_none() {
            run_over(pace, now, Duration::from_millis(1), link);
            assert!(*now < from + within, "no slowdown");
        }
    }

    #[test]
    fn a_stall_during_a_slowdown_raises_the_base_by_the_target_at_most() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now, RTT, now, End::Daemon);
        let mut link = Link::new(now);
        until_slowed(&mut pace, &mut now, &mut link, 3 * FIRST_SLOWDOWN);
        // Everything held up half a second while it measures.
        let stalled = RTT + Duration::from_millis(500);
        run(&mut pace, &mut now, SLOWDOWN_FOR * 2, |_| stalled);
        assert!(pace.slowdown.is_none());
        assert!(pace.base <= RTT + TARGET, "{:?}", pace.base);
        // Back to normal, the next one brings it back down at once.
        until_slowed(&mut pace, &mut now, &mut link, 3 * FIRST_SLOWDOWN);
        run_over(&mut pace, &mut now, SLOWDOWN_FOR * 2, &mut link);
        assert!(
            pace.base < RTT + Duration::from_millis(5),
            "{:?}",
            pace.base
        );
    }

    #[test]
    fn a_slowdown_lasts_a_second_at_most() {
        let long = Duration::from_secs(3);
        let mut now = Instant::now();
        let mut pace = Pace::new(now, long, now, End::Daemon);
        while pace.slowdown.is_none() {
            run(&mut pace, &mut now, Duration::from_millis(1), |_| long);
            assert!(now < pace.filled_at + SLOWDOWN_EVERY, "no slowdown");
        }
        let lasting = pace.slowdown.as_ref().unwrap().until.duration_since(now);
        assert!(lasting <= LONGEST_SLOWDOWN, "{lasting:?}");
    }

    #[test]
    fn a_changed_path_is_measured_at_the_next_slot() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now, RTT, now, End::Daemon);
        let mut link = Link::new(now);
        run_over(&mut pace, &mut now, Duration::from_secs(15), &mut link);
        assert!(pace.settled);
        // Onto a path 60 ms longer, with no queue: measured within one
        // of the first slots, not a stretch.
        let longer = RTT + Duration::from_millis(60);
        pace.path_changed(now, longer);
        assert!(pace.slowdown_at <= now + FIRST_SLOWDOWN);
        run(
            &mut pace,
            &mut now,
            FIRST_SLOWDOWN + LONGEST_SLOWDOWN,
            |_| longer,
        );
        assert_eq!(pace.base, longer);
        let written = run(&mut pace, &mut now, Duration::from_secs(1), |_| longer);
        assert!(written > 1_000_000, "wrote only {written} B in 1 s");
    }

    #[test]
    fn a_slowdown_waits_for_forwards_held_back_and_one_left_stale_is_dropped() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now, RTT, now, End::Daemon);
        // A forward with little to send is never held back: no slowdown.
        for _ in 0..5000 {
            now += Duration::from_millis(1);
            pace.refresh(now, RTT);
            pace.wrote(100, 100);
        }
        assert!(pace.slowdown.is_none());
        assert!(!pace.settled);
        // One that is gets one, at the next slot.
        let held_from = now;
        while pace.slowdown.is_none() {
            run(&mut pace, &mut now, Duration::from_millis(1), |_| RTT);
            assert!(now < held_from + FIRST_SLOWDOWN * 2, "no slowdown");
        }
        // Its forwards stop before it ends, and come back once it would
        // have long ended: what it measured, the idle link, is dropped.
        now += 2 * SLOWDOWN_FOR + Duration::from_millis(1);
        pace.refresh(now, Duration::from_millis(10));
        assert!(pace.slowdown.is_none());
        assert_eq!(pace.base, RTT);
    }

    #[test]
    fn forwards_over_a_round_trip_within_the_machine_start_fast() {
        let now = Instant::now();
        let near = Duration::from_micros(40);
        let pace = Pace::new(now, near, now, End::Daemon);
        assert!(pace.rate(now, near) >= NEAR_START / 2);
        assert_eq!(Pace::new(now, RTT, now, End::Daemon).window, START_WINDOW);
    }

    #[test]
    fn a_forward_held_back_is_woken_once_a_few_packets_are_allowed() {
        let mut now = Instant::now();
        let mut pace = Pace::new(now, RTT, now, End::Daemon);
        assert_eq!(pace.due(), None);
        pace.refresh(now, RTT);
        let most = pace.most(1 << 20);
        pace.wrote(most, 1 << 20);
        let due = pace.due().expect("held back");
        // Two packets at the starting rate, 200 kB/s.
        assert!(
            due > now && due < now + Duration::from_millis(20),
            "{:?}",
            due - now
        );
        pace.woken();
        assert_eq!(pace.due(), None);
        now = due;
        pace.refresh(now, RTT);
        assert!(pace.most(1 << 20) >= usize::try_from(WAKE_FOR).unwrap());
    }
}
