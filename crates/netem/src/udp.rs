//! A relay for UDP: between one client and one port.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::link::{Direction, Fate, Link};
use crate::pipe::Pipe;

/// How often the threads reading the sockets look up to see whether to
/// stop.
const LOOK_UP_EVERY: Duration = Duration::from_millis(20);

/// Stands between a client and a UDP port, passing datagrams both ways
/// through a [`Link`]. The client sends to [`address`](Self::address) in
/// place of the port, and the port sees datagrams come from the relay.
/// Only one client: the last to send is the one answered.
#[derive(Debug)]
pub struct UdpRelay {
    shared: Arc<Shared>,
    pipe: Arc<Pipe>,
    readers: Mutex<Vec<JoinHandle<()>>>,
}

#[derive(Debug)]
struct Shared {
    /// Where the client sends.
    front: UdpSocket,
    /// Where the port's datagrams come back to, until rebound.
    back: RwLock<Arc<UdpSocket>>,
    target: SocketAddr,
    client: Mutex<Option<SocketAddr>>,
    link: Link,
    stopped: AtomicBool,
}

impl UdpRelay {
    /// Starts relaying datagrams to `target` through `link`.
    ///
    /// # Errors
    ///
    /// If the sockets can't be made.
    pub fn start(target: SocketAddr, link: Link) -> io::Result<Self> {
        let front = UdpSocket::bind(SocketAddr::new(loopback(target.ip()), 0))?;
        front.set_read_timeout(Some(LOOK_UP_EVERY))?;
        let shared = Arc::new(Shared {
            front,
            back: RwLock::new(Arc::new(back_socket(target)?)),
            target,
            client: Mutex::new(None),
            link,
            stopped: AtomicBool::new(false),
        });
        let pipe = Arc::new(Pipe::start({
            let shared = Arc::clone(&shared);
            move |direction, bytes| {
                // A full buffer is as good as lost.
                let _ = match direction {
                    Direction::Up => shared.back().send_to(bytes, shared.target),
                    Direction::Down => {
                        match *shared.client.lock().unwrap_or_else(PoisonError::into_inner) {
                            Some(client) => shared.front.send_to(bytes, client),
                            None => Ok(0),
                        }
                    }
                };
            }
        }));
        let relay = Self {
            shared,
            pipe,
            readers: Mutex::new(Vec::new()),
        };
        relay.read_front()?;
        relay.read_back(relay.shared.back())?;
        Ok(relay)
    }

    /// Where the client should send, in place of the target.
    ///
    /// # Panics
    ///
    /// If the socket has no address, which a bound socket always has.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.shared
            .front
            .local_addr()
            .expect("a bound socket has an address")
    }

    /// The link datagrams pass through.
    #[must_use]
    pub fn link(&self) -> &Link {
        &self.shared.link
    }

    /// Sends the client's datagrams from a new port from now on, as a NAT
    /// does when it forgets a mapping: the target sees the client move.
    /// Answers still on their way to the old port are lost.
    ///
    /// # Errors
    ///
    /// If the new socket can't be made.
    pub fn rebind(&self) -> io::Result<()> {
        let socket = Arc::new(back_socket(self.shared.target)?);
        *self
            .shared
            .back
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::clone(&socket);
        self.read_back(socket)
    }

    fn read_front(&self) -> io::Result<()> {
        let shared = Arc::clone(&self.shared);
        let pipe = Arc::clone(&self.pipe);
        self.spawn(move || {
            let mut buffer = vec![0; 65_536];
            while !shared.stopped.load(Ordering::Relaxed) {
                let Ok((n, from)) = shared.front.recv_from(&mut buffer) else {
                    continue;
                };
                *shared.client.lock().unwrap_or_else(PoisonError::into_inner) = Some(from);
                if let Fate::Deliver(at) = shared.link.fate(Direction::Up, n, false) {
                    pipe.send(at, Direction::Up, buffer[..n].to_vec());
                }
            }
        })
    }

    /// Reads what comes back to `socket` while it's the one in use.
    fn read_back(&self, socket: Arc<UdpSocket>) -> io::Result<()> {
        let shared = Arc::clone(&self.shared);
        let pipe = Arc::clone(&self.pipe);
        self.spawn(move || {
            let mut buffer = vec![0; 65_536];
            while !shared.stopped.load(Ordering::Relaxed) && Arc::ptr_eq(&shared.back(), &socket) {
                let Ok((n, from)) = socket.recv_from(&mut buffer) else {
                    continue;
                };
                if from != shared.target {
                    continue;
                }
                if let Fate::Deliver(at) = shared.link.fate(Direction::Down, n, false) {
                    pipe.send(at, Direction::Down, buffer[..n].to_vec());
                }
            }
        })
    }

    fn spawn(&self, read: impl FnOnce() + Send + 'static) -> io::Result<()> {
        let thread = std::thread::Builder::new()
            .name("netem-udp".into())
            .spawn(read)?;
        self.readers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(thread);
        Ok(())
    }
}

impl Shared {
    fn back(&self) -> Arc<UdpSocket> {
        Arc::clone(&self.back.read().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Drop for UdpRelay {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
        let readers =
            std::mem::take(&mut *self.readers.lock().unwrap_or_else(PoisonError::into_inner));
        for reader in readers {
            let _ = reader.join();
        }
    }
}

/// The loopback address of `ip`'s family.
fn loopback(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
        IpAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
    }
}

/// A socket to send to `target` from, on a port of its own.
fn back_socket(target: SocketAddr) -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind(SocketAddr::new(loopback(target.ip()), 0))?;
    socket.set_read_timeout(Some(LOOK_UP_EVERY))?;
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::link::Conditions;

    /// An echo server on a port of its own: it sends back what it gets,
    /// and says where from.
    fn echo() -> (SocketAddr, std::sync::mpsc::Receiver<SocketAddr>) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        let (heard, from) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buffer = [0; 2048];
            while let Ok((n, peer)) = socket.recv_from(&mut buffer) {
                if heard.send(peer).is_err() {
                    return;
                }
                let _ = socket.send_to(&buffer[..n], peer);
            }
        });
        (address, from)
    }

    fn client(relay: &UdpRelay) -> UdpSocket {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.connect(relay.address()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
    }

    #[test]
    fn datagrams_go_both_ways_after_the_delay() {
        let (target, _heard) = echo();
        let relay = UdpRelay::start(
            target,
            Link::new(
                Conditions::lossy(0.0, Duration::from_millis(100), Duration::ZERO),
                1,
            ),
        )
        .unwrap();
        let socket = client(&relay);
        let sent = Instant::now();
        socket.send(b"hello").unwrap();
        let mut buffer = [0; 16];
        let n = socket.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], b"hello");
        assert!(sent.elapsed() >= Duration::from_millis(95));
    }

    #[test]
    fn a_blackout_loses_datagrams_and_rebinding_moves_the_port() {
        let (target, heard) = echo();
        let relay = UdpRelay::start(target, Link::new(Conditions::clean(), 2)).unwrap();
        let socket = client(&relay);
        let mut buffer = [0; 16];
        socket.send(b"one").unwrap();
        socket.recv(&mut buffer).unwrap();
        let first = heard.recv().unwrap();

        relay.link().black_out(Duration::from_millis(300));
        socket.send(b"two").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        assert!(socket.recv(&mut buffer).is_err(), "lost in the blackout");
        std::thread::sleep(Duration::from_millis(300));

        relay.rebind().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket.send(b"three").unwrap();
        let n = socket.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], b"three");
        assert_ne!(heard.recv().unwrap(), first, "the target saw a new port");
    }
}
