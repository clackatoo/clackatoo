//! A bad network on demand, for testing remote connections.
//!
//! A [`UdpRelay`] stands between a client and a UDP port, and a
//! [`StreamRelay`] between the two ends of a byte stream, such as the ssh
//! connection a QUIC carrier runs in. Both pass what they're given through
//! a [`Link`], which decides what becomes of each packet: lost, delayed,
//! held back behind later ones, queued behind a narrow pipe, or held in a
//! stall and then released in a burst, as a relay over TCP does. A link's
//! [`Conditions`] can be changed while traffic flows, and it can black out
//! for a while, losing everything (UDP) or holding it (a stream), as a
//! network that goes away does.
//!
//! A byte stream never loses anything, since TCP sends it again: loss on a
//! stream shows as delay. Packets too big for the link's MTU are dropped,
//! as a tunnel that doesn't fragment drops them.
//!
//! Everything random comes from a seeded generator, so a test that fails
//! fails the same way again.
//!
//! ```no_run
//! use std::time::Duration;
//! use clackatoo_netem::{Conditions, Link, UdpRelay};
//!
//! let link = Link::new(Conditions::lossy(0.3, Duration::from_millis(300), Duration::from_millis(200)), 7);
//! let relay = UdpRelay::start("127.0.0.1:60000".parse().unwrap(), link.clone()).unwrap();
//! // Send to relay.address() instead of the port; later:
//! link.black_out(Duration::from_secs(5));
//! relay.rebind().unwrap(); // the port the far end sees changes
//! ```

mod link;
mod measure;
mod pipe;
mod stream;
mod udp;

pub use link::{Conditions, Direction, Impairment, Link, LinkStats, Stalls};
pub use measure::Latencies;
pub use stream::StreamRelay;
pub use udp::UdpRelay;
