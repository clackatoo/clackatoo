//! A relay for a byte stream, such as the ssh connection a carrier runs in.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::link::{Direction, Fate, Link};
use crate::pipe::Pipe;

/// Stands between the two ends of a byte stream, passing bytes both ways
/// through a [`Link`], which delays and holds them but never loses them,
/// as TCP doesn't. [`kill`](Self::kill) cuts it, as when ssh dies.
#[derive(Debug)]
pub struct StreamRelay {
    client: UnixStream,
    server: UnixStream,
    link: Link,
    readers: Vec<JoinHandle<()>>,
}

impl StreamRelay {
    /// Starts relaying between `server` and a new stream, returned for the
    /// client to use in place of `server`.
    ///
    /// # Errors
    ///
    /// If the streams can't be set up.
    pub fn start(server: UnixStream, link: Link) -> io::Result<(Self, UnixStream)> {
        let (client, ours) = UnixStream::pair()?;
        let mut relay = Self {
            client: ours,
            server,
            link,
            readers: Vec::new(),
        };
        relay.relay(Direction::Up)?;
        relay.relay(Direction::Down)?;
        Ok((relay, client))
    }

    /// The link bytes pass through.
    #[must_use]
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// Cuts the stream both ways, at once, as when ssh dies: each end sees
    /// it close.
    pub fn kill(&self) {
        let _ = self.client.shutdown(Shutdown::Both);
        let _ = self.server.shutdown(Shutdown::Both);
    }

    /// Cuts the stream as [`kill`](Self::kill) does, `after` from now, as
    /// ssh does when the connection under it has been silent too long.
    ///
    /// # Errors
    ///
    /// If the thread waiting to do it can't be started.
    pub fn kill_after(&self, after: Duration) -> io::Result<()> {
        let killer = self.killer()?;
        std::thread::Builder::new()
            .name("netem-kill".into())
            .spawn(move || {
                std::thread::sleep(after);
                killer.kill();
            })?;
        Ok(())
    }

    /// A handle that cuts the stream as [`kill`](Self::kill) does, for
    /// whoever doesn't own the relay.
    ///
    /// # Errors
    ///
    /// If the streams can't be duplicated.
    pub fn killer(&self) -> io::Result<StreamKiller> {
        Ok(StreamKiller {
            client: self.client.try_clone()?,
            server: self.server.try_clone()?,
        })
    }

    /// Starts passing bytes `direction`.
    fn relay(&mut self, direction: Direction) -> io::Result<()> {
        let (mut from, mut to) = match direction {
            Direction::Up => (self.client.try_clone()?, self.server.try_clone()?),
            Direction::Down => (self.server.try_clone()?, self.client.try_clone()?),
        };
        let link = self.link.clone();
        let thread = std::thread::Builder::new()
            .name("netem-stream".into())
            .spawn(move || {
                // A pipe of its own, so a full stream one way never holds
                // up the other. An empty segment is the end of the stream.
                let pipe = Arc::new(Pipe::start(move |_, bytes| {
                    if bytes.is_empty() {
                        let _ = to.shutdown(Shutdown::Write);
                    } else {
                        let _ = to.write_all(bytes);
                    }
                }));
                let mut buffer = vec![0; 64 * 1024];
                loop {
                    let n = match from.read(&mut buffer) {
                        Ok(0) | Err(_) => 0,
                        Ok(n) => n,
                    };
                    if n == 0 {
                        let at = match link.fate(direction, 0, true) {
                            Fate::Deliver(at) => at,
                            Fate::Drop => std::time::Instant::now(),
                        };
                        pipe.send(at, direction, Vec::new());
                        // Waits for what's still on its way to go.
                        drop(pipe);
                        return;
                    }
                    for segment in buffer[..n].chunks(link.segment(direction)) {
                        if let Fate::Deliver(at) = link.fate(direction, segment.len(), true) {
                            pipe.send(at, direction, segment.to_vec());
                        }
                    }
                }
            })?;
        self.readers.push(thread);
        Ok(())
    }
}

/// Cuts a [`StreamRelay`]'s stream, from wherever it's held.
#[derive(Debug)]
pub struct StreamKiller {
    client: UnixStream,
    server: UnixStream,
}

impl StreamKiller {
    /// Cuts the stream both ways, at once, as when ssh dies.
    pub fn kill(&self) {
        let _ = self.client.shutdown(Shutdown::Both);
        let _ = self.server.shutdown(Shutdown::Both);
    }
}

impl Drop for StreamRelay {
    fn drop(&mut self) {
        self.kill();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::link::{Conditions, Impairment};

    #[test]
    fn bytes_cross_in_order_after_the_delay_and_a_kill_ends_both_ends() {
        let (server, mut far) = UnixStream::pair().unwrap();
        let link = Link::new(
            Conditions::both(Impairment {
                delay: Duration::from_millis(50),
                jitter: Duration::from_millis(50),
                mtu: Some(100),
                ..Impairment::default()
            }),
            1,
        );
        let (relay, mut client) = StreamRelay::start(server, link).unwrap();
        let message: Vec<u8> = (0..=255).cycle().take(5000).collect();
        let sent = Instant::now();
        client.write_all(&message).unwrap();
        let mut received = vec![0; message.len()];
        far.read_exact(&mut received).unwrap();
        assert_eq!(received, message, "in order, and whole");
        assert!(sent.elapsed() >= Duration::from_millis(50));

        relay.kill();
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).unwrap_or(0), 0);
        assert_eq!(far.read(&mut byte).unwrap_or(0), 0);
    }

    #[test]
    fn a_blackout_holds_a_stream_until_it_ends() {
        let (server, mut far) = UnixStream::pair().unwrap();
        let link = Link::new(Conditions::clean(), 2);
        let (_relay, mut client) = StreamRelay::start(server, link.clone()).unwrap();
        link.black_out(Duration::from_millis(300));
        let sent = Instant::now();
        client.write_all(b"held").unwrap();
        let mut received = [0; 4];
        far.read_exact(&mut received).unwrap();
        assert!(sent.elapsed() >= Duration::from_millis(290));
    }
}
