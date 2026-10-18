//! `:connect` and `:disconnect`: other machines' workspaces, in this
//! window's bar (CLACKA-117).
//!
//! `:connect HOST` has the daemon connect to the daemon on HOST, which
//! brings HOST's workspaces in beside its own, until `:disconnect HOST`:
//! the daemon keeps the connection, and makes it again whenever it drops,
//! and after it restarts, so this window only asks, and shows what it's
//! told (see [`Desk::connections_changed`]). Each such workspace has a
//! ring in place of its dot, saying how its connection is, and the mouse
//! over it says which machine it's on; `showhost` says when its name says
//! too, as `notes@desk`.
//!
//! `:connect` alone, or Ctrl+B r, opens the picker of a machine to open
//! a new window on (see [`crate::connect`]): the desk doesn't open it
//! itself, as the picker needs no daemon and opens windows, which are the
//! window's to start. It only notes that it was asked for.
//!
//! `:PortForward` is left to the window too, which keeps the forwards, as
//! they outlive the connection the desk sees.

use clackatoo_protocol::{Connection, ConnectionState, Forwarding, Request, Workspace, wire};

use super::{Asked, Desk};
use crate::chrome::Reach;
use crate::options::ShowHost;

/// Where a workspace from another machine is, and how this window's
/// daemon reaches it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Elsewhere {
    /// The machine, as the daemon's connection to it is named, as typed
    /// after `:connect`: `desk`.
    pub(crate) host: String,
    /// What the daemon there calls its machine, if it has said:
    /// `jack-ubuntu-desktop`.
    pub(crate) machine: Option<String>,
    /// How the connection is.
    pub(crate) reach: Reach,
}

impl Elsewhere {
    /// What the mouse over its workspace says: which machine, and how its
    /// connection is.
    pub(crate) fn said(&self) -> String {
        let machine = match &self.machine {
            Some(machine) if *machine != self.host => format!("on {machine} ({})", self.host),
            _ => format!("on {}", self.host),
        };
        let how = match &self.reach {
            Reach::Connected => "connected".to_owned(),
            Reach::Connecting => "connecting".to_owned(),
            Reach::NeedsPassword => format!("needs a password: :connect {}", self.host),
            Reach::Down(why) => format!("not connected, trying again: {why}"),
            Reach::Stopped(why) => format!("not connected: {why}"),
        };
        format!("{machine} · {how}")
    }

    /// What a pane of its workspace says over its last screen while it's
    /// not connected, if it isn't.
    pub(crate) fn notice(&self) -> Option<String> {
        let host = &self.host;
        match &self.reach {
            Reach::Connected => None,
            Reach::Connecting | Reach::Down(_) => Some(format!("{host} isn't connected · trying")),
            Reach::NeedsPassword => Some(format!("{host} needs a password · :connect {host}")),
            Reach::Stopped(_) => Some(format!("{host} isn't connected · :connect {host}")),
        }
    }
}

impl Desk {
    /// Carries out `:connect`: with no machine, asks the window for the
    /// picker; with one, has the daemon connect to it.
    pub(super) fn ask_connect(&mut self, host: Option<String>) {
        let Some(host) = host else {
            self.connect_asked = true;
            return;
        };
        if !self.needs_daemon(":connect HOST", wire::FEDERATION) {
            return;
        }
        // Asked again, one that's down is tried again at once.
        self.request(
            Request::Connect {
                host: host.clone(),
                command: None,
            },
            Asked::Command,
        );
        self.say(format!(
            "connecting to ‘{host}’: its workspaces come into the bar once it's reached"
        ));
        self.connecting = Some(host);
    }

    /// Carries out `:disconnect`: has the daemon end its connection to
    /// `host`, or with none, to the machine the workspace shown is on.
    pub(super) fn ask_disconnect(&mut self, host: Option<String>) {
        if !self.needs_daemon(":disconnect", wire::FEDERATION) {
            return;
        }
        let shown = (self.shown)
            .and_then(|shown| self.workspaces.get(shown.workspace))
            .and_then(|workspace| self.elsewhere(workspace));
        let Some(host) = host.or_else(|| shown.map(|elsewhere| elsewhere.host)) else {
            self.warn(
                "this workspace is this machine's own: :disconnect HOST names the machine".into(),
            );
            return;
        };
        if !self
            .connections
            .iter()
            .any(|connection| connection.host == host)
        {
            let hosts = self
                .connections
                .iter()
                .map(|connection| connection.host.as_str());
            let near = crate::cmdline::nearest::nearest(&host, hosts);
            self.warn(match near {
                Some(near) => format!("there's no connection to ‘{host}’: did you mean ‘{near}’?"),
                None => format!("there's no connection to ‘{host}’"),
            });
            return;
        }
        self.request(Request::Disconnect { host: host.clone() }, Asked::Command);
        self.say(format!("disconnected from ‘{host}’"));
    }

    /// Carries out the config's `client.connect(host)`: asks the daemon
    /// to connect to `host` now, if this window is connected to it, and
    /// on each new connection to it, saying nothing unless it fails.
    pub(super) fn config_connects(&mut self, host: String) {
        if !self.config_hosts.contains(&host) {
            self.config_hosts.push(host.clone());
        }
        if self.connected && self.daemon_has(wire::FEDERATION) {
            self.request(
                Request::Connect {
                    host,
                    command: None,
                },
                Asked::Command,
            );
        }
    }

    /// Carries out the config's `client.disconnect(host)`.
    pub(super) fn config_disconnects(&mut self, host: String) {
        self.config_hosts.retain(|asked| *asked != host);
        if self.connected && self.daemon_has(wire::FEDERATION) {
            self.request(Request::Disconnect { host }, Asked::Command);
        }
    }

    /// Asks the daemon, newly connected to, for the connections the config
    /// asked for.
    pub(super) fn connect_config_hosts(&mut self) {
        if !self.daemon_has(wire::FEDERATION) {
            return;
        }
        for host in self.config_hosts.clone() {
            self.request(
                Request::Connect {
                    host,
                    command: None,
                },
                Asked::Quiet,
            );
        }
    }

    /// Takes the daemon's connections to other machines, as it says they
    /// are now, saying in the bar how the one `:connect` asked for went.
    /// Returns whether anything shown changed.
    pub(crate) fn connections_changed(&mut self, connections: Vec<Connection>) -> bool {
        if let Some(host) = self.connecting.clone()
            && let Some(connection) = connections.iter().find(|link| link.host == host)
        {
            match &connection.state {
                ConnectionState::Connected { .. } => {
                    self.connecting = None;
                    let has = connection.machine.is_some();
                    self.say(if has {
                        format!("connected to ‘{host}’")
                    } else {
                        format!(
                            "connected to ‘{host}’, whose clackatoo is too old to share its \
                             workspaces: update it there"
                        )
                    });
                }
                ConnectionState::Down { reason } | ConnectionState::Stopped { reason } => {
                    self.connecting = None;
                    self.warn(format!("‘{host}’ couldn't be connected to: {reason}"));
                }
                ConnectionState::NeedsPassword => {
                    self.connecting = None;
                    self.warn(format!(
                        "‘{host}’ needs a password: :connect {host} asks for it"
                    ));
                }
                ConnectionState::Connecting | ConnectionState::Asking | ConnectionState::Other => {}
            }
        }
        let changed = connections != self.connections;
        self.connections = connections;
        if changed {
            // Panes not relayed yet say which machine they're on.
            self.show_others();
        }
        changed
    }

    /// Where `workspace` is, if it's from another machine: the connection
    /// to its home, as the daemon last said.
    pub(crate) fn elsewhere(&self, workspace: &Workspace) -> Option<Elsewhere> {
        Some(self.elsewhere_on(workspace.home?))
    }

    /// Where the machine `home` is, another than the daemon's: the
    /// connection to it, as the daemon last said.
    pub(crate) fn elsewhere_on(&self, home: clackatoo_protocol::MachineId) -> Elsewhere {
        let connection = (self.connections.iter()).find(|connection| {
            connection
                .machine
                .as_ref()
                .is_some_and(|machine| machine.id == home)
        });
        match connection {
            Some(connection) => Elsewhere {
                host: connection.host.clone(),
                machine: connection
                    .machine
                    .as_ref()
                    .map(|machine| machine.name.clone()),
                reach: match &connection.state {
                    ConnectionState::Connected { .. } => Reach::Connected,
                    ConnectionState::Asking | ConnectionState::NeedsPassword => {
                        Reach::NeedsPassword
                    }
                    ConnectionState::Down { reason } => Reach::Down(reason.clone()),
                    ConnectionState::Stopped { reason } => Reach::Stopped(reason.clone()),
                    ConnectionState::Connecting | ConnectionState::Other => Reach::Connecting,
                },
            },
            // A machine this window hasn't heard the daemon's connection to,
            // as from a daemon that doesn't say: by the start of its ID.
            None => Elsewhere {
                host: home.to_string()[..8].to_owned(),
                machine: None,
                reach: Reach::Connecting,
            },
        }
    }

    /// `workspace`'s name, as the bar and the command line show it: with
    /// the machine it's on after it, `name@host`, if it's from another
    /// machine and `showhost` says to, as `ambiguous` does while another
    /// workspace has the same name.
    pub(crate) fn shown_name(&self, workspace: &Workspace) -> String {
        let Some(elsewhere) = self.elsewhere(workspace) else {
            return workspace.name.clone();
        };
        let with_host = match self.options.showhost {
            ShowHost::Always => true,
            ShowHost::Never => false,
            ShowHost::Ambiguous => (self.workspaces.list.iter())
                .any(|other| other.id != workspace.id && other.name == workspace.name),
        };
        if with_host {
            format!("{}@{}", workspace.name, elsewhere.host)
        } else {
            workspace.name.clone()
        }
    }

    /// The workspace `name` names, as `:workspace` takes one: `name@host`
    /// for one from another machine, or a name alone, this machine's own
    /// first.
    pub(crate) fn workspace_named(&self, name: &str) -> Option<&Workspace> {
        if let Some((name, host)) = name.rsplit_once('@') {
            return (self.workspaces.list.iter()).find(|workspace| {
                workspace.name == name
                    && self
                        .elsewhere(workspace)
                        .is_some_and(|elsewhere| elsewhere.host == host)
            });
        }
        let named = || (self.workspaces.list.iter()).filter(|workspace| workspace.name == name);
        named()
            .find(|workspace| workspace.home.is_none())
            .or_else(|| named().next())
    }

    /// The machines Tab offers after `:connect` and `:disconnect`: those
    /// the daemon is connected to, then those `~/.ssh/config` names and
    /// those the window connected to lately.
    pub(crate) fn known_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = (self.connections.iter())
            .map(|connection| connection.host.clone())
            .collect();
        if !self.remote {
            if let Some(home) = std::env::var_os("HOME") {
                hosts.extend(crate::hosts::ssh_config_hosts(std::path::Path::new(&home)));
            }
            hosts.extend(crate::hosts::recent());
        }
        let mut seen = std::collections::HashSet::new();
        hosts.retain(|host| seen.insert(host.clone()));
        hosts
    }

    /// Takes whether the picker of a machine was asked for, for the window
    /// to open.
    pub(crate) fn take_connect_asked(&mut self) -> bool {
        std::mem::take(&mut self.connect_asked)
    }

    /// Takes what `:PortForward` asked for since last time, for the window
    /// to do.
    pub(crate) fn take_forwards_asked(&mut self) -> Vec<crate::command::Forwarding> {
        std::mem::take(&mut self.forwards_asked)
    }

    /// What `machine` is called, as its connection names it, or the
    /// daemon's own machine if `None`.
    pub(super) fn machine_called(&self, machine: Option<clackatoo_protocol::MachineId>) -> String {
        let Some(machine) = machine else {
            return "this machine".to_owned();
        };
        (self.connections.iter())
            .find(|connection| {
                connection
                    .machine
                    .as_ref()
                    .is_some_and(|known| known.id == machine)
            })
            .map_or_else(
                || machine.to_string()[..8].to_owned(),
                |connection| connection.host.clone(),
            )
    }

    /// The machine the pane shown is on, if it's another than the daemon's:
    /// in a workspace `:connect` brought in, or one of the daemon's own
    /// showing a pane from another machine.
    pub(super) fn shown_elsewhere(&self) -> Option<Elsewhere> {
        self.pane_elsewhere(self.shown?.pane)
    }

    /// Carries out `:PortForward`, as `forwarding` asks: the window forwards
    /// ports to its daemon's machine itself; in a workspace from another
    /// machine, the daemon forwards them there, from its own machine, which
    /// is this window's.
    pub(super) fn port_forward(&mut self, forwarding: crate::command::Forwarding) {
        use crate::command::Forwarding as Typed;
        let Some(elsewhere) = self.shown_elsewhere() else {
            return self.forwards_asked.push(forwarding);
        };
        let host = elsewhere.host;
        if self.remote {
            // The daemon's machine isn't this one: its ports aren't the
            // window's.
            if !matches!(forwarding, Typed::Config(_)) {
                self.warn(format!(
                    "A window on another machine than its daemon can't forward ports to {host} \
                     yet: :PortForward in one of the daemon's own workspaces forwards to its \
                     machine"
                ));
            }
            return;
        }
        if !self.needs_daemon(":PortForward", wire::RELAY) {
            return;
        }
        let Some(pane) = self.shown.map(|shown| shown.pane) else {
            return;
        };
        let forwarding = match forwarding {
            Typed::List => Forwarding::List,
            Typed::Add(spec) | Typed::Config(spec) => Forwarding::Add {
                local: spec.local,
                host: spec.host,
                port: spec.port,
            },
            Typed::Stop(local) => Forwarding::Stop(local),
        };
        self.request(Request::Forwards { pane, forwarding }, Asked::Forwarded);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elsewhere(reach: Reach) -> Elsewhere {
        Elsewhere {
            host: "desk".into(),
            machine: Some("jack-ubuntu-desktop".into()),
            reach,
        }
    }

    #[test]
    fn the_mouse_over_a_workspace_from_elsewhere_says_where_and_how() {
        assert_eq!(
            elsewhere(Reach::Connected).said(),
            "on jack-ubuntu-desktop (desk) · connected"
        );
        assert_eq!(
            elsewhere(Reach::NeedsPassword).said(),
            "on jack-ubuntu-desktop (desk) · needs a password: :connect desk"
        );
        let same = Elsewhere {
            machine: Some("desk".into()),
            ..elsewhere(Reach::Down("it stopped".into()))
        };
        assert_eq!(
            same.said(),
            "on desk · not connected, trying again: it stopped"
        );
        assert_eq!(elsewhere(Reach::Connected).notice(), None);
        assert_eq!(
            elsewhere(Reach::Stopped("turned down".into()))
                .notice()
                .as_deref(),
            Some("desk isn't connected · :connect desk")
        );
    }
}
