//! Passing machines on (CLACKA-117): a daemon shows the daemons connected
//! to it the machines it's connected to, and they show those machines'
//! workspaces too, by connections of their own that go through it.
//!
//! A laptop connected to a desk connected to a server shows the server's
//! workspaces as `name@server`: keyed by the machine they live on, never
//! by the way there, so never `name@desk@server`. The desk says what it
//! passes on in its workspaces ([`Workspaces::passed`]): each machine,
//! what it calls it, the machines its own connection goes through, and
//! whether it's connected now. The laptop makes a connection to the
//! server for each it doesn't reach already, asked for by no one here and
//! never kept, which goes through the desk: a stream on the laptop's
//! connection to the desk, which the desk carries on through its own
//! connection to the server ([`Request::Reach`]), as ssh would carry it.
//! Everything then works as it does for one machine straight there: the
//! laptop is the server's client, so what the server lends, borrows and
//! shows follows the machine it really is, and its own relay numbers,
//! caps and times out what the server sends, as for any connection. The
//! desk passes the bytes on as fast as each side takes them, without
//! looking at them.
//!
//! - **Who's there.** The desk could read or change what it carries, or
//!   answer in the server's place, so what it carries is QUIC, and the
//!   laptop checks the server's certificate against the identity key it
//!   knows the server by: from when it last reached it with ssh, or, for a
//!   machine it has never met, the key the desk vouches for the first
//!   time, kept from then on (see [`ways`](super::links::ways)). A desk
//!   can then only pass the connection on, hold it up, or drop it, after
//!   that first time. `clackatoo forget HOST` forgets a key, for a machine
//!   whose daemon chose a new one.
//!
//! - **Once, by the shortest way.** A machine is connected to once,
//!   however many daemons pass it on. A connection asked for here goes by
//!   ssh straight there, and through the daemons passing it on only while
//!   ssh can't get there, moving back once it can (see
//!   [`ways`](super::links::ways)); one passed on goes through the daemon
//!   that reaches it through the fewest machines. A connection asked for
//!   to a machine already shown through another takes over what it
//!   showed, numbered as it was.
//! - **Never round a loop.** A daemon never takes back its own machine,
//!   nor a way through itself, by the machines' IDs: a laptop and a desk
//!   connected to each other, or three machines in a ring, each show each
//!   other machine once.
//! - **On unless asked not to.** A connection asked for with `share`
//!   off, as for a server shared with others, isn't passed on, and a
//!   stream asking to reach it is refused, and those that did are ended.
//!   Nor is anything else of it, either way: what the daemons connected
//!   to this one are told, and what they ask through it (see
//!   [`unshared`](super::relay::unshared)).
//! - **Only for daemons.** Only a client that said which machine it is
//!   may ask to reach another, so a way back through it is always seen;
//!   reaching this daemon itself makes a client that may only greet it and
//!   carry QUIC.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use clackatoo_protocol::{
    ClientMessage, ConnectionState, Error, MachineId, Passed, Request, Response, frame, wire,
};
use mio::Token;
use quinn_proto::ConnectionHandle;

use super::forwards::Reaching;
use super::links::{Direct, Linked, Way};
use super::relay::mirror::shown_name;
use super::{Daemon, Flood, Link, Source};
use crate::remote::{Asking, Cancel, Remote};

/// The most machines taken in from what one daemon passes on.
const MOST_PASSED: usize = 64;

/// The most machines a way passed on may go through, as said: more would
/// be a loop, or near enough.
const LONGEST_WAY: usize = 8;

/// What clackatoo is run as for a connection to a machine passed on,
/// which never runs it: it goes through the daemon passing it on.
const COMMAND: &str = "clackatoo";

impl Daemon {
    /// The machines this daemon passes on to the daemons connected to it:
    /// those it has connections to that have said which machine they are,
    /// can be reached through it, having last run at protocol 95 or later,
    /// and weren't asked not to be passed on or stopped; each once, by the
    /// connection that's connected, with the key it's known by, which this
    /// daemon so vouches for. Each is named by its host, without the user
    /// ssh logs in as or a port.
    pub(super) fn passed_on(&self) -> Vec<Passed> {
        self.passed_on_but(&self.kept())
    }

    /// As [`passed_on`](Self::passed_on), but for what's `kept` from the
    /// daemon told: never a connection kept from it, and a way through a
    /// machine kept from it named without that machine, as the way one
    /// asked for here may go, which is the asker's to keep, not passing
    /// on through it (CLACKA-118).
    pub(super) fn passed_on_but(&self, kept: &super::relay::unshared::Kept) -> Vec<Passed> {
        let mut passed: Vec<Passed> = Vec::new();
        for (at, linked) in self.links.list.iter().enumerate() {
            let Some(machine) = linked.machine.as_ref() else {
                continue;
            };
            let usable = !kept.link(at)
                && !matches!(linked.state, ConnectionState::Stopped { .. })
                && linked
                    .wire
                    .is_some_and(|wire| wire::daemon_has(wire, wire::CHAIN));
            if !usable {
                continue;
            }
            let reached = linked.connected.is_some();
            let one = Passed {
                id: machine.id,
                name: bare_host(&linked.remote.host),
                run: machine.run,
                through: (linked.through.iter().copied())
                    .filter(|&id| !kept.machine(id))
                    .collect(),
                reached,
                key: self.links.pin(machine.id).map(|pin| pin.key),
            };
            match passed.iter_mut().find(|known| known.id == machine.id) {
                Some(known) if reached && !known.reached => *known = one,
                Some(_) => {}
                None => passed.push(one),
            }
        }
        passed
    }

    /// Answers [`Request::Forget`](clackatoo_protocol::Request::Forget)
    /// from the client at `token`: forgets the identity key the machine of
    /// the connection to `host` is known by, and tries that connection
    /// again at once if it was stopped, as by a key it didn't know. That's
    /// the machine it last proved it was, or was expected to be, never one
    /// it only said it was, and the connection, tried again, takes
    /// whatever machine is there now, as `:connect` would. `host` may be a
    /// machine's ID instead, which is taken as one first.
    pub(super) fn serve_forget(&mut self, token: Token, host: &str) -> Result<Response, Error> {
        // Only this machine's own clients: a device on the web app, or a
        // daemon relaying, never.
        if matches!(
            self.sources.get(token.0),
            Some(Source::Client(client))
                if matches!(client.link, Link::Web(_)) || client.relays || client.machine.is_some()
        ) {
            return Err(Error::Other(
                "only this machine's own windows and commands forget a machine's key".to_owned(),
            ));
        }
        // A machine's ID first: the key it's known by, whatever connection
        // proved it, or none.
        if let Ok(machine) = host.parse::<MachineId>() {
            let forgot = self.links.forget_pin(machine);
            if forgot {
                log::info!("{machine}'s identity key is forgotten, as asked");
            }
            return Ok(Response::Text(if forgot {
                format!("forgot the identity key of machine {machine}")
            } else {
                format!("no key was known for machine {machine}")
            }));
        }
        let Some(at) = self.links.find(host) else {
            return Err(Error::Other(format!(
                "there's no connection to ‘{}’",
                host.escape_debug()
            )));
        };
        let Some(machine) = self.links.list[at].proved_id() else {
            return Ok(Response::Text(format!(
                "‘{host}’ hasn't been reached yet: no key is known for it"
            )));
        };
        let forgot = self.links.forget_pin(machine);
        if forgot {
            log::info!("{host}'s identity key is forgotten, as asked");
        }
        let linked = &mut self.links.list[at];
        if matches!(linked.state, ConnectionState::Stopped { .. }) && linked.trying.is_none() {
            // Whatever's there now, as `:connect` takes it: a machine whose
            // state was wiped has a new ID as well as a new key.
            linked.asked_anew = linked.asked;
            self.try_link_now(at);
            self.tell_connections();
        }
        Ok(Response::Text(if forgot {
            format!("forgot ‘{host}’'s identity key: the next connection takes the one it shows")
        } else {
            format!("no key was known for ‘{host}’")
        }))
    }

    /// Tells the daemons connected to this one what it passes on, if that
    /// changed since they were told: the streams reaching a machine no
    /// longer passed on end. And if what's kept from them changed, they're
    /// told what they may be now, and let go of what they may not (see
    /// [`shared_anew`](Self::shared_anew)).
    pub(super) fn sharing_changed(&mut self) {
        let kept = self.kept();
        let kept_changed = kept != self.links.kept_told;
        self.links.kept_told = kept;
        let passed = self.passed_on();
        if passed != self.links.passed_told {
            let gone: Vec<MachineId> = (self.links.passed_told.iter())
                .filter(|told| !passed.iter().any(|now| now.id == told.id))
                .map(|told| told.id)
                .collect();
            self.links.passed_told = passed;
            for machine in gone {
                self.abandon_reaching(machine);
            }
            if !kept_changed {
                self.tell_workspaces();
            }
        }
        if kept_changed {
            self.shared_anew();
        }
    }

    /// Takes in what the daemon of the connection at `at` passes on, as
    /// its workspaces say: never this machine, nor that one, nor a way
    /// through either, nor more than [`MOST_PASSED`], with names made safe
    /// to show.
    pub(super) fn take_passed(&mut self, at: usize, passed: &[Passed]) {
        let own = self.machine.id;
        let there = self.links.list[at].machine_id();
        let mut taken: Vec<Passed> = Vec::new();
        for offered in passed {
            let loops = offered.id == own
                || Some(offered.id) == there
                || offered.through.contains(&own)
                || there.is_some_and(|there| offered.through.contains(&there))
                || offered.through.contains(&offered.id);
            if loops
                || offered.through.len() >= LONGEST_WAY
                || taken.iter().any(|known| known.id == offered.id)
            {
                continue;
            }
            if taken.len() == MOST_PASSED {
                break;
            }
            taken.push(Passed {
                id: offered.id,
                name: passed_name(&offered.name, offered.id),
                run: offered.run,
                through: offered.through.clone(),
                reached: offered.reached,
                key: offered.key,
            });
        }
        if taken != self.links.list[at].passed {
            self.links.list[at].passed = taken;
            self.sync_chained();
            // What's kept from the daemons relaying goes with what a kept
            // connection passes on (CLACKA-118).
            self.sharing_changed();
        }
    }

    /// Brings the connections to machines passed on, and every
    /// connection's ways through the daemons passing its machine on, in
    /// step with what's passed on now: a machine no longer passed on, or
    /// reached another way, has its connection through others ended; one
    /// newly passed on, and reached no other way, gets one; and one
    /// waiting for a way, given one, is tried at once.
    pub(super) fn sync_chained(&mut self) {
        if self.config.links.is_none() {
            return;
        }
        let own = self.machine.id;
        let offered = self.offered();
        self.pins_in_use();
        // Those passed on no more, or reached another way, are ended at the
        // end of the turn, once nothing holds places in the list.
        let unpassed = self.unpassed(&offered);
        if !unpassed.is_empty() {
            self.links.unpassed = true;
        }
        // Those newly passed on.
        for (&machine, offers) in &offered {
            if machine == own || self.links.to(machine).is_some() {
                continue;
            }
            let host = self.unique_host(&offers[0].1.name);
            let mut linked = self.links.new_link(
                Remote {
                    host,
                    command: COMMAND.to_owned(),
                },
                None,
                false,
            );
            linked.expected = Some(machine);
            log::info!(
                "{} is passed on to this machine: connecting to it through {}",
                linked.remote.host,
                self.links.list[offers[0].0].remote.host
            );
            self.links.list.push(linked);
        }
        // Every connection's ways.
        for at in 0..self.links.list.len() {
            let ways = self.ways_to(at, &offered);
            let linked = &mut self.links.list[at];
            let machine = linked.machine_id();
            let any = !ways.is_empty();
            linked.ways.set_through(machine, &linked.remote.host, ways);
            let waiting = linked.connected.is_none()
                && linked.trying.is_none()
                && !unpassed.contains(&at)
                && !matches!(
                    linked.state,
                    ConnectionState::Stopped { .. } | ConnectionState::NeedsPassword
                );
            if !waiting {
                if linked.connected.is_some() {
                    self.better_soon(at);
                }
                continue;
            }
            if any {
                self.try_link_now(at);
            } else if !linked.asked {
                // Tried once a daemon passing it on is connected to it.
                linked.retry_at = None;
                let via = (machine.and_then(|machine| offered.get(&machine)))
                    .and_then(|offers| offers.first())
                    .map(|(by, _)| &self.links.list[*by]);
                let reason = match via {
                    Some(via) if via.connected.is_none() => {
                        format!("‘{}’, which passes it on, isn't connected", via.remote.host)
                    }
                    Some(via) => format!("‘{}’ isn't connected to it now", via.remote.host),
                    None => "nothing passing it on is connected to it now".to_owned(),
                };
                self.links.list[at].state = ConnectionState::Down { reason };
            }
        }
        self.tell_connections();
    }

    /// Where the connections to machines no longer passed on, or reached
    /// another way, are in the list, as `offered` says what's passed on.
    fn unpassed(&self, offered: &BTreeMap<MachineId, Vec<(usize, Passed)>>) -> Vec<usize> {
        let own = self.machine.id;
        let list = &self.links.list;
        (list.iter().enumerate())
            .filter(|(_, linked)| !linked.asked)
            .filter(|&(at, linked)| {
                let Some(machine) = linked.machine_id() else {
                    return true;
                };
                let other = (list.iter().enumerate()).any(|(other, linked)| {
                    other != at
                        && linked.machine_id() == Some(machine)
                        && (linked.asked || other < at)
                });
                machine == own || other || !offered.contains_key(&machine)
            })
            .map(|(at, _)| at)
            .collect()
    }

    /// Ends the connections to machines no longer passed on, or reached
    /// another way, as [`sync_chained`](Self::sync_chained) found.
    pub(super) fn drop_unpassed(&mut self) {
        if !std::mem::take(&mut self.links.unpassed) {
            return;
        }
        let offered = self.offered();
        let gone = self.unpassed(&offered);
        let mut said = Vec::new();
        for at in gone.into_iter().rev() {
            let linked = self.links.list.remove(at);
            log::info!(
                "{} is no longer passed on to this machine, or is reached another way: its \
                 connection through others has ended",
                linked.remote.host
            );
            // What its workspaces went for, for a window that shows one.
            let passed = linked
                .machine_id()
                .is_some_and(|machine| offered.contains_key(&machine));
            if !passed && !linked.relayed.mirror.workspaces.is_empty() {
                said.push(linked.remote.host.clone());
            }
            self.end_link(linked, false);
        }
        self.tell_workspaces();
        for host in said {
            self.say_to_windows(format!(
                "‘{host}’ is no longer passed on to this machine: its workspaces have gone"
            ));
        }
        self.sync_chained();
    }

    /// Every machine a connection's daemon passes on, with the place of
    /// each connection passing it on in the list, and what it said.
    fn offered(&self) -> BTreeMap<MachineId, Vec<(usize, Passed)>> {
        let mut offered: BTreeMap<MachineId, Vec<(usize, Passed)>> = BTreeMap::new();
        for (at, linked) in self.links.list.iter().enumerate() {
            for passed in &linked.passed {
                offered
                    .entry(passed.id)
                    .or_default()
                    .push((at, passed.clone()));
            }
        }
        offered
    }

    /// The ways through the daemons passing it on to the machine of the
    /// connection at `at`, of those `offered`, best first: through those
    /// connected now, which are connected to it, by the fewest machines,
    /// and never through one whose own connection goes through the
    /// connection's machine, round a loop.
    fn ways_to(&self, at: usize, offered: &BTreeMap<MachineId, Vec<(usize, Passed)>>) -> Vec<Way> {
        let Some(machine) = self.links.list[at].machine_id() else {
            return Vec::new();
        };
        let Some(offers) = offered.get(&machine) else {
            return Vec::new();
        };
        let mut ways: Vec<(usize, Way)> = (offers.iter())
            .filter(|(by, passed)| *by != at && passed.reached)
            .filter_map(|(by, passed)| {
                let neighbour = &self.links.list[*by];
                let forwarder = neighbour.forwarder.clone()?;
                let nearest = neighbour.machine.as_ref()?.id;
                if neighbour.through.contains(&machine) || passed.through.contains(&nearest) {
                    return None;
                }
                // Through the machines the connection to it goes through,
                // it, and those it goes through itself.
                let mut through = neighbour.through.clone();
                through.push(nearest);
                through.extend(passed.through.iter().copied());
                Some((
                    *by,
                    Way {
                        through,
                        via: neighbour.remote.host.clone(),
                        forwarder,
                        vouched: passed.key,
                    },
                ))
            })
            .collect();
        ways.sort_by_key(|(by, way)| (way.through.len(), *by));
        ways.into_iter().map(|(_, way)| way).collect()
    }

    /// `name`, or if another connection has that host, the same with a
    /// number after it: the host of a connection to a machine passed on.
    fn unique_host(&self, name: &str) -> String {
        let taken = |host: &str| self.links.find(host).is_some();
        if !taken(name) {
            return name.to_owned();
        }
        // There are only so many connections: one of these is free.
        let mut number = 2_usize;
        loop {
            let host = format!("{name}~{number}");
            if !taken(&host) {
                return host;
            }
            number += 1;
        }
    }

    /// Tries the connection at `at` at once, as a way to it has come.
    pub(super) fn try_link_now(&mut self, at: usize) {
        self.links.list[at].backoff = super::links::FIRST_RETRY;
        self.try_link(at);
    }

    /// Gives the connection at `at` ssh straight there as a way, if it
    /// was asked for here, with its askpass to ask ssh's questions
    /// through; or takes it away.
    pub(super) fn set_direct(&mut self, at: usize) {
        self.serve_its_askpass(at);
        let Some(config) = &self.config.links else {
            return;
        };
        let linked = &self.links.list[at];
        let direct = linked.asked.then(|| Direct {
            remote: linked.remote.clone(),
            ssh: config.ssh.clone(),
            asking: (config.askpass.clone())
                .zip(linked.askpass())
                .map(|(program, socket)| Asking { program, socket }),
            cancel: Arc::clone(&linked.cancel),
        });
        linked.ways.set_direct(direct);
    }

    /// Whether a daemon connected to this one passes on the machine of the
    /// connection at `at`.
    pub(super) fn passed_to_here(&self, at: usize) -> bool {
        (self.links.list[at].machine_id()).is_some_and(|machine| self.passed_here(at, machine))
    }

    /// Whether a daemon connected to this one, but for that of the
    /// connection at `at`, passes `machine` on, vouching for `key` as the
    /// key it knows it by.
    pub(super) fn vouched_here(&self, at: usize, machine: MachineId, key: [u8; 32]) -> bool {
        (self.links.list.iter().enumerate()).any(|(other, linked)| {
            other != at
                && (linked.passed.iter())
                    .any(|passed| passed.id == machine && passed.key == Some(key))
        })
    }

    /// Whether a daemon connected to this one, but for that of the
    /// connection at `at`, passes `machine` on.
    pub(super) fn passed_here(&self, at: usize, machine: MachineId) -> bool {
        (self.links.list.iter().enumerate()).any(|(other, linked)| {
            other != at && linked.passed.iter().any(|passed| passed.id == machine)
        })
    }

    /// Why the connection at `at`, to a machine passed on, can't be ended
    /// as asked: it's there as long as it's passed on.
    pub(super) fn passed_on_by(&self, at: usize) -> Error {
        let host = &self.links.list[at].remote.host;
        let by = (self.links.list[at].machine_id())
            .and_then(|machine| {
                (self.links.list.iter())
                    .find(|linked| linked.passed.iter().any(|passed| passed.id == machine))
            })
            .map_or_else(
                || "another machine".to_owned(),
                |linked| format!("‘{}’", linked.remote.host),
            );
        Error::Other(format!(
            "‘{host}’ is passed on by {by}: :connect {host} --no-share there stops that, or \
             :disconnect {} here",
            by.trim_matches(['‘', '’'])
        ))
    }

    /// Makes the connection at `at`, asked for here, one to a machine
    /// passed on, as it was asked to end while a daemon connected to this
    /// one passes its machine on: it goes through that daemon from now
    /// on, without ending, and isn't kept. ssh carrying it now is ended,
    /// and its askpass with it.
    pub(super) fn unask(&mut self, at: usize) {
        let linked = &mut self.links.list[at];
        linked.asked = false;
        linked.ways.set_direct(None);
        // Its ssh ends, and the connection moves through another way.
        linked.cancel.cancel();
        linked.cancel = Arc::new(Cancel::default());
        linked.drop_askpass();
        linked.better_at = None;
        self.sync_chained();
    }

    /// A way to tell the loop the connection to `host` goes through these
    /// machines now, as each carrier its threads open is taken on.
    pub(super) fn used_for(&self, host: &str) -> Arc<dyn Fn(Vec<MachineId>) + Send + Sync> {
        let tell = self.links.teller();
        let waker = Arc::clone(&self.waker);
        let host = host.to_owned();
        Arc::new(move |through| {
            tell(host.clone(), through);
            if let Err(err) = waker.wake() {
                log::warn!("waking the event loop for a connection's way failed: {err}");
            }
        })
    }

    /// The connection to `host` goes `through` these machines now.
    pub(super) fn link_goes(&mut self, host: &str, through: Vec<MachineId>) {
        let Some(at) = self.links.find(host) else {
            return;
        };
        if self.links.list[at].through == through {
            self.links.list[at].ways.set_current(through);
            return;
        }
        let way = if through.is_empty() {
            "straight there".to_owned()
        } else {
            let names: Vec<String> = (through.iter())
                .map(|&machine| self.links.called(machine))
                .collect();
            format!("through {}", names.join(" and "))
        };
        log::info!("the connection to {host} goes {way} now");
        let linked = &mut self.links.list[at];
        linked.ways.set_current(through.clone());
        linked.through = through;
        linked.better_at = None;
        self.better_soon(at);
        self.tell_connections();
        self.sharing_changed();
        // Others' ways through it may go round a loop now, or no longer.
        self.sync_chained();
    }

    /// Notes that the connection at `at` is down: nothing can be carried
    /// on through it, or offered it, until it's made again.
    pub(super) fn link_down(&mut self, at: usize) {
        let linked = &mut self.links.list[at];
        linked.ways.connection_ended();
        linked.offerer_gone();
        linked.forwarder = None;
        linked.better_at = None;
        // A try straight there can be offered to nothing now: its ssh
        // stops, and it ends, saying so (see `went_direct`).
        self.stop_going_direct(at);
    }

    /// Has a better way for the connection at `at` tried in a while, if
    /// there may be one.
    pub(super) fn better_soon(&mut self, at: usize) {
        let every = (self.config.links.as_ref())
            .map_or(crate::DEFAULT_BETTER_EVERY, |config| config.better_every);
        let udp = self.on_udp(&self.links.list[at]);
        let linked = &mut self.links.list[at];
        if linked.connected.is_some()
            && !udp
            && linked.better_at.is_none()
            && linked.ways.has_better()
        {
            linked.better_at = Some(Instant::now() + every);
        }
    }

    /// Tries a better way for each connection whose time for it has come,
    /// on a thread of its own, offering what opens to its connection.
    pub(super) fn try_better_ways(&mut self) {
        let now = Instant::now();
        let every = (self.config.links.as_ref())
            .map_or(crate::DEFAULT_BETTER_EVERY, |config| config.better_every);
        for at in 0..self.links.list.len() {
            let used = self.used_for(&self.links.list[at].remote.host.clone());
            // Over UDP, it goes straight there already.
            let udp = self.on_udp(&self.links.list[at]);
            let linked = &mut self.links.list[at];
            if linked.better_at.is_none_or(|due| due > now) {
                continue;
            }
            linked.better_at = None;
            if udp {
                continue;
            }
            let Some(offerer) = linked.offerer() else {
                continue;
            };
            if !linked.ways.has_better() {
                continue;
            }
            linked.better_at = Some(now + every);
            if linked
                .bettering
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
            {
                continue;
            }
            let ways = Arc::clone(&linked.ways);
            let spawned = std::thread::Builder::new()
                .name("better-way".into())
                .spawn(move || {
                    if let Some(opened) = ways.open_better() {
                        let straight = opened.through.is_empty();
                        let carrier = super::links::ways::redialed(opened, &used);
                        match offerer.offer(carrier) {
                            // ssh straight there proves a key vouched for.
                            Ok((certificate, Some(signed))) if straight => {
                                ways.met(&certificate, &signed);
                            }
                            Ok(_) => {}
                            Err(err) => log::info!("a better way couldn't be offered: {err}"),
                        }
                    }
                });
            match spawned {
                Ok(thread) => {
                    if let Some(old) = linked.bettering.replace(thread) {
                        self.links.ending.push(old);
                    }
                }
                Err(err) => log::warn!("there's no thread to try a better way on: {err}"),
            }
        }
    }

    /// Tries ssh straight to the machine of the connection at `at`, which
    /// goes through another machine, as the client at `token` asked
    /// (`:connect HOST` again): asking that client ssh's questions, such
    /// as a password, which the tries for a better way never ask
    /// (CLACKA-118). What opens is offered to the connection, which goes
    /// straight there without its panes noticing, its machine's key met;
    /// kept as asked for here, from then on a fresh connection to it tries
    /// ssh straight there first, asking too. The client is told how it
    /// went. Turned down, or with nobody to ask, only this try fails.
    pub(super) fn go_direct(&mut self, at: usize, token: Token) {
        let host = self.links.list[at].remote.host.clone();
        let used = self.used_for(&host);
        let tell = self.links.direct_teller(Arc::clone(&self.waker));
        let linked = &mut self.links.list[at];
        if linked.going_direct.is_some() {
            linked.going_direct = Some(token);
            return;
        }
        let Some(offerer) = linked.offerer() else {
            return;
        };
        linked.going_direct = Some(token);
        linked.direct_asked = false;
        let ways = Arc::clone(&linked.ways);
        let outcome = DirectOutcome::new(move |outcome| tell(host, outcome));
        let spawned = std::thread::Builder::new()
            .name("direct-way".into())
            .spawn(move || {
                let opened = match ways.open_direct() {
                    None => Err("it's reached only through another machine".to_owned()),
                    Some(Err(err)) => Err(err.to_string()),
                    Some(Ok(opened)) => Ok(opened),
                };
                let opened = match opened {
                    Ok(opened) => opened,
                    Err(why) => return outcome.say(Err(why)),
                };
                // Gone straight there once the connection takes it on;
                // let go, as when it reaches another run there, not.
                let taking = Taking {
                    outcome: outcome.clone(),
                    used: false,
                };
                let carrier = super::links::ways::redialed_then(opened, &used, move || {
                    let mut taking = taking;
                    taking.used = true;
                    taking.outcome.say(Ok(()));
                });
                match offerer.offer(carrier) {
                    Ok((certificate, signed)) => {
                        if let Some(signed) = signed {
                            ways.met(&certificate, &signed);
                        }
                        outcome.offered();
                    }
                    Err(err) => outcome.say(Err(err.to_string())),
                }
            });
        match spawned {
            Ok(thread) => {
                if let Some(old) = linked.bettering.replace(thread) {
                    self.links.ending.push(old);
                }
            }
            Err(err) => {
                linked.going_direct = None;
                log::warn!("there's no thread to try ssh straight there on: {err}");
            }
        }
    }

    /// Ends a try of ssh straight to the machine of the connection at
    /// `at`, as a client asked, if one runs: its ssh is stopped, and the
    /// try says how it went as it ends, which tells that client.
    pub(super) fn stop_going_direct(&mut self, at: usize) {
        let linked = &mut self.links.list[at];
        if linked.going_direct.is_none() {
            return;
        }
        // Its ssh is the connection's, as a carrier it opens becomes: the
        // connection has ended, so what stops it can stop the try, and
        // another stops what's opened from now on.
        let stopped = std::mem::take(&mut linked.cancel);
        stopped.cancel();
        if linked.asked {
            self.set_direct(at);
        }
    }

    /// ssh straight to `host`, tried as a client asked, came to `outcome`:
    /// that client is told. Questions it asked still waiting go.
    pub(super) fn went_direct(&mut self, host: &str, outcome: Result<(), String>) {
        let Some(at) = self.links.find(host) else {
            return;
        };
        let Some(token) = self.links.list[at].going_direct.take() else {
            return;
        };
        // One that asked is tried in the background no more: those tries
        // never ask, so they'd only fail, a login at a time on that
        // machine, until a client asks again (CLACKA-118).
        let asked = outcome.is_err() && self.links.list[at].direct_asked;
        if self.links.list[at].ways.asks_direct() != asked {
            // Kept, so a restart doesn't try it in the background again.
            self.keep_soon();
        }
        self.links.list[at].ways.direct_asks(asked);
        if asked {
            self.links.list[at].better_at = None;
            self.better_soon(at);
        }
        let said = match outcome {
            Ok(()) => {
                log::info!("the connection to {host} goes straight there now, as asked");
                format!("‘{host}’ goes straight there now")
            }
            Err(why) => {
                log::info!("ssh couldn't go straight to {host}, as asked: {why}");
                let through = self.links.list[at].through.clone();
                let names: Vec<String> = (through.iter())
                    .map(|&machine| self.links.called(machine))
                    .collect();
                if names.is_empty() {
                    format!("ssh couldn't go straight to ‘{host}’: {why}")
                } else {
                    format!(
                        "ssh couldn't go straight to ‘{host}’, which still goes through {}: {why}",
                        names.join(" and ")
                    )
                }
            }
        };
        self.take_back_probes(host);
        self.send_event(token, clackatoo_protocol::Event::Said(said));
    }

    /// Takes over what the connection to `machine` through others showed,
    /// for the connection at `at`, asked for here, which has just reached
    /// that machine straight: that connection ends, and what it relayed
    /// is relayed through this one from now on, numbered as it was, as
    /// though it had dropped and been made again. Returns where the
    /// connection at `at` is in the list now.
    pub(super) fn take_over_passed(&mut self, at: usize, machine: MachineId) -> usize {
        if !self.links.list[at].asked {
            return at;
        }
        let Some(other) = (self.links.list.iter().enumerate()).position(|(other, linked)| {
            other != at && !linked.asked && linked.machine_id() == Some(machine)
        }) else {
            return at;
        };
        if let Some((token, _)) = self.links.list[other].connected.take()
            && let Some(Source::Reached(mut reached)) = self.sources.try_remove(token.0)
        {
            let _ = self.poll.registry().deregister(&mut reached.stream);
            self.unread
                .retain(|&unread| unread != Flood::Reached(token));
            self.let_carried_go(reached.carried.take());
            self.relay_lost(other, reached.upstream);
        }
        self.link_down(other);
        let relayed = std::mem::take(&mut self.links.list[other].relayed);
        let removed = self.links.list.remove(other);
        removed.ways.end();
        removed.cancel.cancel();
        let at = if other < at { at - 1 } else { at };
        log::info!(
            "{} is reached straight now, in place of {} through others: what it showed is shown \
             through this connection",
            self.links.list[at].remote.host,
            removed.remote.host
        );
        self.links.retire(removed);
        self.links.list[at].relayed = relayed;
        at
    }

    /// Where a stream on the QUIC connection with `handle`, asking to
    /// reach the daemon on `machine`, goes (see [`Request::Reach`]): to
    /// this daemon itself, as a new client of it, or on through this
    /// daemon's connection to that machine, if it passes it on; or why
    /// it can't. A way back through the asking daemon's own machine,
    /// round a loop, is refused.
    pub(super) fn reaching(
        &mut self,
        handle: ConnectionHandle,
        machine: MachineId,
    ) -> Result<Reaching, Error> {
        let asker = (self.quic.as_ref())
            .and_then(|quic| quic.client_of(handle))
            .and_then(|token| match self.sources.get(token.0) {
                Some(Source::Client(client)) => client.machine.as_ref().map(|machine| machine.id),
                _ => None,
            });
        // Only a daemon that said which machine it is, so a way back
        // through it, round a loop, is always found.
        let Some(asker) = asker else {
            return Err(Error::Other(
                "only a daemon that said which machine it is reaches another through this one"
                    .to_owned(),
            ));
        };
        let io =
            |err: std::io::Error| Error::Other(format!("the daemon couldn't make a socket: {err}"));
        let impostor = (self.config.links.as_ref()).is_some_and(|config| config.impostor);
        if machine == self.machine.id || impostor {
            // Fed straight to QUIC, which may only greet this daemon and
            // carry QUIC; or, as a test's daemon in the way, sign as the
            // machine asked for.
            return Ok(Reaching::Here {
                carrier_as: (machine != self.machine.id).then_some(machine),
            });
        }
        let own = &self.machine.name;
        // Kept from it, it's told nothing of which machine (CLACKA-118).
        let kept = self.kept_but(Some(asker));
        if kept.machine(machine) {
            return Err(super::relay::unshared::not_shared());
        }
        // The connected connection to it, if there are two, and never one
        // stopped.
        let to_it = |linked: &&Linked| {
            (linked.machine.as_ref()).is_some_and(|known| known.id == machine)
                && !matches!(linked.state, ConnectionState::Stopped { .. })
        };
        let at = (self.links.list.iter().enumerate())
            .filter(|(_, linked)| to_it(linked))
            .max_by_key(|(_, linked)| linked.connected.is_some())
            .map(|(at, _)| at);
        let Some(at) = at else {
            return Err(Error::Other(format!(
                "‘{own}’ isn't connected to that machine, so it doesn't pass it on"
            )));
        };
        let linked = &self.links.list[at];
        if kept.link(at) {
            return Err(super::relay::unshared::not_shared());
        }
        let host = &linked.remote.host;
        if asker == machine || linked.through.contains(&asker) {
            return Err(Error::Other(format!(
                "‘{own}’ reaches ‘{host}’ through the machine asking, round a loop"
            )));
        }
        let Some(forwarder) = linked.forwarder.clone() else {
            return Err(Error::Other(format!(
                "‘{own}’ isn't connected to ‘{host}’ now"
            )));
        };
        let wire = forwarder.wire();
        if !wire::daemon_has(wire, wire::CHAIN) {
            return Err(Error::Other(format!(
                "‘{host}’ runs a clackatoo too old to be reached through ‘{own}’"
            )));
        }
        let request = ClientMessage::Request(Request::Reach { machine });
        let request = frame::encode(wire, &request)
            .map_err(|err| Error::Other(format!("the request couldn't be encoded: {err}")))?;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().map_err(io)?;
        forwarder.carry(theirs);
        Ok(Reaching::On {
            socket: ours,
            request,
            wire,
            via: host.clone(),
        })
    }
}

/// `host`, a connection's, as the machine is named when it's passed on:
/// without the user ssh logs in as or a port, which are this machine's
/// business (`jack@server:2222` is `server`).
pub(super) fn bare_host(host: &str) -> String {
    let machine = host.rsplit_once('@').map_or(host, |(_, machine)| machine);
    if let Some(inner) = machine.strip_prefix('[') {
        return inner.split(']').next().unwrap_or(inner).to_owned();
    }
    match machine.split_once(':') {
        // One colon: a port. More: an IPv6 address, as it is.
        Some((name, port)) if !port.contains(':') => name.to_owned(),
        _ => machine.to_owned(),
    }
}

/// `name`, what a daemon calls a machine it passes on, as this one shows
/// it: made safe to show, without the user ssh logs in as (`jack@server`
/// is `server`), which would read as yet another machine after a
/// workspace's name; or, if nothing's left, the start of its ID.
fn passed_name(name: &str, machine: MachineId) -> String {
    let shown = shown_name(name);
    let host = shown
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if host.is_empty() {
        machine.to_string()[..8].to_owned()
    } else {
        host
    }
}

/// Says how a try of ssh straight there, as a client asked, went, once:
/// the first of the try failing, and, once its carrier was offered, the
/// connection taking it on or letting it go.
#[derive(Clone)]
struct DirectOutcome(Arc<std::sync::Mutex<Outcome>>);

/// What tells how a try straight there went.
type Tell = Box<dyn FnOnce(Result<(), String>) + Send>;

/// What [`DirectOutcome`] knows.
struct Outcome {
    /// Who to tell, until told.
    tell: Option<Tell>,
    /// The carrier was offered to the connection.
    offered: bool,
    /// The connection let the carrier go without taking it on.
    let_go: bool,
}

impl DirectOutcome {
    fn new(tell: impl FnOnce(Result<(), String>) + Send + 'static) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Outcome {
            tell: Some(Box::new(tell)),
            offered: false,
            let_go: false,
        })))
    }

    /// Tells `outcome`, unless something was told already.
    fn say(&self, outcome: Result<(), String>) {
        let tell = (self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
        .tell
        .take();
        if let Some(tell) = tell {
            tell(outcome);
        }
    }

    /// The carrier was offered: if the connection let it go meanwhile,
    /// that's told now.
    fn offered(&self) {
        let let_go = {
            let mut outcome = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            outcome.offered = true;
            outcome.let_go
        };
        if let_go {
            self.say(Err(NOT_TAKEN.to_owned()));
        }
    }
}

/// Why a carrier straight there wasn't taken on.
const NOT_TAKEN: &str =
    "it reached another run of clackatoo there, so the connection didn't take it";

/// Goes with a carrier straight there offered to its connection: once
/// it's let go without being taken on, that's told, if it was offered.
struct Taking {
    outcome: DirectOutcome,
    used: bool,
}

impl Drop for Taking {
    fn drop(&mut self) {
        if self.used {
            return;
        }
        let offered = {
            let mut outcome =
                (self.outcome.0.lock()).unwrap_or_else(std::sync::PoisonError::into_inner);
            outcome.let_go = true;
            outcome.offered
        };
        if offered {
            self.outcome.say(Err(NOT_TAKEN.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_passed_on_without_its_user_or_port() {
        assert_eq!(bare_host("jack@server:2222"), "server");
        assert_eq!(bare_host("server"), "server");
        assert_eq!(bare_host("[fe80::1]:22"), "fe80::1");
        assert_eq!(bare_host("fe80::1"), "fe80::1");
        assert_eq!(bare_host("jack@desk"), "desk");
    }

    #[test]
    fn a_name_passed_on_is_its_machine_alone_and_safe_to_show() {
        let id = MachineId([0xab; 16]);
        assert_eq!(passed_name("jack@server", id), "server");
        assert_eq!(passed_name("ser\u{202e}ver\x1b", id), "server");
        assert_eq!(passed_name("jack@", id), "abababab");
        assert_eq!(passed_name("nas:2222", id), "nas:2222");
    }
}
