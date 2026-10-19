//! The ways a connection to another machine goes (CLACKA-117): ssh
//! straight there, for a connection asked for here, or through a daemon
//! connected to this one that passes the machine on, which carries it on
//! through its own connection there (see [`Request::Reach`]). Either way
//! the connection is this daemon's own, to the daemon there, as a client
//! of it: only what carries its QUIC packets differs, so a connection can
//! move from one way to another without ending, and nothing it relays
//! notices.
//!
//! The loop keeps [`Ways`] up to date as connections come and go and
//! what they pass on changes; the threads that open carriers read it:
//!
//! - **A new connection** tries ssh first, as the shortest way, unless it
//!   failed lately, and then the ways through other daemons, fewest
//!   machines between first.
//! - **A carrier lost,** while the QUIC connection lives on, is replaced
//!   by another way first, and only then the one that was lost, as that's
//!   the one that just failed: a connection whose ssh drops moves through
//!   another daemon at once. With no way at all, it waits a while for
//!   one to come.
//! - **A better way,** ssh straight there or fewer machines between, is
//!   tried now and then while a worse one is in use, and offered to the
//!   connection, which moves onto it (see
//!   [`Offerer`](crate::quic::Offerer)). ssh tried so never asks a
//!   question: a password is asked for only when there's no other way.
//!
//! # Who's there
//!
//! A daemon a way goes through carries the stream on, and could answer in
//! the machine's place, with a QUIC certificate of its own. So a new
//! connection checks the certificate it's given against the identity key
//! it knows the machine by ([`Pins`]), as its daemon signs its certificate
//! with it (see `crate::machine`):
//!
//! - **ssh straight there** proves itself, as it logged in to that
//!   machine: its key is taken, and kept, as met. A key that differs from
//!   one kept is refused, loudly: the machine's daemon chose a new one,
//!   and `clackatoo forget HOST` takes it, or something is in the way.
//! - **Through another daemon,** the certificate must be signed by the key
//!   kept for the machine. For a connection asked for here, that key must
//!   have been met straight. For one to a machine only passed on, never
//!   met, the daemon passing it on vouches for the key it knows it by, the
//!   first time, and that key is kept from then on: trust on first use,
//!   through that daemon, which could lie then, and only then. A way
//!   through another daemon that answers with a key other than the one
//!   kept is refused, loudly, and that way alone is ruled out: ssh
//!   straight there is still tried, and the connection is stopped only
//!   when that's what differs.
//!
//! A carrier that replaces another, or a better way offered, must present
//! the very certificate the connection was made with, so a daemon on the
//! way can only pass it on, hold it up, or drop it.

use std::ffi::OsString;
use std::io;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use clackatoo_protocol::{MachineId, Signed};

use crate::client::{self, ClientError};
use crate::forward::Forwarder;
use crate::known::Known;
use crate::quic::{Redial, Redialed};
use crate::remote::{self, Asking, Cancel, Remote, RemoteError, Ssh};

/// The identity keys this daemon knows other machines by, shared by every
/// connection's threads: kept from one run to the next (see
/// [`crate::known`]).
pub(in crate::server) type Pins = Arc<Known>;

pub(in crate::server) use crate::known::Pin;

/// Why a connection won't take the daemon it reached.
#[derive(Debug)]
pub(in crate::server) struct Untrusted {
    pub(in crate::server) reason: String,
    /// It's not merely unproven but another than the one known: said
    /// loudly, and not tried again until asked.
    pub(in crate::server) loud: bool,
}

/// How long ssh straight there is tried after the other ways, once it
/// has failed.
const DIRECT_FAILED_FOR: Duration = Duration::from_mins(1);

/// How long a carrier lost waits for a way to come, when there's none,
/// before saying it failed, to be tried again a while later.
const WAY_WAIT: Duration = Duration::from_secs(20);

/// The ways a connection may go, for its threads to read: see the module
/// docs.
#[derive(Debug, Default)]
pub(in crate::server) struct Ways {
    state: Mutex<State>,
    /// Told whenever the ways change, for a carrier lost waiting for one.
    changed: Condvar,
    /// The keys machines are known by.
    pins: Pins,
}

/// What [`Ways`] holds.
#[derive(Debug, Default)]
struct State {
    /// ssh straight there, for a connection asked for here.
    direct: Option<Direct>,
    /// Through the daemons connected to this one that pass the machine
    /// on, best first.
    through: Vec<Way>,
    /// The machine, once known: what a daemon a way goes through is asked
    /// to reach.
    machine: Option<MachineId>,
    /// What it's called here, for what's said when a way fails.
    name: String,
    /// The way the connection's carrier goes, as far as known: the
    /// machines it goes through, none for ssh straight there.
    current: Option<Vec<MachineId>>,
    /// When ssh straight there last failed.
    direct_failed: Option<Instant>,
    /// Counts changes, for a carrier lost waiting for one to tell.
    generation: u64,
    /// Counts the connections that have ended, so the threads of one
    /// that has stop opening carriers for it.
    epoch: u64,
    /// The connection has ended: nothing waits for a way any more.
    ended: bool,
}

/// ssh straight there: where, how, and what asks its questions.
#[derive(Clone, Debug)]
pub(in crate::server) struct Direct {
    pub(in crate::server) remote: Remote,
    pub(in crate::server) ssh: OsString,
    pub(in crate::server) asking: Option<Asking>,
    pub(in crate::server) cancel: Arc<Cancel>,
}

/// A way through a daemon connected to this one.
#[derive(Clone, Debug)]
pub(in crate::server) struct Way {
    /// The machines it goes through, nearest first: the nearest is the
    /// one this daemon's connection carrying it goes to.
    pub(in crate::server) through: Vec<MachineId>,
    /// What the nearest is called here.
    pub(in crate::server) via: String,
    /// That connection's.
    pub(in crate::server) forwarder: Forwarder,
    /// The key the nearest's daemon knows the machine by, vouching for it.
    pub(in crate::server) vouched: Option<[u8; 32]>,
}

impl Way {
    /// Whether `other` is the same way, through the same connection.
    fn same(&self, other: &Self) -> bool {
        self.through == other.through && self.forwarder.same(&other.forwarder)
    }
}

/// One way to try.
#[derive(Clone, Debug)]
pub(in crate::server) enum Attempt {
    Direct(Direct),
    Through(Way, MachineId),
}

impl Attempt {
    /// The machines it goes through.
    fn through(&self) -> Vec<MachineId> {
        match self {
            Self::Direct(_) => Vec::new(),
            Self::Through(way, _) => way.through.clone(),
        }
    }
}

/// A carrier opened, and what came with it.
pub(in crate::server) struct Opened {
    pub(in crate::server) stream: UnixStream,
    /// The ssh carrying it, if it's ssh's.
    pub(in crate::server) ssh: Option<Ssh>,
    /// Where ssh says it reached the machine, for QUIC to try UDP to.
    pub(in crate::server) address: Option<std::net::IpAddr>,
    /// The key the daemon it goes through vouched for the machine, for a
    /// way through one.
    pub(in crate::server) vouched: Option<[u8; 32]>,
    /// The machines it goes through.
    pub(in crate::server) through: Vec<MachineId>,
}

impl Ways {
    /// Ways checking who's there by `pins`.
    pub(in crate::server) fn new(pins: &Pins) -> Self {
        Self {
            pins: Arc::clone(pins),
            ..Self::default()
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the ways with `change`, waking whatever waits for one, if
    /// it says it changed them.
    fn change(&self, change: impl FnOnce(&mut State) -> bool) {
        let mut state = self.state();
        if !change(&mut state) {
            return;
        }
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        self.changed.notify_all();
    }

    /// Sets ssh straight there, or with `None`, takes it away.
    pub(in crate::server) fn set_direct(&self, direct: Option<Direct>) {
        self.change(|state| {
            state.direct = direct;
            true
        });
    }

    /// Sets the machine, if known, what it's called here, and the ways
    /// through other daemons to it, best first.
    pub(in crate::server) fn set_through(
        &self,
        machine: Option<MachineId>,
        name: &str,
        through: Vec<Way>,
    ) {
        self.change(|state| {
            let same = state.machine == machine
                && state.through.len() == through.len()
                && (state.through.iter().zip(&through)).all(|(old, new)| old.same(new));
            state.machine = machine;
            name.clone_into(&mut state.name);
            state.through = through;
            !same
        });
    }

    /// Notes that the connection's carrier goes `through` these machines
    /// now.
    pub(in crate::server) fn set_current(&self, through: Vec<MachineId>) {
        self.state().current = Some(through);
    }

    /// The connection in use, by which a thread opening carriers for it
    /// knows when it has ended (see [`connection_ended`](Self::connection_ended)).
    pub(in crate::server) fn epoch(&self) -> u64 {
        self.state().epoch
    }

    /// Notes that the connection in use has ended: its threads open no
    /// more carriers for it, and stop waiting for a way.
    pub(in crate::server) fn connection_ended(&self) {
        self.change(|state| {
            state.epoch = state.epoch.wrapping_add(1);
            true
        });
    }

    /// Ends the ways, as the connection has: nothing waits for one now.
    pub(in crate::server) fn end(&self) {
        self.change(|state| {
            state.ended = true;
            true
        });
    }

    /// Whether a better way than the one in use is there to try.
    pub(in crate::server) fn has_better(&self) -> bool {
        !self.better().is_empty()
    }

    /// The ways better than the one in use, best first: ssh straight
    /// there, while it goes through another daemon, and ways through
    /// fewer machines.
    fn better(&self) -> Vec<Attempt> {
        let state = self.state();
        let Some(current) = &state.current else {
            return Vec::new();
        };
        let mut better = Vec::new();
        if !current.is_empty()
            && let Some(direct) = &state.direct
        {
            better.push(Attempt::Direct(direct.clone()));
        }
        if let Some(machine) = state.machine {
            better.extend(
                (state.through.iter())
                    .filter(|way| !current.is_empty() && way.through.len() < current.len())
                    .map(|way| Attempt::Through(way.clone(), machine)),
            );
        }
        better
    }

    /// The ways for a new connection to try, in turn: ssh first, unless
    /// it failed lately, then the ways through other daemons.
    fn for_new(&self) -> Vec<Attempt> {
        let state = self.state();
        let through = state.machine.into_iter().flat_map(|machine| {
            (state.through.iter()).map(move |way| Attempt::Through(way.clone(), machine))
        });
        let mut attempts: Vec<Attempt> = through.collect();
        if let Some(direct) = &state.direct {
            let failed = (state.direct_failed).is_some_and(|at| at.elapsed() < DIRECT_FAILED_FOR);
            let direct = Attempt::Direct(direct.clone());
            if failed {
                attempts.push(direct);
            } else {
                attempts.insert(0, direct);
            }
        }
        attempts
    }

    /// The ways for a carrier lost to try, in turn, and the generation
    /// they're of: every way but the one in use first, as that's the one
    /// that failed, then that one.
    fn for_lost(&self) -> (Vec<Attempt>, u64) {
        let mut attempts = self.for_new();
        let state = self.state();
        if let Some(current) = &state.current
            && let Some(at) = (attempts.iter()).position(|attempt| attempt.through() == *current)
        {
            let lost = attempts.remove(at);
            attempts.push(lost);
        }
        (attempts, state.generation)
    }

    /// Waits for the ways to change from generation `generation`, at most
    /// `wait`. Returns whether they did, and neither the connection of
    /// `epoch` nor every connection has ended.
    fn wait_for_change(&self, generation: u64, epoch: u64, wait: Duration) -> bool {
        let state = self.state();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, wait, |state| {
                state.generation == generation && !state.ended && state.epoch == epoch
            })
            .unwrap_or_else(PoisonError::into_inner);
        state.generation != generation && !state.ended && state.epoch == epoch
    }

    /// Whether the connection of `epoch` has ended.
    fn ended(&self, epoch: u64) -> bool {
        let state = self.state();
        state.ended || state.epoch != epoch
    }

    /// Opens a carrier `attempt`'s way, with ssh asking no question if
    /// `batch`.
    fn open(&self, attempt: &Attempt, batch: bool) -> Result<Opened, RemoteError> {
        match attempt {
            Attempt::Direct(direct) => {
                let dialed = remote::dial_ssh(
                    &direct.remote,
                    &direct.ssh,
                    direct.asking.as_ref(),
                    Some(&direct.cancel),
                    batch,
                );
                match dialed {
                    Ok((stream, ssh, address)) => Ok(Opened {
                        stream,
                        ssh: Some(ssh),
                        address,
                        through: Vec::new(),
                        vouched: None,
                    }),
                    Err(err) => {
                        self.state().direct_failed = Some(Instant::now());
                        Err(err)
                    }
                }
            }
            Attempt::Through(way, machine) => {
                let name = self.state().name.clone();
                let stream = carrier_through(way, *machine, &name)?;
                Ok(Opened {
                    stream,
                    ssh: None,
                    address: None,
                    through: way.through.clone(),
                    vouched: way.vouched,
                })
            }
        }
    }

    /// The ways for a new connection to try, in turn.
    pub(in crate::server) fn attempts(&self) -> Vec<Attempt> {
        self.for_new()
    }

    /// Opens a carrier `attempt`'s way.
    pub(in crate::server) fn open_attempt(&self, attempt: &Attempt) -> Result<Opened, RemoteError> {
        self.open(attempt, false)
    }

    /// Whether a new connection by the way `through`, which a daemon on
    /// the way `vouched` for, if it did, takes the daemon that presented
    /// `certificate`, `signed` as it says: see the module docs. Returns
    /// the machine and the key to keep it by, if it proved itself.
    ///
    /// # Errors
    ///
    /// Why not, if it doesn't.
    pub(in crate::server) fn trusts(
        &self,
        through: &[MachineId],
        vouched: Option<[u8; 32]>,
        certificate: &[u8],
        signed: Option<&Signed>,
    ) -> Result<Option<(MachineId, Pin)>, Untrusted> {
        let state = self.state();
        let name = &state.name;
        let quiet = |reason: String| Untrusted {
            reason,
            loud: false,
        };
        let loud = |reason: String| Untrusted { reason, loud: true };
        let Some(signed) = signed else {
            // A daemon before identity keys, which ssh proves.
            return if through.is_empty() {
                Ok(None)
            } else {
                Err(quiet(format!(
                    "‘{name}’ can't prove which machine it is, as its clackatoo is too old"
                )))
            };
        };
        let machine = signed.machine;
        if !crate::machine::verify(&signed.key, machine, certificate, &signed.signature) {
            return Err(loud(format!(
                "‘{name}’'s certificate isn't signed by the key that came with it: something on \
                 the way changed it"
            )));
        }
        let pinned = self.pins.get(machine);
        let forget = format!(
            "if its daemon chose a new key, as when its `machine` file was deleted, \
             `clackatoo forget {name}` takes it"
        );
        if through.is_empty() {
            return match pinned {
                Some(pin) if pin.key != signed.key && pin.met => Err(loud(format!(
                    "‘{name}’ shows another identity key than it did: {forget}; otherwise \
                     something is in the way"
                ))),
                Some(pin) if pin.key != signed.key => Err(loud(format!(
                    "‘{name}’ shows another identity key than the one a machine passing it on \
                     vouched for: {forget}"
                ))),
                _ => Ok(Some((
                    machine,
                    Pin {
                        key: signed.key,
                        met: true,
                    },
                ))),
            };
        }
        if state.machine.is_some_and(|expected| expected != machine) {
            return Err(loud(format!(
                "another machine than ‘{name}’ answered through the machines on the way"
            )));
        }
        match pinned {
            // Asked for here, it must have been met straight, not only
            // vouched for by a machine on the way.
            Some(pin) if pin.key == signed.key && !pin.met && state.direct.is_some() => {
                Err(quiet(format!(
                    "‘{name}’ has never been reached straight, so it can't be checked through \
                     another machine"
                )))
            }
            Some(pin) if pin.key == signed.key => Ok(Some((machine, pin))),
            Some(_) => Err(loud(format!(
                "‘{name}’, reached through another machine, shows another identity key than \
                 the one it's known by: {forget}; otherwise that machine is in the way"
            ))),
            None if state.direct.is_some() => Err(quiet(format!(
                "‘{name}’ has never been reached straight, so it can't be checked through \
                 another machine"
            ))),
            None => match vouched {
                Some(key) if key == signed.key => Ok(Some((machine, Pin { key, met: false }))),
                Some(_) => Err(loud(format!(
                    "‘{name}’ shows another identity key than the machine passing it on vouched \
                     for"
                ))),
                None => Err(quiet(format!(
                    "the machine passing ‘{name}’ on doesn't know its identity key"
                ))),
            },
        }
    }

    /// Notes that ssh straight there reached the machine whose daemon
    /// presented `certificate`, `signed` as it says: a key known only as
    /// vouched for is known as met from now on, if it's the one. Whatever
    /// machine it signs as, only a daemon holding the key that machine is
    /// known by can sign so, which ssh reached.
    pub(in crate::server) fn met(&self, certificate: &[u8], signed: &Signed) {
        let proved =
            crate::machine::verify(&signed.key, signed.machine, certificate, &signed.signature);
        if let Some(pin) = self.pins.get(signed.machine)
            && proved
            && pin.key == signed.key
            && !pin.met
        {
            let met = Pin {
                key: pin.key,
                met: true,
            };
            self.pins.insert(signed.machine, met);
        }
    }

    /// Opens a carrier by a better way than the one in use, if one is
    /// there and opens, with ssh asking no question.
    pub(in crate::server) fn open_better(&self) -> Option<Opened> {
        for attempt in self.better() {
            match self.open(&attempt, true) {
                Ok(opened) => return Some(opened),
                Err(err) => log::debug!("a better way to a machine isn't open yet: {err}"),
            }
        }
        None
    }

    /// Opens a carrier in place of one lost, for the connection of
    /// `epoch`: see [`for_lost`](Self::for_lost). With no way, waits a
    /// while for one to come. Once that connection has ended, opens none.
    fn open_lost(&self, epoch: u64) -> io::Result<Opened> {
        let ended = || io::Error::other("the connection has ended");
        loop {
            let (attempts, generation) = self.for_lost();
            let mut last = None;
            for attempt in &attempts {
                if self.ended(epoch) {
                    return Err(ended());
                }
                match self.open(attempt, false) {
                    Ok(opened) => return Ok(opened),
                    Err(err) => last = Some(err),
                }
            }
            if self.ended(epoch) {
                return Err(ended());
            }
            if !self.wait_for_change(generation, epoch, WAY_WAIT) {
                return Err(match last {
                    Some(err) => io::Error::other(err),
                    None => io::Error::other("there's no way there now"),
                });
            }
        }
    }
}

/// A carrier opened, as the QUIC connection takes it on, calling `used`
/// with the machines it goes through once it does.
pub(in crate::server) fn redialed(
    opened: Opened,
    used: &Arc<dyn Fn(Vec<MachineId>) + Send + Sync>,
) -> Redialed {
    let Opened {
        stream,
        ssh,
        through,
        address,
        ..
    } = opened;
    let carrier = match ssh {
        Some(ssh) => Redialed::keeping(stream, ssh),
        None => Redialed::new(stream),
    };
    let used = Arc::clone(used);
    carrier.reached_at(address).when_used(move || used(through))
}

/// How a connection going by `ways`, the one in use now, replaces a
/// carrier it lost, calling `used` with the machines each goes through as
/// it's taken on.
pub(in crate::server) fn redial(
    ways: Arc<Ways>,
    used: Arc<dyn Fn(Vec<MachineId>) + Send + Sync>,
) -> Redial {
    let epoch = ways.epoch();
    Box::new(move || Ok(redialed(ways.open_lost(epoch)?, &used)))
}

/// Opens a carrier through `way`, to `machine`, called `name`: a stream on
/// the connection to its nearest machine, whose daemon is asked to reach
/// `machine` with it. Returns it once that daemon answers that it did.
fn carrier_through(way: &Way, machine: MachineId, name: &str) -> Result<UnixStream, RemoteError> {
    client::reach(&way.forwarder, machine).map_err(|err| match err {
        ClientError::Daemon(err) => RemoteError::Unreachable {
            host: name.to_owned(),
            reason: format!("through ‘{}’: {err}", way.via),
        },
        ClientError::Disconnected => RemoteError::Unreachable {
            host: name.to_owned(),
            reason: format!(
                "through ‘{}’: the connection it goes through has ended",
                way.via
            ),
        },
        err => err.into(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    const DESK: MachineId = MachineId([0xde; 16]);
    const NAS: MachineId = MachineId([0x0a; 16]);
    const SERVER: MachineId = MachineId([0x5e; 16]);

    /// A way through `through`, by a connection whose thread nothing
    /// reads: these tests only order ways, never open them.
    fn way(through: &[MachineId]) -> Way {
        let (commands, _) = mpsc::channel();
        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(mio::Waker::new(poll.registry(), mio::Token(0)).unwrap());
        Way {
            through: through.to_vec(),
            via: "desk".into(),
            forwarder: Forwarder::new(commands, waker, clackatoo_protocol::PROTOCOL_VERSION),
            vouched: Some([7; 32]),
        }
    }

    fn direct() -> Direct {
        Direct {
            remote: Remote {
                host: "server".into(),
                command: "clackatoo".into(),
            },
            ssh: "ssh".into(),
            asking: None,
            cancel: Arc::default(),
        }
    }

    /// What `attempts` go through, in turn: `None` for ssh straight there.
    fn order(attempts: &[Attempt]) -> Vec<Option<Vec<MachineId>>> {
        (attempts.iter())
            .map(|attempt| match attempt {
                Attempt::Direct(_) => None,
                Attempt::Through(way, _) => Some(way.through.clone()),
            })
            .collect()
    }

    #[test]
    fn a_new_connection_tries_ssh_first_unless_it_failed_lately() {
        let ways = Ways::default();
        ways.set_direct(Some(direct()));
        let through = vec![way(&[DESK]), way(&[NAS, DESK])];
        ways.set_through(Some(SERVER), "server", through);
        assert_eq!(
            order(&ways.for_new()),
            [None, Some(vec![DESK]), Some(vec![NAS, DESK])]
        );
        ways.state().direct_failed = Some(Instant::now());
        assert_eq!(
            order(&ways.for_new()),
            [Some(vec![DESK]), Some(vec![NAS, DESK]), None]
        );
        // With no machine known, nothing is asked to reach it.
        ways.set_through(None, "server", vec![way(&[DESK])]);
        assert_eq!(order(&ways.for_new()), [None]);
    }

    #[test]
    fn a_carrier_lost_tries_every_other_way_before_its_own() {
        let ways = Ways::default();
        ways.set_direct(Some(direct()));
        ways.set_through(Some(SERVER), "server", vec![way(&[DESK])]);
        ways.set_current(Vec::new());
        let (lost, _) = ways.for_lost();
        assert_eq!(order(&lost), [Some(vec![DESK]), None]);
        ways.set_current(vec![DESK]);
        let (lost, _) = ways.for_lost();
        assert_eq!(order(&lost), [None, Some(vec![DESK])]);
    }

    #[test]
    fn a_better_way_is_ssh_or_fewer_machines_between() {
        let ways = Ways::default();
        ways.set_direct(Some(direct()));
        let through = vec![way(&[DESK]), way(&[NAS, DESK])];
        ways.set_through(Some(SERVER), "server", through);
        // Not known yet, or straight there: none.
        assert!(!ways.has_better());
        ways.set_current(Vec::new());
        assert!(!ways.has_better());
        ways.set_current(vec![NAS, DESK]);
        assert_eq!(order(&ways.better()), [None, Some(vec![DESK])]);
        ways.set_current(vec![DESK]);
        assert_eq!(order(&ways.better()), [None]);
        ways.set_direct(None);
        assert!(!ways.has_better());
    }

    /// A machine's daemon, and its certificate signed as it would be.
    fn signing() -> (MachineId, crate::machine::Signer, Signed) {
        let (machine, keys) = crate::machine::identity(None, None, None, 1);
        let certified = Signed {
            machine: machine.id,
            key: keys.key(),
            signature: keys.sign(machine.id, b"certificate"),
        };
        (machine.id, keys, certified)
    }

    #[test]
    fn ssh_proves_the_key_and_a_changed_one_is_refused_loudly() {
        let (machine, _, signed) = signing();
        let ways = Ways::default();
        let proved = ways
            .trusts(&[], None, b"certificate", Some(&signed))
            .unwrap();
        assert_eq!(
            proved,
            Some((
                machine,
                Pin {
                    key: signed.key,
                    met: true
                }
            ))
        );
        // Before keys, ssh is proof enough; through another, nothing is.
        assert_eq!(ways.trusts(&[], None, b"certificate", None).unwrap(), None);
        assert!(
            !ways
                .trusts(&[DESK], None, b"certificate", None)
                .unwrap_err()
                .loud
        );
        // Known by another key, met or vouched for: refused, loudly.
        for met in [true, false] {
            let pin = Pin { key: [9; 32], met };
            ways.pins.insert(machine, pin);
            let refused = ways
                .trusts(&[], None, b"certificate", Some(&signed))
                .unwrap_err();
            assert!(
                refused.loud && refused.reason.contains("forget"),
                "{refused:?}"
            );
        }
    }

    /// `machine`'s ID on a certificate signed, validly, by another key: as
    /// a daemon in the way would sign in its place.
    fn signed_as(machine: MachineId) -> Signed {
        let (_, other) = crate::machine::identity(None, None, None, 1);
        Signed {
            machine,
            key: other.key(),
            signature: other.sign(machine, b"certificate"),
        }
    }

    #[test]
    fn another_key_signing_as_the_machine_is_refused_whatever_it_was_known_by() {
        let (machine, _, real) = signing();
        let forged = signed_as(machine);
        let ways = Ways::default();
        ways.set_through(Some(machine), "server", vec![way(&[DESK])]);
        for met in [true, false] {
            let pin = Pin { key: real.key, met };
            ways.pins.insert(machine, pin);
            for through in [&[][..], &[DESK][..]] {
                let refused = ways
                    .trusts(through, Some(forged.key), b"certificate", Some(&forged))
                    .unwrap_err();
                assert!(refused.loud, "{refused:?}");
                assert!(
                    refused.reason.contains("another identity key"),
                    "{refused:?}"
                );
            }
            // The real one still is.
            let taken = ways.trusts(&[DESK], None, b"certificate", Some(&real));
            assert!(taken.is_ok(), "{taken:?}");
        }
    }

    #[test]
    fn ssh_straight_there_makes_a_vouched_for_key_met() {
        let (machine, _, real) = signing();
        let ways = Ways::default();
        ways.pins.insert(
            machine,
            Pin {
                key: real.key,
                met: false,
            },
        );
        // Not another key.
        ways.met(b"certificate", &signed_as(machine));
        assert!(!ways.pins.get(machine).unwrap().met);
        ways.met(b"certificate", &real);
        assert!(ways.pins.get(machine).unwrap().met);
    }

    #[test]
    fn a_signature_that_isnt_the_keys_is_refused_whatever_the_way() {
        let (_, _, mut signed) = signing();
        let ways = Ways::default();
        // Another certificate in its place, with the machine's signature.
        let refused = ways
            .trusts(&[], None, b"certificat3", Some(&signed))
            .unwrap_err();
        assert!(refused.loud, "{refused:?}");
        // Another key, with the machine's signature.
        signed.key = [3; 32];
        assert!(
            ways.trusts(&[DESK], Some([3; 32]), b"certificate", Some(&signed))
                .is_err()
        );
    }

    #[test]
    fn through_another_the_key_known_or_vouched_for_is_needed() {
        let (machine, _, signed) = signing();
        let ways = Ways::default();
        ways.set_through(Some(machine), "server", vec![way(&[DESK])]);
        // Never met: what the daemon passing it on vouches for, kept.
        assert_eq!(
            ways.trusts(&[DESK], Some(signed.key), b"certificate", Some(&signed))
                .unwrap(),
            Some((
                machine,
                Pin {
                    key: signed.key,
                    met: false
                }
            ))
        );
        let lied = ways.trusts(&[DESK], Some([1; 32]), b"certificate", Some(&signed));
        assert!(lied.unwrap_err().loud);
        assert!(
            !ways
                .trusts(&[DESK], None, b"certificate", Some(&signed))
                .unwrap_err()
                .loud
        );
        // Known: that key, whatever's vouched for.
        let pin = Pin {
            key: signed.key,
            met: true,
        };
        ways.pins.insert(machine, pin);
        assert_eq!(
            ways.trusts(&[DESK], Some([1; 32]), b"certificate", Some(&signed))
                .unwrap(),
            Some((machine, pin))
        );
        // Another machine answering in its place.
        let (_, _, other) = signing();
        let refused = ways.trusts(&[DESK], Some(other.key), b"certificate", Some(&other));
        assert!(refused.unwrap_err().loud);
        // Asked for here and never met: nothing vouched for will do, kept
        // or not.
        ways.pins.remove(machine);
        ways.set_direct(Some(direct()));
        let refused = ways.trusts(&[DESK], Some(signed.key), b"certificate", Some(&signed));
        assert!(!refused.unwrap_err().loud);
        let vouched = Pin {
            key: signed.key,
            met: false,
        };
        ways.pins.insert(machine, vouched);
        let refused = ways.trusts(&[DESK], Some(signed.key), b"certificate", Some(&signed));
        assert!(!refused.unwrap_err().loud);
    }

    #[test]
    fn the_ways_change_only_when_they_do() {
        let ways = Ways::default();
        let desk = way(&[DESK]);
        ways.set_through(Some(SERVER), "server", vec![desk.clone()]);
        let before = ways.state().generation;
        // The same way, through the same connection: no change.
        ways.set_through(Some(SERVER), "server", vec![desk]);
        assert_eq!(ways.state().generation, before);
        // Through desk's connection made again: a change.
        ways.set_through(Some(SERVER), "server", vec![way(&[DESK])]);
        assert_ne!(ways.state().generation, before);
        // A connection ended: its threads stop waiting for one.
        let epoch = ways.epoch();
        assert!(!ways.ended(epoch));
        ways.connection_ended();
        assert!(ways.ended(epoch));
        let generation = ways.state().generation;
        assert!(!ways.wait_for_change(generation, epoch, Duration::ZERO));
    }
}
