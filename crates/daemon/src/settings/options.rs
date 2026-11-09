//! The daemon's options, declared once: what each holds, its default, and
//! where it's listed. Config sets them as `daemon.GROUP.NAME`, as
//! `daemon.web.mode = "tailscale"`, each read as the daemon starts or
//! upgrades.

use crate::schema::{Category, Info, Kind, Scope, Value};

/// Declares the daemon's options: [`ALL`], and [`default_of`]. Each is
/// written as `"group.name" ("description") in Category: Kind = default`.
macro_rules! options {
    ($(
        $(#[doc = $doc:literal])+
        $name:literal ($description:literal) in $category:ident: $kind:expr => $default:expr;
    )+) => {
        /// Every option the daemon has, in the order they're listed.
        pub const ALL: &[Info] = &[$(
            Info {
                name: $name,
                short: "",
                kind: $kind,
                description: $description,
                category: Category::$category,
                scope: Scope::Daemon,
                // Read as the daemon starts or upgrades.
                restart: true,
                local: false,
            },
        )+];

        /// The value `info`, one of [`ALL`], has unless config sets it.
        #[must_use]
        pub fn default_of(info: &Info) -> Value {
            match info.name {
                $($name => $default,)+
                _ => unreachable!("every option is in ALL"),
            }
        }
    };
}

/// A whole number from `min` to `max`.
const fn number(min: u32, max: u32) -> Kind {
    Kind::Number {
        min,
        max,
        or_zero: false,
    }
}

/// The ways the web app is reached, as `web.mode` says.
pub(crate) const WEB_MODES: &[&str] = &["off", "tailscale", "public"];

options! {
    /// The name other machines show this machine's daemon by. Empty is
    /// the machine's hostname.
    "machine.name" ("The name other machines show this one by") in Machine: Kind::Text => Value::Text(String::new());
    /// Whether the daemon takes a higher priority and a place of its own
    /// (see [`crate::priority`]).
    "machine.priority" ("Run the daemon ahead of other programs") in Machine: Kind::Bool => Value::Bool(true);
    /// Whether agents go on through an upgrade, with all they run.
    "upgrade.keep_agents" ("Keep agents running through upgrades") in Agents: Kind::Bool => Value::Bool(true);
    /// The most chats agents started that may work at once, in all.
    "subagents.working" ("Chats agents started working at once, in all") in Subagents: number(1, 64) => Value::Number(16);
    /// The Subagents setting a chat of the user's starts with: at most
    /// `subagents.working`.
    "subagents.default" ("A new chat's Subagents setting") in Subagents: number(0, 64) => Value::Number(6);
    /// The most chats agents started open under one of the user's.
    "subagents.open" ("Chats agents started open under one chat") in Subagents: number(1, 256) => Value::Number(32);
    /// The most chats agents start under one of the user's between two of
    /// the user's messages there.
    "subagents.started" ("Chats agents start under one, between your messages") in Subagents: number(1, 10_000) => Value::Number(128);
    /// How deep chats agents start go.
    "subagents.depth" ("How deep chats agents start go") in Subagents: number(1, crate::agents::DEEPEST_SUBAGENTS as u32) => Value::Number(3);
    /// The longest chain of messages between agents.
    "subagents.hops" ("The longest chain of messages between agents") in Subagents: number(1, 64) => Value::Number(6);
    /// The most turns notices of chats finishing start under one of the
    /// user's, between two of the user's messages there.
    "subagents.notice_turns" ("Turns notices start, between your messages") in Subagents: number(1, 10_000) => Value::Number(100);
    /// How phones and browsers reach the web app, if they do.
    "web.mode" ("How phones and browsers reach the web app") in Web: Kind::Choice(WEB_MODES) => Value::Choice("off");
    /// With tailscale: the unix socket `tailscale serve` connects to.
    /// Empty is `web.sock` in the web app's data.
    "web.socket" ("tailscale: the socket tailscale serve connects to") in Web: Kind::Text => Value::Text(String::new());
    /// With tailscale: the only login let in. Empty is the first device's.
    "web.login" ("tailscale: the only login let in") in Web: Kind::Text => Value::Text(String::new());
    /// With public: the IP address listened on.
    "web.address" ("public: the IP address listened on") in Web: Kind::Text => Value::Text("0.0.0.0".to_owned());
    /// With public: the port listened on.
    "web.port" ("public: the port listened on") in Web: number(1, 65_535) => Value::Number(u32::from(crate::web::config::DEFAULT_PORT));
    /// With public: the certificate chain's PEM file.
    "web.certificate" ("public: the certificate's PEM file") in Web: Kind::Text => Value::Text(String::new());
    /// With public: the certificate's private key's PEM file.
    "web.key" ("public: the certificate's private key's PEM file") in Web: Kind::Text => Value::Text(String::new());
    /// The address the app is reached at, as `https://host:port`.
    "web.origin" ("The address the app is reached at") in Web: Kind::Text => Value::Text(String::new());
    /// How many days a device goes unused before it's unpaired.
    "web.expire_days" ("Days a device goes unused before it's unpaired") in Web: number(1, 36_500) => Value::Number(90);
    /// How many days before the password is asked for again; 0 never.
    "web.password_days" ("Days before the password is asked for again") in Web: Kind::Number { min: 1, max: 36_500, or_zero: true } => Value::Number(0);
    /// Whether a device may ask to be let in from a window: unless set,
    /// with tailscale and not with public.
    "web.ask" ("Let devices ask a window to let them in") in Web: Kind::Bool => Value::Bool(true);
    /// Wrong passwords in a row before pairing with the password locks.
    "web.lock_after" ("Wrong passwords before pairing locks") in Web: number(1, 10_000) => Value::Number(crate::web::config::LOCK_AFTER);
    /// Whether terminals' history is kept across restarts.
    "history.keep" ("Keep terminals' history across restarts") in Kept: Kind::Bool => Value::Bool(true);
    /// The most lines kept of each terminal.
    "history.lines" ("Lines kept of each terminal") in Kept: number(1, 1_000_000) => Value::Number(10_000);
    /// How often, in seconds, the history of terminals that changed is
    /// written.
    "history.every" ("Seconds between writes of terminals' history") in Kept: number(1, 86_400) => Value::Number(30);
    /// How many MiB of images each terminal, chat, picture pane or buffer
    /// may hold.
    "images.terminal_mb" ("MiB of images each terminal or chat may hold") in Kept: number(1, 1 << 20) => Value::Number(64);
    /// How many MiB of images all of them may hold together.
    "images.total_mb" ("MiB of images all of them may hold") in Kept: number(1, 1 << 20) => Value::Number(256);
}

/// The option called `name`, if there is one.
#[must_use]
pub fn find(name: &str) -> Option<&'static Info> {
    ALL.iter().find(|info| info.name == name)
}

/// The groups options are in, as config writes them: `daemon.web`.
pub(crate) fn groups() -> impl Iterator<Item = &'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    ALL.iter().filter_map(move |info| {
        let group = info
            .name
            .split_once('.')
            .map_or(info.name, |(group, _)| group);
        (!seen.contains(&group)).then(|| {
            seen.push(group);
            group
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every default is one the option can hold, and the defaults the
    /// daemon used before match.
    #[test]
    fn every_default_suits_its_option() {
        for info in ALL {
            info.check(&default_of(info))
                .unwrap_or_else(|why| panic!("{}: {why}", info.name));
            assert!(info.name.contains('.'), "{} has a group", info.name);
        }
        let limits = crate::agents::SubagentLimits::default();
        let number = |name| default_of(find(name).unwrap());
        assert_eq!(
            number("subagents.working"),
            Value::Number(limits.working.into())
        );
        assert_eq!(number("subagents.open"), Value::Number(limits.open as u32));
        assert_eq!(
            number("history.lines"),
            Value::Number(crate::DEFAULT_HISTORY_LINES as u32)
        );
        assert_eq!(
            number("history.every"),
            Value::Number(crate::DEFAULT_HISTORY_EVERY.as_secs() as u32)
        );
        let images = crate::images::Limits::default();
        assert_eq!(
            number("images.total_mb"),
            Value::Number((images.total >> 20) as u32)
        );
        assert_eq!(
            groups().collect::<Vec<_>>(),
            [
                "machine",
                "upgrade",
                "subagents",
                "web",
                "history",
                "images"
            ]
        );
    }
}
