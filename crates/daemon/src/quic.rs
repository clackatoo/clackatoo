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
mod path;
mod server;

use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use clackatoo_protocol::frame;
use quinn_proto::congestion::BbrConfig;
use quinn_proto::{TransportConfig, VarInt};

pub use client::Contact;
pub(crate) use client::{Carried, carry};
pub use path::Route;
pub(crate) use server::{QuicEvent, QuicServer, Received};

/// The name the daemon's certificate is for. Nothing checks it: the client
/// pins the certificate itself.
const SERVER_NAME: &str = "clackatoo";

/// How long a connection may go without hearing from the other end before
/// it ends: long enough for a laptop to change networks, or sleep briefly,
/// and pick up where it left off. The client's keep-alives stop an idle
/// terminal reaching it.
const IDLE_TIMEOUT: Duration = Duration::from_mins(10);

/// How often a client lets the daemon know it's there when nothing else is
/// sent: often enough to keep a home router's mapping for UDP open.
const KEEP_ALIVE: Duration = Duration::from_secs(25);

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
        // A stream for each terminal a client follows.
        .max_concurrent_uni_streams(VarInt::from_u32(1024));
    transport
}

/// The address QUIC knows the carrier with `key` by: one in the range set
/// aside for discarding (RFC 6666), so never a real one.
pub(crate) fn carrier_address(key: usize) -> SocketAddr {
    let key = u32::try_from(key).unwrap_or(u32::MAX);
    #[allow(clippy::cast_possible_truncation)]
    let ip = Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, (key >> 16) as u16, key as u16);
    SocketAddr::new(IpAddr::V6(ip), 1)
}

/// The key of the carrier with `address`, if it's a carrier's.
pub(crate) fn carrier_key(address: SocketAddr) -> Option<usize> {
    let IpAddr::V6(ip) = address.ip() else {
        return None;
    };
    match ip.segments() {
        [0x100, 0, 0, 0, 0, 0, high, low] if address.port() == 1 => {
            usize::try_from(u32::from(high) << 16 | u32::from(low)).ok()
        }
        _ => None,
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
            assert_eq!(carrier_key(carrier_address(key)), Some(key));
        }
        assert_eq!(carrier_key("127.0.0.1:1".parse().unwrap()), None);
        assert_eq!(carrier_key("[100::5]:2".parse().unwrap()), None);
    }
}
