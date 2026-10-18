//! Ports forwarded from this daemon's machine to another it's connected
//! to (CLACKA-117, phase 3): `:PortForward 8080` in a workspace from
//! another machine, in a window on this one, so a browser here reaches a
//! dev server there.
//!
//! A window forwards ports to its own daemon's machine itself, listening
//! on its loopback and passing each connection to the daemon over QUIC
//! (see [`crate::forward`]). For another machine's workspace, this daemon
//! does the same through its connection to that machine: it listens on
//! its own loopback, which is the window's too when the window is on this
//! machine, and passes each connection on as a window would, on a stream
//! of the connection's own, the daemon there connecting where it says.
//! The forwards are the connection's: they keep listening while it's
//! dropped, and are handed to it again once it's made again, until
//! they're stopped, or the connection is ended. What they run into is
//! told to the client that last asked about them.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use clackatoo_protocol::{Error, Event, Forwarding, PaneId, Response, wire};
use mio::Token;

use super::Routed;
use crate::forward::{Forwards, Spec, refusal};
use crate::server::{Daemon, Link, Source};

/// The ports forwarded through one connection.
#[derive(Debug)]
pub(in crate::server) struct Forwarded {
    forwards: Forwards,
    /// The client told what they run into: the one that last asked about
    /// them.
    asker: Option<Token>,
}

impl Daemon {
    /// Does what `forwarding` says to the ports forwarded to the machine
    /// `pane` is on, as the client at `token` asks, if that's another
    /// machine: answered at once, saying what was done.
    pub(super) fn route_forwards(
        &mut self,
        token: Token,
        pane: PaneId,
        forwarding: &Forwarding,
    ) -> Option<Routed> {
        let (at, _) = self.relayed_pane(pane)?;
        // Only a window on this machine forwards this machine's ports.
        let local = matches!(
            self.sources.get(token.0),
            Some(Source::Client(client)) if matches!(client.link, Link::Unix(_))
        );
        if !local {
            let refused = Error::Other(
                "only a window on the daemon's machine forwards its ports to another".into(),
            );
            return Some(Routed::Answered(Response::Error(refused)));
        }
        let host = self.links.list[at].remote.host.clone();
        let link = (self.reached(at))
            .and_then(|reached| Some(reached.carried.as_ref()?.forwarder(reached.wire)));
        let waker = Arc::clone(&self.waker);
        let news = Arc::clone(&self.links.forward_news);
        let forwarded = (self.links.list[at].relayed.forwards).get_or_insert_with(|| {
            let mut forwards = Forwards::new(move || {
                news.store(true, Ordering::Release);
                drop(waker.wake());
            });
            if let Some(link) = link {
                forwards.attach(link);
            }
            Forwarded {
                forwards,
                asker: None,
            }
        });
        forwarded.asker = Some(token);
        let forwards = &mut forwarded.forwards;
        let said = match forwarding {
            Forwarding::List => Ok(listing(&forwards.list(), &host)),
            Forwarding::Add {
                local,
                host: to,
                port,
            } => {
                let asked = Spec {
                    local: *local,
                    host: to.clone(),
                    port: *port,
                };
                match forwards.add(asked.clone()) {
                    Ok(spec) => Ok(format!(
                        "Forwarding port {} here to {}:{} on {host}",
                        spec.local,
                        shown_host(&spec),
                        spec.port
                    )),
                    Err(err) => Err(refusal(&err, &asked)),
                }
            }
            Forwarding::Stop(Some(local)) => match forwards.remove(*local) {
                Some(spec) => Ok(format!("Stopped forwarding {spec} to {host}")),
                None => Err(format!("Port {local} isn't forwarded to {host}")),
            },
            Forwarding::Stop(None) => Ok(match forwards.clear() {
                0 => format!("No ports are forwarded to {host}"),
                1 => format!("Stopped forwarding the port to {host}"),
                count => format!("Stopped forwarding {count} ports to {host}"),
            }),
        };
        Some(Routed::Answered(match said {
            Ok(said) => Response::Text(said),
            Err(why) => Response::Error(Error::Other(why)),
        }))
    }

    /// The connection at `at` has been made: the ports forwarded through
    /// it are handed to it.
    pub(super) fn forwards_linked(&mut self, at: usize) {
        let forwarder = (self.reached(at))
            .and_then(|reached| Some(reached.carried.as_ref()?.forwarder(reached.wire)));
        if let (Some(forwarded), Some(forwarder)) =
            (&mut self.links.list[at].relayed.forwards, forwarder)
        {
            forwarded.forwards.attach(forwarder);
        }
    }

    /// The connection at `at` has been lost: the ports forwarded through
    /// it wait, still listening, for it to be made again.
    pub(super) fn forwards_lost(&mut self, at: usize) {
        if let Some(forwarded) = &mut self.links.list[at].relayed.forwards {
            forwarded.forwards.detach();
        }
    }

    /// Tells each client that asked about the ports forwarded through a
    /// connection what they've run into since: as each forward's thread
    /// wakes the loop to say.
    pub(in crate::server) fn tell_forward_reports(&mut self) {
        // Asked every turn: only a forward's thread saying something
        // makes it look.
        if !self.links.forward_news.swap(false, Ordering::Acquire) {
            return;
        }
        let mut said = Vec::new();
        for linked in &self.links.list {
            let Some(forwarded) = &linked.relayed.forwards else {
                continue;
            };
            let host = &linked.remote.host;
            for report in forwarded.forwards.take_reports() {
                if let Some(asker) = forwarded.asker {
                    said.push((asker, report.describe(host)));
                }
            }
        }
        for (token, said) in said {
            let new_enough = matches!(
                self.sources.get(token.0),
                Some(Source::Client(client)) if client.wire >= wire::RELAY
            );
            if new_enough {
                self.send_event(token, Event::Forwarded(said));
            }
        }
    }

    /// Forgets the client at `token`, gone: it's told nothing more of the
    /// ports forwarded, which go on.
    pub(super) fn forget_forwards_client(&mut self, token: Token) {
        for linked in &mut self.links.list {
            if let Some(forwarded) = &mut linked.relayed.forwards {
                forwarded.asker.take_if(|asker| *asker == token);
            }
        }
    }
}

/// The host a forward goes to, as written after `:PortForward`: IPv6
/// addresses in brackets.
fn shown_host(spec: &Spec) -> String {
    if spec.host.contains(':') {
        format!("[{}]", spec.host)
    } else {
        spec.host.clone()
    }
}

/// The forwards to the machine called `machine`, and how many connections
/// each has open, for the bar.
fn listing(list: &[(Spec, usize)], machine: &str) -> String {
    if list.is_empty() {
        return format!("No ports are forwarded to {machine}: :PortForward 8080 forwards one");
    }
    let mut said = format!("Forwarding to {machine}");
    for (at, (spec, open)) in list.iter().enumerate() {
        said.push_str(if at == 0 { " " } else { " · " });
        let _ = write!(said, "{spec}");
        match open {
            0 => {}
            1 => said.push_str(" (1 connection)"),
            open => {
                let _ = write!(said, " ({open} connections)");
            }
        }
    }
    said.push_str(" · :PortForward! PORT stops one");
    said
}
