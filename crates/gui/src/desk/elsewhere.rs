//! Which machine panes are on, and where new ones go (CLACKA-117, phase
//! 4).
//!
//! A pane is on its workspace's machine, or on another if it says so
//! ([`Pane::home`](clackatoo_protocol::Pane::home)): a workspace of the
//! daemon's own can show a pane on a machine it's connected to. Anything
//! made from a pane, a split, a new tab, a file opened beside, is made on
//! that pane's machine, so a window on a laptop working in a desk pane
//! keeps working on desk without asking. `:Terminal@desk`, and `@host`
//! after any kind of pane, makes it on the machine named instead; `@local`,
//! or this machine's own name, is the daemon's machine. The daemon does
//! the rest: in a workspace of its own, a pane on another machine is one
//! the daemon there lends it; in a workspace from another machine, the
//! daemon there makes it, and one of a third machine is refused, for now.
//!
//! `:ws new@desk NAME`, `:ws NAME@desk` for one that isn't there, and the
//! bar's `+`, which asks which machine once the daemon is connected to
//! another, make a workspace on another machine, which lives there.

use clackatoo_protocol::{
    ConnectionState, MachineId, NewTerminal, PaneId, PaneTerminal, Request, wire,
};

use super::Desk;
use super::connecting::Elsewhere;
use crate::command::Command;

impl Desk {
    /// The machine `pane` is on, if it's another than the daemon's: a
    /// pane of the daemon's own machine in a workspace of another's, which
    /// that machine's daemon can't reach, is on the daemon's. A background
    /// tab's pane is on its workspace's machine, as it holds a terminal of
    /// that machine's own.
    pub(crate) fn pane_machine(&self, pane: PaneId) -> Option<MachineId> {
        let Some((workspace, _, pane)) = self.workspaces.find_pane(pane) else {
            let (workspace, ..) = self.workspaces.find_background_pane(pane)?;
            return workspace.home;
        };
        let own = self.own_machine().map(|own| own.id);
        match pane.home {
            Some(home) if Some(home) == own => None,
            home => home.or(workspace.home),
        }
    }

    /// Where `pane` is, if it's on another machine than the daemon's: the
    /// connection to it, as the daemon last said.
    pub(crate) fn pane_elsewhere(&self, pane: PaneId) -> Option<Elsewhere> {
        Some(self.elsewhere_on(self.pane_machine(pane)?))
    }

    /// The daemon's own machine, if it has said.
    fn own_machine(&self) -> Option<&clackatoo_protocol::Machine> {
        self.workspaces.machine.as_ref()
    }

    /// The machine `host` names after `@`: `local`, or the daemon's own
    /// machine's name, for the daemon's; or one it's connected to, by the
    /// connection's name, as typed after `:connect`, or the machine's own.
    /// Or why it names none.
    pub(crate) fn machine_named(&self, host: &str) -> Result<MachineId, String> {
        let own = self.own_machine();
        if host == "local" || own.is_some_and(|own| own.name == host) {
            return own
                .map(|own| own.id)
                .ok_or_else(|| "the daemon doesn't say which machine it is".to_owned());
        }
        let connection = (self.connections.iter()).find(|connection| {
            connection.host == host
                || (connection.machine.as_ref()).is_some_and(|machine| machine.name == host)
        });
        if let Some(connection) = connection {
            return (connection.machine.as_ref())
                .map(|machine| machine.id)
                .ok_or_else(|| format!("‘{host}’ hasn't said which machine it is yet"));
        }
        let names = self.machine_names();
        Err(
            match crate::cmdline::nearest::nearest(host, names.iter().map(String::as_str)) {
                Some(near) => format!("no machine ‘{host}’: did you mean ‘{near}’?"),
                None => format!("no machine ‘{host}’ is connected: :connect {host} brings it in"),
            },
        )
    }

    /// The names `@` takes, for Tab to offer: the machines the daemon is
    /// connected to, as their connections are named, then `local`.
    pub(crate) fn machine_names(&self) -> Vec<String> {
        (self.connections.iter())
            .filter(|connection| connection.machine.is_some())
            .map(|connection| connection.host.clone())
            .chain(["local".to_owned()])
            .collect()
    }

    /// The machine a new pane made from the focused pane goes on: the one
    /// `@host` named, or else the focused pane's; `None` for the daemon's
    /// own.
    pub(super) fn aimed_machine(&self) -> Option<MachineId> {
        let own = self.own_machine().map(|own| own.id);
        match self.opening_on {
            Some(on) if Some(on) == own => None,
            Some(on) => Some(on),
            None => self.pane_machine(self.shown?.pane),
        }
    }

    /// `request`, a new pane or workspace, made on the machine `@host`
    /// named, if one did, or else, for a new pane, on the machine of the
    /// pane it's made from: the one split, or the focused one, said even
    /// when that's the daemon's own, as the daemon would otherwise go by
    /// the pane its workspace last had focused, which may be another
    /// window's. As it is to a daemon that can't be told.
    pub(super) fn aimed(&self, request: Request) -> Request {
        if !self.daemon_has(wire::LEND) {
            return request;
        }
        let own = self.own_machine().map(|own| own.id);
        let from = |pane: Option<PaneId>| {
            (self.opening_on)
                .or_else(|| self.pane_machine(pane?))
                .or(own)
        };
        let shown = self.shown.map(|shown| shown.pane);
        match request {
            Request::NewWorkspace {
                name,
                terminal,
                on: None,
            } => Request::NewWorkspace {
                name,
                terminal,
                on: self.opening_on,
            },
            Request::NewTab {
                workspace,
                terminal,
                on: None,
            } => Request::NewTab {
                workspace,
                terminal,
                // The focused pane's machine, if it's in that workspace, or
                // else the workspace's own.
                on: match self.shown {
                    Some(shown) if shown.workspace == workspace => from(Some(shown.pane)),
                    _ => (self.opening_on)
                        .or_else(|| self.workspaces.get(workspace)?.home)
                        .or(own),
                },
            },
            Request::Split {
                pane,
                side,
                terminal,
                on: None,
            } => Request::Split {
                pane,
                side,
                terminal,
                on: from(Some(pane)),
            },
            Request::SplitTab {
                tab,
                side,
                terminal,
                share,
                on: None,
            } => Request::SplitTab {
                tab,
                side,
                terminal,
                share,
                on: from(shown),
            },
            request => request,
        }
    }

    /// Carries out `command`, which makes a pane, on the machine `host`
    /// names.
    pub(super) fn run_on(&mut self, host: &str, command: Command) {
        let machine = match self.machine_named(host) {
            Ok(machine) => machine,
            Err(why) => return self.warn(why),
        };
        if !self.needs_daemon(&format!("a pane on @{host}"), wire::LEND) {
            return;
        }
        self.opening_on = Some(machine);
        self.run(command);
        self.opening_on = None;
    }

    /// The machines a new workspace can go on, as the bar's `+` offers
    /// them: the daemon's own, then those it's connected to and can make
    /// one on, by name. Only the daemon's own while it's connected to none
    /// or can't be told.
    pub(crate) fn workspace_machines(&self) -> Vec<(String, Option<MachineId>)> {
        let own =
            (self.own_machine()).map_or_else(|| "this machine".to_owned(), |own| own.name.clone());
        let mut machines = vec![(own, None)];
        if !self.daemon_has(wire::LEND) {
            return machines;
        }
        machines.extend((self.connections.iter()).filter_map(|connection| {
            let machine = connection.machine.as_ref()?;
            matches!(connection.state, ConnectionState::Connected { .. })
                .then(|| (connection.host.clone(), Some(machine.id)))
        }));
        machines
    }

    /// Makes a new workspace with a terminal on `machine`, or on the
    /// daemon's own if `None`, named with the lowest number free there.
    pub(crate) fn new_workspace_on(&mut self, machine: Option<MachineId>) {
        self.opening_on = machine;
        self.new_workspace_here(None);
        self.opening_on = None;
    }

    /// Makes a workspace on the machine `host` names, called `name`, or
    /// with the lowest number free there.
    pub(super) fn new_workspace_named_on(&mut self, name: Option<String>, host: &str) {
        let machine = match self.machine_named(host) {
            Ok(machine) => machine,
            Err(why) => return self.warn(why),
        };
        if self.own_machine().is_some_and(|own| own.id == machine) {
            return self.new_workspace_here(name);
        }
        if !self.needs_daemon(&format!("a workspace on @{host}"), wire::LEND) {
            return;
        }
        self.opening_on = Some(machine);
        self.new_workspace_here(name);
        self.opening_on = None;
    }

    /// Makes a workspace with a terminal, called `name`, on the machine
    /// asked for, if any, or else the daemon's.
    pub(super) fn new_workspace_here(&mut self, name: Option<String>) {
        let terminal = PaneTerminal::New(NewTerminal {
            size: self.new_pane_size(None),
            command: None,
            cwd: None,
            record_to: None,
        });
        let new = Request::NewWorkspace {
            name,
            terminal,
            on: None,
        };
        self.request(new, super::Asked::Pane);
    }
}

/// What `:workspace` asks for, if it's a new one on another machine:
/// `new@host`, then a name if one is given, or `name@host`. The name, if
/// any, and the machine's.
pub(super) fn new_elsewhere(asked: &str) -> Option<(Option<String>, &str)> {
    let (first, rest) = match asked.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, Some(rest.trim()).filter(|rest| !rest.is_empty())),
        None => (asked, None),
    };
    let (name, host) = first.rsplit_once('@')?;
    if host.is_empty() {
        return None;
    }
    match (name, rest) {
        ("new", rest) => Some((rest.map(str::to_owned), host)),
        (name, None) if !name.is_empty() => Some((Some(name.to_owned()), host)),
        _ => None,
    }
}
