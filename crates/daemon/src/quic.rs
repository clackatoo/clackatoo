//! QUIC, for clients on other machines.
//!
//! A client on another machine reaches the daemon over SSH, through
//! `clackatoo bridge` (see [`remote`](crate::remote)). It asks the daemon
//! to turn that connection into a *carrier* of QUIC packets
//! ([`Request::CarryQuic`](clackatoo_protocol::Request::CarryQuic)), then
//! opens a QUIC connection through it, each packet framed like a message.
//! Its messages go on the connection's first stream, just as they would on
//! the Unix socket, and so do the daemon's answers. Each terminal's view
//! updates come on a stream of their own, so a flood of output in one never
//! holds up another, or the answers.
//!
//! QUIC inside SSH gives every remote client one way in, whether or not UDP
//! gets through to the machine, and streams that don't hold each other up.
//! When UDP does get through, the client moves the connection onto it, to
//! survive its network changing: see [`carry`].
//!
//! SSH is what authenticates. The daemon takes new QUIC connections only
//! from carriers, which only someone logged in over SSH can open, and the
//! client accepts only the certificate the daemon gave it over SSH. Over
//! UDP, the daemon only takes packets for connections already open, which
//! only the keys agreed inside SSH can make.
//!
//! quinn-proto, a QUIC implementation with no networking of its own, is
//! driven by the daemon's event loop ([`QuicServer`]) and, on the client,
//! by a thread of its own ([`carry`]).

mod client;
mod forward;
mod pace;
mod path;
mod server;

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clackatoo_protocol::frame;
use quinn_proto::congestion::BbrConfig;
use quinn_proto::{TransportConfig, VarInt};

pub(crate) use client::{Carried, carry};
pub use client::{Contact, Redial, Redialed};
pub(crate) use pace::{End, Pace};
pub use path::Route;
pub(crate) use server::{Forwarded, QuicEvent, QuicServer, Received};

/// The name the daemon's certificate is for. Nothing checks it: the client
/// pins the certificate itself.
const SERVER_NAME: &str = "clackatoo";

/// How long a connection may go without hearing from the other end before
/// it ends, on either side. Keep-alives every [`KEEP_ALIVE`] stop a link
/// that's merely idle reaching it, and an outage shorter than it is ridden
/// out without a reconnect. Past it, a client gone without a word stops
/// costing the daemon anything, and a window connects again, from fresh
/// snapshots: what it typed into editors meanwhile isn't lost (see
/// [`BufferSnapshot::applied`](clackatoo_protocol::BufferSnapshot)).
const IDLE_TIMEOUT: Duration = Duration::from_mins(2);

/// How often a client lets the daemon know it's there when nothing else is
/// sent: often enough to keep a home router's mapping for UDP open, which
/// some forget after 30 s, and for each end to notice soon when the other
/// goes quiet.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// The size of every packet's UDP payload, and never more. Tunnels such as
/// Tailscale's take IP packets of at most 1280 bytes, which leaves 1232
/// for UDP over IPv6; QUIC can't go below 1200. So packets are never
/// bigger than what any path QUIC can use takes, and nothing has to find
/// out how big they may be. Searching (quinn's DPLPMTUD) would look on a
/// carrier first, where anything fits, and the client's side never starts
/// again when it moves onto UDP, as its QUIC never sees it move: it would
/// go on sending 1452-byte packets into a 1280-byte tunnel, to be dropped,
/// or, as the sockets don't forbid it, cut into two fragments, doubling
/// the chance of losing each on a lossy link. The few percent more each
/// packet could carry isn't worth that.
const PACKET_SIZE: u16 = 1200;

/// How often each end logs how its connection is going, while it's open.
const STATS_EVERY: Duration = Duration::from_mins(5);

/// What `connection`'s numbers say about how it's going, for the log: the
/// round trip, how much may be in flight, how many packets were lost, and
/// the largest packet sent.
pub(crate) fn describe(connection: &quinn_proto::Connection) -> String {
    let stats = connection.stats();
    let path = stats.path;
    // Tenths of a percent, without floating point.
    let lost = (path.lost_packets * 1000)
        .checked_div(path.sent_packets)
        .unwrap_or(0);
    format!(
        "round trip {:?} (least {:?}), window {} B, {} of {} packets lost ({}.{}%), \
         {} congestion events, packets up to {} B, {} received",
        path.rtt,
        path.min_rtt,
        path.cwnd,
        path.lost_packets,
        path.sent_packets,
        lost / 10,
        lost % 10,
        path.congestion_events,
        path.current_mtu,
        stats.udp_rx.datagrams,
    )
}

/// How often a path's loss is measured.
const LOSS_EVERY: Duration = Duration::from_secs(1);

/// Of each thousand packets, how many lost makes a path lossy.
const LOSSY: u64 = 20;

/// How long a path counts as lossy after it last lost that much: long, as
/// sending small packets twice hides the loss it's there for.
const LOSSY_FOR: Duration = Duration::from_mins(1);

/// The largest packet sent twice on a lossy path: keys, their echoes,
/// acknowledgements and pings, which are what typing waits on, and cost
/// next to nothing to send twice.
const SMALL_PACKET: usize = 600;

/// Whether a connection's path over UDP has been losing packets lately, as
/// bad Wi-Fi does, so that its small packets go twice. One lost packet
/// costs QUIC a probe timeout to notice, at best, and it waits twice as
/// long each time the next try is lost too: at 30% loss each way, the
/// typing of the moment would wait seconds, now and then many. Sent twice,
/// a packet is lost only when both copies are (9% at 30%); QUIC drops the
/// copy that arrives second. That's mosh's answer too. Big packets, such
/// as a screen after a flood, go once: they're most of the bytes, and
/// congestion control looks after them.
#[derive(Debug)]
pub(crate) struct Loss {
    /// The connection's counts when last measured.
    sent: u64,
    lost: u64,
    measured_at: Instant,
    /// Until when the path counts as lossy, if it has been.
    lossy_until: Option<Instant>,
}

impl Loss {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            sent: 0,
            lost: 0,
            measured_at: now,
            lossy_until: None,
        }
    }

    /// Measures `connection`'s loss since last time, if it's time.
    pub(crate) fn update(&mut self, connection: &quinn_proto::Connection, now: Instant) {
        if now < self.measured_at + LOSS_EVERY {
            return;
        }
        let path = connection.stats().path;
        // A path the connection moved to counts from nothing: start again.
        if path.sent_packets < self.sent || path.lost_packets < self.lost {
            self.sent = 0;
            self.lost = 0;
        }
        let sent = path.sent_packets.saturating_sub(self.sent);
        // Packets are found lost a while after they're sent, so those
        // found lost now may have been sent before: more can be lost than
        // were sent since last time.
        let lost = path.lost_packets.saturating_sub(self.lost);
        // Too few sent to say: wait for more.
        if sent < 8 {
            return;
        }
        if lost * 1000 >= sent * LOSSY {
            if self.lossy_until.is_none_or(|until| until < now) {
                log::info!(
                    "the path is losing packets ({lost} found lost while {sent} went): small \
                     ones go twice"
                );
            }
            self.lossy_until = Some(now + LOSSY_FOR);
        }
        self.sent = path.sent_packets;
        self.lost = path.lost_packets;
        self.measured_at = now;
    }

    /// Whether a packet of `size` bytes should go twice, as of `now`.
    pub(crate) fn twice(&self, size: usize, now: Instant) -> bool {
        size <= SMALL_PACKET && self.lossy_until.is_some_and(|until| now < until)
    }
}

/// How much of a stream's data either end takes before it has read it,
/// and so how much the other end may have sent on it and not had
/// acknowledged: quinn's own default, 100 ms at 100 Mbit/s, written out
/// for [`send_window`] to count with.
const STREAM_WINDOW: u32 = 1_250_000;

/// How much one end may have sent on all its streams together and not
/// had acknowledged, before writing to any of them waits: eight streams'
/// worth, quinn's own default, while no ports are forwarded.
const SEND_WINDOW: u64 = 8 * STREAM_WINDOW as u64;

/// The most connections a window may forward at once, over all its
/// forwards: a stream each, beside the one it talks over. Each holds at
/// most [`STREAM_WINDOW`] at either end that the other hasn't read.
pub(crate) const MOST_FORWARDED: u32 = 64;

/// What a forwarded connection's streams send with: less than
/// everything else, which sends with 0, so a download through a forward
/// only ever takes what's left after terminals' updates, answers and keys.
pub(crate) const FORWARD_PRIORITY: i32 = -1;

/// The send window for a connection with `forwarded` connections going
/// through it. Each can fill only its own stream's window, so growing the
/// send window by that much for each leaves the rest of it to everything
/// else: forwards never make writing a terminal's update wait.
pub(crate) fn send_window(forwarded: usize) -> u64 {
    let forwarded = u64::try_from(forwarded).unwrap_or(u64::MAX);
    SEND_WINDOW.saturating_add(forwarded.saturating_mul(u64::from(STREAM_WINDOW)))
}

/// How both ends' connections behave.
fn transport() -> TransportConfig {
    let mut transport = TransportConfig::default();
    transport
        .max_idle_timeout(Some(
            IDLE_TIMEOUT
                .try_into()
                .expect("the idle timeout is within QUIC's limits"),
        ))
        // A carrier never loses packets, so a loss-based controller would
        // grow its window without end, queueing behind keystrokes. BBR
        // paces to the measured rate and round trip instead.
        .congestion_controller_factory(Arc::new(BbrConfig::default()))
        // One packet per frame on a carrier.
        .enable_segmentation_offload(false)
        .initial_mtu(PACKET_SIZE)
        .min_mtu(PACKET_SIZE)
        .mtu_discovery_config(None)
        // A stream for each terminal a client follows.
        .max_concurrent_uni_streams(VarInt::from_u32(1024))
        // The one a client talks over, and one for each connection it
        // forwards.
        .max_concurrent_bidi_streams(VarInt::from_u32(1 + MOST_FORWARDED))
        .stream_receive_window(VarInt::from_u32(STREAM_WINDOW))
        .send_window(SEND_WINDOW);
    transport
}

/// Names one carrier: its key among the daemon's sources, and how many
/// carriers came before it, so that a key used again never names an old
/// carrier, whose connections may outlive it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CarrierId {
    pub(crate) key: usize,
    pub(crate) generation: u32,
}

/// The address QUIC knows the carrier `id` by: one in the range set aside
/// for discarding (RFC 6666), so never a real one.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn carrier_address(id: CarrierId) -> SocketAddr {
    let key = u32::try_from(id.key).unwrap_or(u32::MAX);
    let generation = id.generation;
    let ip = Ipv6Addr::new(
        0x100,
        0,
        (generation >> 16) as u16,
        generation as u16,
        0,
        0,
        (key >> 16) as u16,
        key as u16,
    );
    SocketAddr::new(IpAddr::V6(ip), 1)
}

/// The carrier with `address`, if it's a carrier's.
pub(crate) fn carrier_id(address: SocketAddr) -> Option<CarrierId> {
    let IpAddr::V6(ip) = address.ip() else {
        return None;
    };
    let join = |high: u16, low: u16| u32::from(high) << 16 | u32::from(low);
    match ip.segments() {
        [
            0x100,
            0,
            generation_high,
            generation_low,
            0,
            0,
            key_high,
            key_low,
        ] if address.port() == 1 => Some(CarrierId {
            key: usize::try_from(join(key_high, key_low)).ok()?,
            generation: join(generation_high, generation_low),
        }),
        _ => None,
    }
}

/// Names the way packets from `address` come, for the log: through a
/// carrier, or over UDP.
pub(crate) struct Way(pub(crate) SocketAddr);

impl std::fmt::Display for Way {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match carrier_id(self.0) {
            Some(id) => write!(f, "carrier {}.{}", id.key, id.generation),
            None => write!(f, "UDP {}", self.0),
        }
    }
}

/// What begins a probe: a datagram a client sends the daemon's UDP port,
/// for the daemon to send straight back, to find out whether UDP gets
/// through. Then come the daemon's probe key and 8 bytes of the client's
/// own, to tell its probes apart.
const PROBE: &[u8; 8] = b"clackprb";

/// How long a probe is.
const PROBE_LEN: usize = PROBE.len() + 16 + 8;

/// A probe with `key`, the daemon's, and `nonce`, the client's.
pub(crate) fn probe(key: &[u8; 16], nonce: u64) -> [u8; PROBE_LEN] {
    let mut probe = [0; PROBE_LEN];
    probe[..8].copy_from_slice(PROBE);
    probe[8..24].copy_from_slice(key);
    probe[24..].copy_from_slice(&nonce.to_le_bytes());
    probe
}

/// Whether `datagram` is a probe with `key`. No QUIC packet is: its first
/// 24 bytes would have to match by chance.
pub(crate) fn is_probe(datagram: &[u8], key: &[u8; 16]) -> bool {
    datagram.len() == PROBE_LEN && datagram[..8] == *PROBE && datagram[8..24] == *key
}

/// The most of a carrier's packets left unwritten, for its socket to
/// take, before no more are made for the connections on it until it has
/// taken them. Whatever waits there waits in order, a key's echo behind a
/// download through a forwarded port; whatever QUIC hasn't made into
/// packets yet still goes in order of priority.
pub(crate) const CARRIER_BACKLOG: usize = 64 * 1024;

/// Appends `packet` to `out`, framed for a carrier.
pub(crate) fn frame_packet(packet: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&frame::header(packet.len()));
    out.extend_from_slice(packet);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_need_the_key() {
        let key = [7; 16];
        let probe = probe(&key, 42);
        assert!(is_probe(&probe, &key));
        assert!(!is_probe(&probe, &[8; 16]));
        assert!(!is_probe(&probe[..PROBE_LEN - 1], &key));
    }

    #[test]
    fn carrier_addresses_lead_back_to_their_carrier() {
        for key in [0, 1, 0xffff, 0x1_0000, 0x1234_5678] {
            for generation in [0, 7, 0x8765_4321] {
                let id = CarrierId { key, generation };
                assert_eq!(carrier_id(carrier_address(id)), Some(id));
            }
        }
        let old = CarrierId {
            key: 3,
            generation: 1,
        };
        let new = CarrierId {
            key: 3,
            generation: 2,
        };
        assert_ne!(carrier_address(old), carrier_address(new));
        assert_eq!(carrier_id("127.0.0.1:1".parse().unwrap()), None);
        assert_eq!(carrier_id("[100::5]:2".parse().unwrap()), None);
    }
}
