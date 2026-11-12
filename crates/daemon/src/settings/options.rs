//! The daemon's options, declared once: what each holds, its default, and
//! where it's listed. Config sets them as `daemon.GROUP.NAME`, as
//! `daemon.web.mode = "tailscale"`. Those marked `live` change as the
//! daemon runs, as its config is run again, for what it starts from then
//! on; the rest are read as the daemon starts or upgrades.

use crate::schema::{Category, Info, Kind, Scope, Value};

/// Declares the daemon's options: [`ALL`], and [`default_of`]. Each is
/// written as `"group.name" ("description") in Category: Kind = default`,
/// with `live` after the category for one that changes as the daemon runs.
macro_rules! options {
    ($(
        $(#[doc = $doc:literal])+
        $name:literal ($description:literal) in $category:ident $($live:ident)?: $kind:expr => $default:expr;
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
                restart: options!(@restart $($live)?),
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
    // Read only as the daemon starts or upgrades, unless it's `live`.
    (@restart) => { true };
    (@restart live) => { false };
}

/// A whole number from `min` to `max`.
const fn number(min: u32, max: u32) -> Kind {
    Kind::Number {
        min,
        max,
        or_zero: false,
    }
}

/// What's done with lines programs mark to be hidden, as `hints.hide`
/// says.
pub(crate) const HIDE_RULES: &[&str] = &["mask", "show"];

/// What's done with lines programs mark to be kept from agents, as
/// `hints.noagent` says.
pub(crate) const NOAGENT_RULES: &[&str] = &["hide", "show"];

/// The ways the web app is reached, as `web.mode` says.
pub(crate) const WEB_MODES: &[&str] = &["off", "tailscale", "public"];

pub(crate) const MCP_POLICIES: &[&str] = &["inherit", "off", "allow"];

options! {
    /// The name other machines show this machine's daemon by. Empty is
    /// the machine's hostname.
    "machine.name" ("The name other machines show this one by") in Machine: Kind::Text => Value::Text(String::new());
    /// Whether the daemon takes a higher priority and a place of its own
    /// (see [`crate::priority`]).
    "machine.priority" ("Run the daemon ahead of other programs") in Machine: Kind::Bool => Value::Bool(true);
    /// Whether agents go on through an upgrade, with all they run.
    "upgrade.keep_agents" ("Keep agents running through upgrades") in Agents live:Kind::Bool => Value::Bool(true);
    /// The engine offered by default to new chats. Existing chats keep theirs.
    "permissions.engine" ("A new chat's permission engine") in Agents live: Kind::Choice(&["provider", "clackatoo"]) => Value::Choice("provider");
    /// A JSON list of literal Allow, Ask and Deny rules.
    "permissions.rules" ("Clackatoo permission rules") in Agents live: Kind::Text => Value::Text("[]".to_owned());
    /// Allow Claude Code to load its own plugins and skill catalogue.
    /// Applied when the daemon starts or upgrades.
    "plugins.claude" ("Allow Claude Code's native plugins") in Agents: Kind::Bool => Value::Bool(false);
    /// Allow Codex to load its own plugins and skill catalogue.
    /// Applied when the daemon starts or upgrades.
    "plugins.codex" ("Allow Codex's native plugins") in Agents: Kind::Bool => Value::Bool(false);
    /// Allow servers configured directly in providers. Managed servers are separate.
    "mcp.providers" ("Allow provider-managed MCP servers") in Agents: Kind::Bool => Value::Bool(false);
    /// Override provider-managed MCP for Claude Code.
    "mcp.claude" ("Claude Code's provider-managed MCP servers") in Agents: Kind::Choice(MCP_POLICIES) => Value::Choice("inherit");
    /// Override provider-managed MCP for the bundled Codex adapter.
    "mcp.codex" ("Codex's provider-managed MCP servers") in Agents: Kind::Choice(MCP_POLICIES) => Value::Choice("inherit");
    /// The most chats agents started that may work at once, in all.
    "subagents.working" ("Chats agents started working at once, in all") in Subagents live:number(1, 64) => Value::Number(16);
    /// The Subagents setting a chat of the user's starts with: at most
    /// `subagents.working`.
    "subagents.default" ("A new chat's Subagents setting") in Subagents live:number(0, 64) => Value::Number(6);
    /// The most chats agents started open under one of the user's.
    "subagents.open" ("Chats agents started open under one chat") in Subagents live:number(1, 256) => Value::Number(32);
    /// The most chats agents start under one of the user's between two of
    /// the user's messages there.
    "subagents.started" ("Chats agents start under one, between your messages") in Subagents live:number(1, 10_000) => Value::Number(128);
    /// How deep chats agents start go.
    "subagents.depth" ("How deep chats agents start go") in Subagents live:number(1, crate::agents::DEEPEST_SUBAGENTS as u32) => Value::Number(3);
    /// The longest chain of messages between agents.
    "subagents.hops" ("The longest chain of messages between agents") in Subagents live:number(1, 64) => Value::Number(6);
    /// The most turns notices of chats finishing start under one of the
    /// user's, between two of the user's messages there.
    "subagents.notice_turns" ("Turns notices start, between your messages") in Subagents live:number(1, 10_000) => Value::Number(100);
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
    "history.lines" ("Lines kept of each terminal") in Kept live:number(1, 1_000_000) => Value::Number(10_000);
    /// How often, in seconds, the history of terminals that changed is
    /// written.
    "history.every" ("Seconds between writes of terminals' history") in Kept live:number(1, 86_400) => Value::Number(30);
    /// How many MiB of images each terminal, chat, picture pane or buffer
    /// may hold.
    "images.terminal_mb" ("MiB of images each terminal or chat may hold") in Kept: number(1, 1 << 20) => Value::Number(64);
    /// How many MiB of images all of them may hold together.
    "images.total_mb" ("MiB of images all of them may hold") in Kept: number(1, 1 << 20) => Value::Number(256);
    /// How many seconds a command must have run for its end to be told
    /// in a notification while no one is looking; 0 tells none.
    "terminals.notify_after" ("Seconds a command runs before its end is told") in DaemonTerminals live:Kind::Number { min: 1, max: 86_400, or_zero: true } => Value::Number(10);
    /// Whether variation selector 16 widens an emoji to two columns, in
    /// terminals started from then on (see [`clackatoo_term::Widths`]).
    "terminals.vs16" ("Variation selector 16 widens an emoji") in DaemonTerminals live:Kind::Bool => Value::Bool(true);
    /// Whether lines a program marks `hide` (OSC 46373) are masked until
    /// the user asks to see them, or shown.
    "hints.hide" ("Lines programs mark hidden: mask or show") in DaemonTerminals: Kind::Choice(HIDE_RULES) => Value::Choice("mask");
    /// Whether lines a program marks `nosave` are left out of the history
    /// kept across restarts.
    "hints.nosave" ("Never write to disk lines programs mark nosave") in DaemonTerminals: Kind::Bool => Value::Bool(true);
    /// Whether lines a program marks `noagent` are kept from agents, or
    /// shown to them.
    "hints.noagent" ("Lines programs keep from agents: hide or show") in DaemonTerminals: Kind::Choice(NOAGENT_RULES) => Value::Choice("hide");
    /// How many seconds hidden lines the user asked to see stay shown.
    "hints.shown_for" ("Seconds hidden lines stay shown once asked to") in DaemonTerminals: number(1, 86_400) => Value::Number(30);
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
        assert_eq!(
            number("subagents.open"),
            Value::Number(u32::try_from(limits.open).unwrap())
        );
        assert_eq!(
            number("history.lines"),
            Value::Number(u32::try_from(crate::DEFAULT_HISTORY_LINES).unwrap())
        );
        assert_eq!(
            number("history.every"),
            Value::Number(u32::try_from(crate::DEFAULT_HISTORY_EVERY.as_secs()).unwrap())
        );
        assert_eq!(
            number("terminals.notify_after"),
            Value::Number(u32::try_from(crate::DEFAULT_LONG_COMMAND.as_secs()).unwrap())
        );
        assert_eq!(
            number("terminals.vs16"),
            Value::Bool(clackatoo_term::Config::default().widths.vs16)
        );
        let hints = crate::HintRules::default();
        assert_eq!(
            number("hints.shown_for"),
            Value::Number(u32::try_from(hints.shown_for.as_secs()).unwrap())
        );
        assert_eq!(number("hints.nosave"), Value::Bool(hints.nosave));
        let images = crate::images::Limits::default();
        assert_eq!(
            number("images.total_mb"),
            Value::Number(u32::try_from(images.total >> 20).unwrap())
        );
        assert_eq!(
            groups().collect::<Vec<_>>(),
            [
                "machine",
                "upgrade",
                "permissions",
                "plugins",
                "mcp",
                "subagents",
                "web",
                "history",
                "images",
                "terminals",
                "hints"
            ]
        );
    }
}
