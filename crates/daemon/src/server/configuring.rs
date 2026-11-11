//! The daemon's own config as the daemon runs (see [`crate::settings`]):
//! watched on the daemon's one inotify instance, and run again a while
//! after its files last changed, off the loop; changed by windows'
//! settings menus; and told to windows, whose menus show it.
//!
//! What can change as the daemon runs does, at once, for what's started
//! from then on: the notice when a long command ends, how terminals
//! started then lay out emoji, how much of terminals' history is kept, and
//! how often, whether agents go on through upgrades, and the limits on
//! chats agents start, lowered. The rest is read only as the daemon starts
//! or upgrades, and windows are told it waits for that. A config that
//! doesn't run changes nothing: the daemon goes on by what it has, and
//! windows are told why.
//!
//! A run as the files changed, not a window's change, that changes what
//! runs programs, lets the web in, or keeps secret lines secret, is said
//! to every window, what it was and what it is, and logged (see
//! [`settings::noticed`]): nothing waits on it.

use std::collections::VecDeque;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    ConfigChange, DaemonOption, DaemonSettings, Error, Event, OptionPlace, OptionSaved,
    OptionSetBy, OptionSource, Response, wire,
};
use mio::Token;

use super::{Daemon, Link, Source};
use crate::DaemonConfig;
use crate::schema::{self, Info, Place, Value};
use crate::settings::noticed::{self, Noticed};
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
    /// What windows asked for, in turn, waiting for the run under way.
    jobs: VecDeque<Asked>,
}

/// A run of the config under way, off the loop.
#[derive(Debug)]
struct Running {
    answer: mpsc::Receiver<Ran>,
    /// What a window asked, if it's for that.
    job: Option<Asked>,
}

/// A change a window's menu asked for.
#[derive(Debug, Clone)]
struct Asked {
    token: Token,
    change: Change,
}

#[derive(Debug, Clone)]
enum Change {
    Option {
        info: &'static Info,
        value: Option<Value>,
    },
}
impl Asked {
    fn option_saved(&self, refused: Option<String>) -> OptionSaved {
        let Change::Option { info, value } = &self.change;
        OptionSaved {
            name: info.name.into(),
            value: value.as_ref().map(Value::text),
            refused,
        }
    }
}

/// What a run off the loop came to: what the config set, and the daemon's
/// config with it, made off the loop, to take what changes as it runs
/// from; or why not.
type Ran = Result<(Settings, Box<DaemonConfig>), String>;

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
            let ran = match running.answer.try_recv() {
                Ok(ran) => ran,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("the thread running the daemon's config went away".to_owned())
                }
            };
            let job = self
                .configuring
                .running
                .take()
                .and_then(|running| running.job);
            self.config_ran(job.as_ref(), ran);
        }
        if let Some(job) = self.configuring.jobs.pop_front() {
            return self.run_config(Some(job));
        }
        if (self.configuring.due).is_some_and(|due| due <= Instant::now()) {
            self.configuring.due = None;
            self.run_config(None);
        }
    }

    /// Runs the config again on a thread of its own, saving the option a
    /// window asked to first, if one did.
    fn run_config(&mut self, job: Option<Asked>) {
        let Some(own) = &self.config.own_config else {
            return;
        };
        let dirs = own.dirs.clone();
        let waker = Arc::clone(&self.waker);
        let (send, answer) = mpsc::channel();
        let work = job.clone();
        let spawned = std::thread::Builder::new()
            .name("daemon-config".into())
            .spawn(move || {
                let ran = run_job(&dirs, work);
                let _ = send.send(ran);
                if let Err(err) = waker.wake() {
                    log::warn!("waking the daemon after its config ran failed: {err}");
                }
            });
        match spawned {
            Ok(_) => self.configuring.running = Some(Running { answer, job }),
            Err(err) => {
                let said = format!("the daemon's config couldn't be run: {err}");
                log::warn!("{said}");
                self.config_ran(job.as_ref(), Err(said));
            }
        }
    }

    /// Takes what a run of the config came to, for `job`, if a window asked
    /// for one: what changes as the daemon runs changes, and windows are
    /// told; of a run as the files changed, what it changed that's
    /// noticed too.
    fn config_ran(&mut self, job: Option<&Asked>, ran: Ran) {
        let told = job.map(|asked| asked.token);
        let failure = ran.as_ref().err().cloned();
        let saved = job.map(|asked| asked.option_saved(failure.clone()));
        // A run as the files changed that comes to what the daemon has, as
        // the one after a window's change does, is told no one; a folder
        // taken away and made again is watched again.
        if job.is_none() && self.config_unchanged(&ran) {
            self.watch_config();
            return;
        }
        let mut outside = Vec::new();
        match ran {
            Ok((settings, fresh)) => {
                if job.is_none() {
                    outside = self.noticed(&settings);
                }
                self.take_live_config(&fresh);
                self.configuring.configured = Some(settings);
                self.configuring.failure = None;
                self.configuring.ran = true;
                // Modules it requires now are watched too.
                self.watch_config();
            }
            // A change refused leaves the config as it was, and as it ran.
            Err(_) if job.is_some() => {}
            Err(said) => {
                self.configuring.failure = Some(said);
                self.configuring.ran = true;
            }
        }
        self.tell_daemon_settings(told, saved.as_ref(), &outside);
    }

    /// What `settings`, a run as the files changed, outside clackatoo,
    /// changes of what's noticed since the last run: logged, for windows
    /// to say.
    fn noticed(&self, settings: &Settings) -> Vec<ConfigChange> {
        let Some(own) = &self.config.own_config else {
            return Vec::new();
        };
        let before = (self.configuring.configured.as_ref()).unwrap_or(&own.settings);
        let changes = noticed::changes(&Noticed::of(before), &Noticed::of(settings));
        (changes.into_iter())
            .map(|(name, before, after)| {
                log::warn!(
                    "the daemon's config changed outside clackatoo: {name}: {before:?} -> {after:?}"
                );
                ConfigChange {
                    name,
                    before,
                    after,
                }
            })
            .collect()
    }

    /// Whether `ran`, a run as the files changed, comes to what the daemon
    /// has: the same settings, or the same mistake.
    fn config_unchanged(&self, ran: &Result<(Settings, Box<DaemonConfig>), String>) -> bool {
        let Some(own) = &self.config.own_config else {
            return false;
        };
        let (configured, failure) = if self.configuring.ran {
            let configured = self.configuring.configured.as_ref();
            (
                configured.unwrap_or(&own.settings),
                &self.configuring.failure,
            )
        } else {
            (&own.settings, &own.failure)
        };
        match ran {
            Ok((settings, _)) => failure.is_none() && settings == configured,
            Err(said) => failure.as_ref() == Some(said),
        }
    }

    /// Takes from `fresh`, the daemon's config as its own config is now,
    /// what changes as the daemon runs.
    fn take_live_config(&mut self, fresh: &DaemonConfig) {
        let claude_plugins = self.config.agents.configured.claude_plugins;
        let codex_plugins = self.config.agents.configured.codex_plugins;
        self.config.agents.configured = fresh.agents.configured.clone();
        // Plugin ownership changes the subprocess and its prompt. Existing
        // runtimes keep one policy until the daemon restarts or upgrades.
        self.config.agents.configured.claude_plugins = claude_plugins;
        self.config.agents.configured.codex_plugins = codex_plugins;
        self.refresh_subagent_limits();
        let chats: Vec<_> = self
            .terminals
            .keys()
            .copied()
            .filter(|id| self.chat_ref(*id).is_some())
            .collect();
        for id in chats {
            let specialists = self.specialists_for(id);
            if let Some(chat) = self.chat(id) {
                chat.offer_specialists(&specialists);
            }
            self.collect_damage(id);
        }
        self.config.long_commands = fresh.long_commands;
        self.config.terminal.widths.vs16 = fresh.terminal.widths.vs16;
        if let (Some(history), Some(fresh)) = (&mut self.config.history, &fresh.history) {
            history.lines = fresh.lines;
            history.every = fresh.every;
        }
    }

    /// Tells every window of the daemon's settings, and what the run
    /// changed `outside` clackatoo that's noticed, and the one at `asker`
    /// what came of its change, `saved`: never a device on the web app, a
    /// daemon relaying, nor what serves agents' tools.
    pub(super) fn tell_daemon_settings(
        &mut self,
        asker: Option<Token>,
        saved: Option<&OptionSaved>,
        outside: &[ConfigChange],
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
                    outside: outside.to_vec(),
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

    /// Why the client at `token` can't change the daemon's config, if it
    /// can't: a device on the web app, a daemon relaying, or what serves
    /// agents' tools never can.
    pub(super) fn not_a_window(&self, token: Token) -> Option<Error> {
        let window = matches!(
            self.sources.get(token.0),
            Some(Source::Client(client))
                if !matches!(client.link, Link::Web(_))
                    && !client.relays
                    && client.serving_tools.is_none()
        );
        (!window).then(|| Error::Other("the daemon's settings are changed in a window".to_owned()))
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
        if let Some(refused) = self.not_a_window(token) {
            return Err(refused);
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
        let asked = Asked {
            token,
            change: Change::Option { info, value },
        };
        self.configuring.jobs.push_back(asked);
        // Started at once, unless a run is under way.
        self.run_config_if_due();
        Ok(Response::Done)
    }

    pub(super) fn refuse_legacy_specialist_save(&mut self, token: Token) {
        let Some(ticket) = self.take_place(token) else {
            return;
        };
        self.hand_answer_with(
            token,
            ticket,
            Response::Error(Error::Other(
                "Update the window to save specialists as SPECIALIST.md files".into(),
            )),
        );
    }

    /// Before an upgrade allowed to wait until `until`: whether, at `now`,
    /// it waits for what a window asked, which is saved, and, if the config
    /// doesn't run with it, put back. Past `until`, it goes on: the file is
    /// whole either way.
    pub(super) fn config_holds_upgrade(&self, until: Instant, now: Instant) -> bool {
        now < until
            && (self.specialist_files_busy()
                || (self.configuring.running.as_ref()).is_some_and(|running| running.job.is_some()))
    }

    /// As the upgrade is about to exec, past anything that could stop it:
    /// what a window asked that's still being done, past the time the
    /// upgrade waited, is cut off where it is, which the log says, and the
    /// next build runs the config as the files are then, or else what last
    /// ran; the window that asked, and those whose asks were waiting, are
    /// told theirs weren't done.
    pub(super) fn config_upgrading(&mut self) {
        let mut refused: Vec<Asked> = self.configuring.jobs.drain(..).collect();
        if let Some(job) = self
            .configuring
            .running
            .take()
            .and_then(|running| running.job)
        {
            log::warn!(
                "upgrading while what a window asked of the daemon's config is still being \
                 done: the next build runs the config as its files are, or else what last ran"
            );
            refused.insert(0, job);
        }
        for asked in refused {
            let why = "the daemon upgraded before it was saved: try again".to_owned();
            match &asked.change {
                Change::Option { .. } => self.tell_daemon_settings(
                    Some(asked.token),
                    Some(&asked.option_saved(Some(why))),
                    &[],
                ),
            }
        }
    }
}

/// Saves the option `job` asks to, if a window asked one, off the loop,
/// then runs the config.
fn run_job(dirs: &settings::Dirs, job: Option<Asked>) -> Ran {
    let ran = match job {
        Some(Asked {
            change: Change::Option { info, value },
            ..
        }) => settings::change(dirs, info, value),
        None => settings::rerun(dirs),
    };
    // Made here, off the loop, as it reads the web app's password.
    ran.map(|settings| {
        let config = Box::new(DaemonConfig::from_settings(&settings));
        (settings, config)
    })
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
    use super::*;
    use crate::agents::Launch;
    use crate::server::agent_files::tests::daemon_with_chat;

    #[test]
    fn native_plugin_policy_waits_for_restart_while_other_agent_settings_apply() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let settings = Settings::from_lua(
            "daemon.plugins.claude = true\ndaemon.plugins.codex = true\ndaemon.upgrade.keep_agents = false"
        ).unwrap();
        let fresh = DaemonConfig::from_settings(&settings);
        daemon.take_live_config(&fresh);
        assert!(!daemon.config.agents.configured.native_plugins("claude"));
        assert!(!daemon.config.agents.configured.native_plugins("codex"));
        assert!(!daemon.config.agents.configured.keep_agents);
        assert!(options::find("plugins.claude").unwrap().restart);
        assert!(options::find("plugins.codex").unwrap().restart);
    }

    /// A change to what runs programs made outside clackatoo, as an agent
    /// writing daemon/init.lua makes, applies, as the config says, and is
    /// noticed: what it was and what it is, for every window to say; and
    /// only a window changes the daemon's settings (CLACKA-237).
    #[test]
    fn a_change_outside_clackatoo_applies_and_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        daemon.config.own_config = Some(settings::Own {
            dirs: settings::Dirs {
                daemon: dir.path().join("config/daemon"),
                shared: dir.path().join("config/lua"),
                state: Some(dir.path().join("state")),
            },
            settings: Settings::default(),
            failure: None,
        });
        let written = Settings::from_lua(
            "daemon.agents.claude = { command = { \"other\" } }\ndaemon.subagents.working = 64\n\
             daemon.terminals.notify_after = 30",
        )
        .unwrap();
        let noticed = daemon.noticed(&written);
        let names: Vec<&str> = (noticed.iter())
            .map(|change| change.name.as_str())
            .collect();
        // Claude Code given a command of the config's, which it hadn't.
        assert_eq!(names, ["agents.claude", "subagents.working"]);
        assert_eq!(
            (noticed[1].before.as_str(), noticed[1].after.as_str()),
            ("16", "64")
        );
        let fresh = Box::new(DaemonConfig::from_settings(&written));
        daemon.config_ran(None, Ok((written, fresh)));
        assert_eq!(daemon.config.long_commands, Some(Duration::from_secs(30)));
        assert_eq!(
            daemon.config.agents.find("claude").unwrap().launch,
            Launch::Command(vec!["other".into()])
        );
        // Noticed once: run again the same, nothing is.
        let again = Settings::from_lua(
            "daemon.agents.claude = { command = { \"other\" } }\ndaemon.subagents.working = 64\n\
             daemon.terminals.notify_after = 30",
        )
        .unwrap();
        assert_eq!(daemon.noticed(&again), []);
        // Only a window changes them: here, no client at all.
        let refused =
            daemon.serve_set_daemon_option(Token(usize::MAX - 50), "subagents.working", Some("8"));
        assert!(refused.is_err());
    }
}
