//! The daemon's own config as the daemon runs (see [`crate::settings`]):
//! watched on the daemon's one inotify instance, and run again a while
//! after its files last changed, off the loop; changed by windows'
//! settings menus; and told to windows, whose menus show it.
//!
//! What can change as the daemon runs does, at once, for what's started
//! from then on: the agents and profiles new chats start with, the limits
//! on chats agents start, whether agents go on through upgrades, the
//! notice when a long command ends, how terminals started then lay out
//! emoji, and how much of terminals' history is kept, and how often. The
//! rest is read only as the daemon starts or upgrades, and windows are
//! told it waits for that. A config that doesn't run changes nothing: the
//! daemon goes on by what it has, and windows are told why.
//!
//! What runs programs takes a change in only as the user starts a chat,
//! so an agent that can write a file of config can't make itself, or the
//! chats it starts, run something else. Chats agents start go by the
//! agents and profiles the config said as the user's own chat at the top
//! of their tree started; the limits on them change only as the user
//! starts a chat, but for limits lowered, which apply at once. And a change
//! to an agent's command, a profile, or a limit raised, made outside a
//! window's settings menu, is said in every window, to undo if it's not
//! the user's.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use clackatoo_protocol::TerminalId;
use clackatoo_protocol::{
    DaemonOption, DaemonSettings, Error, Event, OptionPlace, OptionSaved, OptionSetBy,
    OptionSource, Response, wire,
};
use mio::Token;

use super::{Daemon, Link, Source};
use crate::DaemonConfig;
use crate::agents::{Configured, SubagentLimits};
use crate::schema::{self, Info, Place, Value};
use crate::settings::{self, Settings, options};

/// How long after the config's files last changed it's run again: an
/// editor saving, or a checkout, changes several at once.
const GATHER: Duration = Duration::from_millis(250);

/// The daemon's config, as it's watched and run again.
#[derive(Debug, Default)]
pub(super) struct Configuring {
    /// What the config set as it last ran without a mistake, once it's run
    /// again; until then, what it set as the daemon started.
    configured: Option<Settings>,
    /// Why it didn't run, as its files are now, if it didn't, once it's
    /// run again; until then, [`settings::Own::failure`] says.
    failure: Option<String>,
    /// It has run again since the daemon started.
    ran: bool,
    /// When to run it again, as its files have changed.
    due: Option<Instant>,
    /// A run under way, off the loop.
    running: Option<Running>,
    /// Changes windows asked for, in turn, waiting for the run under way.
    asked: VecDeque<Asked>,
    /// What the config said of agents as each of the user's own chats
    /// started, by the chat: what chats agents start under it go by.
    trees: HashMap<TerminalId, Configured>,
    /// What it said as the daemon started, for chats agents start under a
    /// chat of the user's from before, or gone.
    started: Option<Configured>,
}

impl Configuring {
    /// As the daemon starts, its config saying `started` of agents.
    pub(super) fn starting(started: &Configured) -> Self {
        Self {
            started: Some(started.clone()),
            ..Self::default()
        }
    }
}

/// A run of the config under way, off the loop.
#[derive(Debug)]
struct Running {
    /// What it comes to: what the config set, and the daemon's config with
    /// it, made off the loop, to take what changes as it runs from; or why
    /// not.
    answer: mpsc::Receiver<Result<(Settings, Box<DaemonConfig>), String>>,
    /// The change it's for, if a window asked for one.
    asked: Option<Asked>,
}

/// A change a window's menu asked for.
#[derive(Debug)]
struct Asked {
    token: Token,
    info: &'static Info,
    value: Option<Value>,
}

impl Daemon {
    /// Watches the folders of the daemon's own config, if it has one, and
    /// those of the modules it last required.
    pub(super) fn watch_config(&mut self) {
        let Some(own) = &self.config.own_config else {
            return;
        };
        let settings = (self.configuring.configured.as_ref()).unwrap_or(&own.settings);
        for dir in own.dirs.watched(settings) {
            self.add_config_watch(&dir);
        }
    }

    /// The config's files changed: it's run again a while after the last.
    pub(super) fn config_files_changed(&mut self) {
        if self.config.own_config.is_some() {
            self.configuring.due = Some(Instant::now() + GATHER);
        }
    }

    /// When the config is next to be run again, if it is.
    pub(super) fn next_config_due(&self) -> Option<Instant> {
        self.configuring.due
    }

    /// Takes what a run off the loop came to, and starts the next, as a
    /// window asked or the files changed.
    pub(super) fn run_config_if_due(&mut self) {
        if let Some(running) = &self.configuring.running {
            let answer = match running.answer.try_recv() {
                Ok(answer) => answer,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("the thread running the daemon's config went away".to_owned())
                }
            };
            let asked = self
                .configuring
                .running
                .take()
                .and_then(|running| running.asked);
            self.config_ran(asked, answer);
        }
        if let Some(asked) = self.configuring.asked.pop_front() {
            return self.run_config(Some(asked));
        }
        if self
            .configuring
            .due
            .is_some_and(|due| due <= Instant::now())
        {
            self.configuring.due = None;
            self.run_config(None);
        }
    }

    /// Runs the config again on a thread of its own, saving the change
    /// `asked` first, if a window asked for one.
    fn run_config(&mut self, asked: Option<Asked>) {
        let Some(own) = &self.config.own_config else {
            return;
        };
        let dirs = own.dirs.clone();
        let waker = Arc::clone(&self.waker);
        let (send, answer) = mpsc::channel();
        let change = asked
            .as_ref()
            .map(|asked| (asked.info, asked.value.clone()));
        let spawned = std::thread::Builder::new()
            .name("daemon-config".into())
            .spawn(move || {
                let ran = match change {
                    Some((info, value)) => settings::change(&dirs, info, value),
                    None => settings::rerun(&dirs),
                };
                // Made here, off the loop, as it reads the web app's
                // password.
                let ran = ran.map(|settings| {
                    let config = Box::new(DaemonConfig::from_settings(&settings));
                    (settings, config)
                });
                let _ = send.send(ran);
                if let Err(err) = waker.wake() {
                    log::warn!("waking the daemon after its config ran failed: {err}");
                }
            });
        match spawned {
            Ok(_) => self.configuring.running = Some(Running { answer, asked }),
            Err(err) => {
                let said = format!("the daemon's config couldn't be run: {err}");
                log::warn!("{said}");
                self.config_ran(asked, Err(said));
            }
        }
    }

    /// Takes what a run of the config came to, for the change `asked`, if
    /// a window asked for one: what changes as the daemon runs changes,
    /// and windows are told.
    fn config_ran(
        &mut self,
        asked: Option<Asked>,
        answer: Result<(Settings, Box<DaemonConfig>), String>,
    ) {
        let saved = asked.as_ref().map(|asked| OptionSaved {
            name: asked.info.name.to_owned(),
            value: asked.value.as_ref().map(Value::text),
            refused: answer.as_ref().err().cloned(),
        });
        // A run as the files changed that comes to what the daemon has, as
        // the one after a window's change does, is told no one.
        let unchanged = asked.is_none()
            && self.config.own_config.as_ref().is_some_and(|own| {
                let (configured, failure) = if self.configuring.ran {
                    let configured = self.configuring.configured.as_ref();
                    (
                        configured.unwrap_or(&own.settings),
                        &self.configuring.failure,
                    )
                } else {
                    (&own.settings, &own.failure)
                };
                match &answer {
                    Ok((settings, _)) => failure.is_none() && settings == configured,
                    Err(said) => failure.as_ref() == Some(said),
                }
            });
        if unchanged {
            // A folder taken away and made again is watched again.
            self.watch_config();
            return;
        }
        match answer {
            Ok((settings, fresh)) => {
                // What runs programs, changed where no window's menu did.
                if asked.is_none() {
                    let changed =
                        outside_changes(&self.config.agents.configured, &fresh.agents.configured);
                    if !changed.is_empty() {
                        let said = format!(
                            "daemon config changed outside clackatoo: {}",
                            changed.join(", ")
                        );
                        log::warn!("{said}");
                        self.tell_windows(&Event::Said(said));
                    }
                }
                self.take_live_config(&fresh);
                self.configuring.configured = Some(settings);
                self.configuring.failure = None;
                self.configuring.ran = true;
                // Modules it requires now are watched too.
                self.watch_config();
            }
            // A change refused leaves the config as it was, and as it ran.
            Err(_) if asked.is_some() => {}
            Err(said) => {
                self.configuring.failure = Some(said);
                self.configuring.ran = true;
            }
        }
        self.tell_daemon_settings(asked.map(|asked| asked.token), saved.as_ref());
    }

    /// Takes from `fresh`, the daemon's config as its own config is now,
    /// what changes as the daemon runs. The agents and profiles are for
    /// chats the user starts from now on (see [`Daemon::tree_agents`]),
    /// and the limits on chats agents start apply at once only if they're
    /// lower: higher, as the user next starts a chat.
    fn take_live_config(&mut self, fresh: &DaemonConfig) {
        let lower = lower_or_same(
            &fresh.agents.configured.subagents,
            &self.tools.subagent_limits(),
        );
        self.config.agents.configured = fresh.agents.configured.clone();
        if lower {
            self.refresh_subagent_limits();
        }
        self.config.long_commands = fresh.long_commands;
        self.config.terminal.widths.vs16 = fresh.terminal.widths.vs16;
        if let (Some(history), Some(fresh)) = (&mut self.config.history, &fresh.history) {
            history.lines = fresh.lines;
            history.every = fresh.every;
        }
    }

    /// Notes, as the user's own chat `chat` starts, what the config says of
    /// agents now, for chats agents start under it to go by; and forgets
    /// what it noted of chats gone.
    pub(super) fn note_tree(&mut self, chat: TerminalId) {
        let terminals = &self.terminals;
        (self.configuring.trees).retain(|id, _| terminals.contains_key(id));
        (self.configuring.trees).insert(chat, self.config.agents.configured.clone());
    }

    /// What the config said of agents as the user's own chat at the top of
    /// `chat`'s tree started, which chats agents start under it go by: the
    /// agents they run and the profiles they start as. If that chat is
    /// from before the daemon started, or gone, what it said as the daemon
    /// started.
    pub(super) fn tree_agents(&self, chat: TerminalId) -> &Configured {
        let root = (self.chat_ref(chat))
            .and_then(|chat| chat.relations().root)
            .unwrap_or(chat);
        (self.configuring.trees.get(&root))
            .or(self.configuring.started.as_ref())
            .unwrap_or(&self.config.agents.configured)
    }

    /// Whether `chat` is one an agent started, which goes by its tree's
    /// config (see [`Daemon::tree_agents`]).
    pub(super) fn agent_started(&self, chat: TerminalId) -> bool {
        (self.chat_ref(chat)).is_some_and(|chat| chat.relations().root.is_some())
    }

    /// Every window: never a device on the web app, a daemon relaying, nor
    /// what serves agents' tools.
    fn windows(&self) -> Vec<Token> {
        (self.sources.iter())
            .filter_map(|(at, source)| match source {
                Source::Client(client)
                    if !matches!(client.link, Link::Web(_))
                        && !client.relays
                        && client.serving_tools.is_none() =>
                {
                    Some(Token(at))
                }
                _ => None,
            })
            .collect()
    }

    /// Tells every window `event`.
    fn tell_windows(&mut self, event: &Event) {
        for token in self.windows() {
            self.send_event(token, event.clone());
        }
    }

    /// Tells every window of the daemon's settings, and the one at `asker`,
    /// what came of its change, `saved`: never a device on the web app, a
    /// daemon relaying, nor what serves agents' tools.
    pub(super) fn tell_daemon_settings(
        &mut self,
        asker: Option<Token>,
        saved: Option<&OptionSaved>,
    ) {
        let windows: Vec<Token> = (self.sources.iter())
            .filter_map(|(at, source)| match source {
                Source::Client(client)
                    if !matches!(client.link, Link::Web(_))
                        && !client.relays
                        && client.serving_tools.is_none()
                        && client.wire >= wire::DAEMON_SETTINGS =>
                {
                    Some(Token(at))
                }
                _ => None,
            })
            .collect();
        if windows.is_empty() {
            return;
        }
        let settings = self.daemon_settings();
        for token in windows {
            let saved = saved.filter(|_| Some(token) == asker).cloned();
            self.send_event(
                token,
                Event::DaemonSettings {
                    settings: settings.clone(),
                    saved,
                },
            );
        }
    }

    /// The daemon's settings, as a window's menu shows them.
    fn daemon_settings(&self) -> DaemonSettings {
        let Some(own) = &self.config.own_config else {
            return DaemonSettings {
                machine: self.machine.name.clone(),
                ..DaemonSettings::default()
            };
        };
        let configured = (self.configuring.configured.as_ref()).unwrap_or(&own.settings);
        let failure = if self.configuring.ran {
            self.configuring.failure.clone()
        } else {
            own.failure.clone()
        };
        let options = (options::ALL.iter())
            .map(|info| {
                let value = configured.value(info.name);
                // What the daemon went by as it started, of one it reads
                // only then.
                let running = own.settings.value(info.name);
                DaemonOption {
                    name: info.name.to_owned(),
                    value: value.text(),
                    running: (info.restart && running != value).then(|| running.text()),
                    restart: info.restart,
                    by: set_by(&configured.set_by(info.name)),
                }
            })
            .collect();
        DaemonSettings {
            machine: self.machine.name.clone(),
            dir: own.dirs.config().display().to_string(),
            options,
            failure,
        }
    }

    /// Answers [`Request::DaemonSettings`](clackatoo_protocol::Request):
    /// never to a device on the web app.
    pub(super) fn serve_daemon_settings(&self, token: Token) -> Result<Response, Error> {
        if self.is_web_device(token) {
            return Err(Error::Other(
                "the daemon's settings are changed in a window".to_owned(),
            ));
        }
        Ok(Response::DaemonSettings(self.daemon_settings()))
    }

    /// Answers [`Request::SetDaemonOption`](clackatoo_protocol::Request):
    /// the change is saved, and the config run with it, off the loop, and
    /// the window told what came of it then.
    pub(super) fn serve_set_daemon_option(
        &mut self,
        token: Token,
        name: &str,
        value: Option<&str>,
    ) -> Result<Response, Error> {
        if self.is_web_device(token) {
            return Err(Error::Other(
                "the daemon's settings are changed in a window".to_owned(),
            ));
        }
        let Some(own) = &self.config.own_config else {
            return Err(Error::Other(
                "this daemon runs without a config of its own, so it has nowhere to save it"
                    .to_owned(),
            ));
        };
        let info = options::find(name)
            .ok_or_else(|| Error::Other(format!("the daemon has no option {name}")))?;
        let value = value
            .map(|value| info.read(value))
            .transpose()
            .map_err(Error::Other)?;
        // What the user's own config sets outdoes what settings.lua does.
        let configured = (self.configuring.configured.as_ref()).unwrap_or(&own.settings);
        let by = configured.set_by(info.name);
        if by.source == schema::Source::Config {
            let place = (by.at.as_ref())
                .map_or_else(|| "daemon/init.lua".to_owned(), |at| shown_place(own, at));
            return Err(Error::Other(format!(
                "{place} sets daemon.{}, which outdoes what the menu saves",
                info.name
            )));
        }
        self.configuring
            .asked
            .push_back(Asked { token, info, value });
        // Started at once, unless a run is under way.
        self.run_config_if_due();
        Ok(Response::Done)
    }

    /// Before an upgrade allowed to wait until `until`: whether, at `now`,
    /// it waits for a change a window asked for, which is saved and, if
    /// the config doesn't run with it, put back. Past `until`, it goes on:
    /// the file is whole either way.
    pub(super) fn config_holds_upgrade(&self, until: Instant, now: Instant) -> bool {
        now < until
            && (self.configuring.running.as_ref()).is_some_and(|running| running.asked.is_some())
    }

    /// As the upgrade goes ahead: a change still being saved, past the
    /// time the upgrade waited, is cut off where it is, which the log
    /// says, and the next build runs the config as the file is then, or
    /// else what last ran; the window that asked for it, and those whose
    /// changes were waiting, are told theirs weren't saved.
    pub(super) fn config_upgrading(&mut self) {
        let mut refused: Vec<Asked> = self.configuring.asked.drain(..).collect();
        if let Some(asked) = self
            .configuring
            .running
            .take()
            .and_then(|running| running.asked)
        {
            log::warn!(
                "upgrading while daemon.{} is still being saved: the next build runs the config \
                 as daemon/settings.lua is, or else what last ran",
                asked.info.name
            );
            refused.insert(0, asked);
        }
        for asked in refused {
            let saved = OptionSaved {
                name: asked.info.name.to_owned(),
                value: asked.value.as_ref().map(Value::text),
                refused: Some("the daemon upgraded before it was saved: try again".to_owned()),
            };
            self.tell_daemon_settings(Some(asked.token), Some(&saved));
        }
    }
}

/// What changed from `old` to `new`, of what runs programs: an agent's
/// command, or one added; a profile changed or added; a limit on chats
/// agents start raised. As windows are told it, `agents.claude command`.
fn outside_changes(old: &Configured, new: &Configured) -> Vec<String> {
    let mut changed = Vec::new();
    for agent in &new.agents {
        let before = old.agents.iter().find(|each| each.name == agent.name);
        if before.is_none_or(|before| before.command != agent.command) {
            changed.push(format!("agents.{} command", agent.name));
        }
    }
    for profile in &new.profiles {
        if !old.profiles.contains(profile) {
            changed.push(format!("profiles.{}", profile.name));
        }
    }
    let (old, new) = (&old.subagents, &new.subagents);
    let raised = [
        ("working", new.working > old.working),
        ("default", new.default > old.default),
        ("open", new.open > old.open),
        ("started", new.started > old.started),
        ("depth", new.depth > old.depth),
        ("hops", new.hops > old.hops),
        ("notice_turns", new.notice_turns > old.notice_turns),
    ];
    for (name, raised) in raised {
        if raised {
            changed.push(format!("subagents.{name}"));
        }
    }
    changed
}

/// Whether every one of the limits `new` is at most what it is in `old`.
pub(super) fn lower_or_same(new: &SubagentLimits, old: &SubagentLimits) -> bool {
    new.working <= old.working
        && new.default <= old.default
        && new.open <= old.open
        && new.started <= old.started
        && new.depth <= old.depth
        && new.hops <= old.hops
        && new.notice_turns <= old.notice_turns
}

/// `by`, as a window's menu is told it.
fn set_by(by: &schema::SetBy) -> OptionSetBy {
    let place = |place: &Place| OptionPlace {
        file: place.file.to_string(),
        line: place.line,
    };
    OptionSetBy {
        source: match by.source {
            schema::Source::Settings => OptionSource::Settings,
            schema::Source::Config | schema::Source::Runtime => OptionSource::Config,
            schema::Source::Default | schema::Source::Startup => OptionSource::Default,
        },
        at: by.at.as_ref().map(place),
        inner: by.inner.as_ref().map(place),
    }
}

/// `at`, a line of the config in `own`, as `daemon/init.lua:12`.
fn shown_place(own: &settings::Own, at: &Place) -> String {
    let config = format!("{}/", own.dirs.config().display());
    format!("{}:{}", at.file.replace(&config, ""), at.line)
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{AgentTools, Size};

    use super::*;
    use crate::agents::Launch;
    use crate::chat::HostedChat;
    use crate::server::agent_files::tests::daemon_with_chat;

    /// The daemon's config `text`, as the daemon builds it.
    fn fresh(text: &str) -> DaemonConfig {
        DaemonConfig::from_settings(&Settings::from_lua(text).unwrap())
    }

    /// An agent that can write the daemon's config can't make the chats it
    /// starts run another command, nor raise their limits: a change made
    /// as the daemon runs is for chats the user starts, and windows are
    /// told what changed (CLACKA-237 review, F1).
    #[test]
    fn what_runs_programs_changes_only_for_chats_the_user_starts() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = daemon_with_chat(dir.path());
        daemon.note_tree(a);
        let limits = daemon.tools.subagent_limits();
        let written = "daemon.agents.claude = { command = { \"evil\" } }\n\
                       daemon.subagents.working = 64\ndaemon.profiles.mine = { about = \"x\" }";
        let before = daemon.config.agents.configured.clone();
        daemon.take_live_config(&fresh(written));
        let evil = Launch::Command(vec!["evil".into()]);
        // The user's next chat goes by it...
        assert_eq!(daemon.config.agents.find("claude").unwrap().launch, evil);
        // ...but not the chats agents start under the user's chat, whose
        // agent and profiles are as the user's chat began.
        assert_ne!(daemon.agent_spec_under(a, "claude").launch, evil);
        let profiles = crate::agents::AgentConfig::profiles_in(daemon.tree_agents(a));
        assert!(!profiles.iter().any(|profile| profile.name == "mine"));
        // Nor a chat started under it, by the chat's own tree.
        let agent = crate::AgentConfig::default().find("claude").unwrap();
        let size = Size::new(40, 10).unwrap();
        let mut kid = HostedChat::new(
            TerminalId(7),
            size,
            "/".into(),
            &agent,
            AgentTools::Ask,
            true,
            false,
        );
        kid.started_by(a, a, 1, None);
        daemon.start_hosted_chat(kid);
        assert_ne!(
            daemon.agent_spec_under(TerminalId(7), "claude").launch,
            evil
        );
        // The limits aren't raised by it, nor by a chat an agent starts.
        assert_eq!(daemon.tools.subagent_limits(), limits);
        // What windows are told changed.
        assert_eq!(
            outside_changes(&before, &daemon.config.agents.configured),
            [
                "agents.claude command",
                "profiles.mine",
                "subagents.working"
            ]
        );
        // Lowered, they apply at once.
        daemon.take_live_config(&fresh("daemon.subagents.working = 2"));
        assert_eq!(daemon.tools.subagent_limits().working, 2);
        assert!(outside_changes(&before, &daemon.config.agents.configured).is_empty());
    }
}
