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
//! What runs programs and lets the web in, the agents, the profiles,
//! limits raised, the web app's options and its password, changes only as
//! the user says (see [`settings::staging`]): a change the files make is
//! *pending*, which every window shows, for the user to apply or discard
//! there. A change the user makes in a window's settings menu is theirs,
//! and applies.

use std::collections::VecDeque;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    ConfigChange, DaemonOption, DaemonSettings, Error, Event, OptionPlace, OptionSaved,
    OptionSetBy, OptionSource, PendingConfig, Response, wire,
};
use mio::Token;

use super::{Daemon, Link, Source};
use crate::DaemonConfig;
use crate::schema::{self, Info, Place, Value};
use crate::settings::staging::{self, Discarded, Guarded};
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
    jobs: VecDeque<Job>,
}

/// A run of the config under way, off the loop.
#[derive(Debug)]
struct Running {
    answer: mpsc::Receiver<Ran>,
    /// What a window asked, if it's for that.
    job: Option<Job>,
}

/// What a window asked of the daemon's config.
#[derive(Debug, Clone)]
enum Job {
    /// To save an option.
    Change(Asked),
    /// To apply the change to what runs programs that's pending, or
    /// discard it.
    Decide {
        token: Token,
        apply: bool,
        pending: Guarded,
    },
}

impl Job {
    fn token(&self) -> Token {
        match self {
            Self::Change(asked) => asked.token,
            Self::Decide { token, .. } => *token,
        }
    }
}

/// A change a window's menu asked for.
#[derive(Debug, Clone)]
struct Asked {
    token: Token,
    info: &'static Info,
    value: Option<Value>,
}

/// What a run off the loop came to.
#[derive(Debug)]
struct Ran {
    /// What the config set, and the daemon's config with it, made off the
    /// loop, to take what changes as it runs from; or why not.
    outcome: Result<(Settings, Box<DaemonConfig>), String>,
    /// The web app's password's hash, as the state directory has it now.
    password: Option<String>,
    /// What's staged, as the user approved it now, if that changed.
    approved: Option<Guarded>,
    /// What the user discarded now, if that changed.
    discarded: Discarded,
    /// Why what the user decided couldn't be kept, if it couldn't.
    unkept: Option<String>,
}

impl Ran {
    /// A run that came to `outcome`, keeping nothing the user decided.
    fn only(outcome: Result<(Settings, Box<DaemonConfig>), String>) -> Self {
        Self {
            outcome,
            password: None,
            approved: None,
            discarded: Discarded::Same,
            unkept: None,
        }
    }
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
            let ran = match running.answer.try_recv() {
                Ok(ran) => ran,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => Ran::only(Err(
                    "the thread running the daemon's config went away".to_owned(),
                )),
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

    /// Runs the config again on a thread of its own, doing what a window
    /// asked first, if one did: saving an option, or keeping what the user
    /// decided of what runs programs.
    fn run_config(&mut self, job: Option<Job>) {
        let Some(own) = &self.config.own_config else {
            return;
        };
        let dirs = own.dirs.clone();
        let approved = own.staged.approved.clone();
        let waker = Arc::clone(&self.waker);
        let (send, answer) = mpsc::channel();
        let work = job.clone();
        let spawned = std::thread::Builder::new()
            .name("daemon-config".into())
            .spawn(move || {
                let ran = run_job(&dirs, approved, work);
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
                self.config_ran(job.as_ref(), Ran::only(Err(said)));
            }
        }
    }

    /// Takes what a run of the config came to, for `job`, if a window asked
    /// for one: what changes as the daemon runs changes, what runs
    /// programs as the user approved it, and windows are told.
    fn config_ran(&mut self, job: Option<&Job>, ran: Ran) {
        let Ran {
            outcome: ran,
            password,
            approved,
            discarded,
            unkept,
        } = ran;
        let told = job.map(Job::token);
        if let Some(why) = &unkept {
            log::warn!("{why}");
            if let Some(told) = told {
                self.send_event(told, Event::Said(why.clone()));
            }
        }
        let saved = match job {
            Some(Job::Change(asked)) => Some(OptionSaved {
                name: asked.info.name.to_owned(),
                value: asked.value.as_ref().map(Value::text),
                refused: ran.as_ref().err().cloned(),
            }),
            _ => None,
        };
        if let Some(own) = &mut self.config.own_config {
            own.staged.decided(approved, discarded);
        }
        // A run as the files changed that comes to what the daemon has, as
        // the one after a window's change does, is told no one; a folder
        // taken away and made again is watched again.
        if job.is_none() && self.config_unchanged(&ran) {
            self.watch_config();
            return;
        }
        match ran {
            Ok((settings, mut fresh)) => {
                if let Some(own) = &mut self.config.own_config {
                    let pending = own.staged.pending.clone();
                    // What runs programs, as the user approved it; the web
                    // app's is read only as the daemon starts.
                    let staged = Guarded::of(&settings, password);
                    fresh.agents.configured = own.staged.stage(&staged).configured;
                    if own.staged.pending != pending && own.staged.pending.is_some() {
                        log::warn!(
                            "the daemon's config changes what runs programs, which waits for \
                             the user"
                        );
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
            Err(_) if job.is_some() => {}
            Err(said) => {
                self.configuring.failure = Some(said);
                self.configuring.ran = true;
            }
        }
        self.tell_daemon_settings(told, saved.as_ref());
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
    /// with what runs programs as the user approved it, what changes as
    /// the daemon runs.
    fn take_live_config(&mut self, fresh: &DaemonConfig) {
        self.config.agents.configured = fresh.agents.configured.clone();
        self.refresh_subagent_limits();
        self.config.long_commands = fresh.long_commands;
        self.config.terminal.widths.vs16 = fresh.terminal.widths.vs16;
        if let (Some(history), Some(fresh)) = (&mut self.config.history, &fresh.history) {
            history.lines = fresh.lines;
            history.every = fresh.every;
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
        let pending = own.staged.revision().map(|revision| PendingConfig {
            revision,
            note: own.staged.note.clone(),
            changes: (own.staged.changes().into_iter())
                .map(|(name, before, after)| ConfigChange {
                    name,
                    before,
                    after,
                })
                .collect(),
        });
        DaemonSettings {
            machine: self.machine.name.clone(),
            dir: own.dirs.config().display().to_string(),
            options,
            failure,
            pending,
        }
    }

    /// Why the client at `token` can't change the daemon's config, if it
    /// can't: a device on the web app, a daemon relaying, or what serves
    /// agents' tools never can.
    fn not_a_window(&self, token: Token) -> Option<Error> {
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
        let asked = Asked { token, info, value };
        self.configuring.jobs.push_back(Job::Change(asked));
        // Started at once, unless a run is under way.
        self.run_config_if_due();
        Ok(Response::Done)
    }

    /// Answers [`Request::DecideDaemonConfig`](clackatoo_protocol::Request):
    /// the change to what runs programs that's pending, the one `revision`
    /// names, is applied or discarded as the user says, off the loop, and
    /// windows told then.
    pub(super) fn serve_decide_daemon_config(
        &mut self,
        token: Token,
        revision: u64,
        apply: bool,
    ) -> Result<Response, Error> {
        if let Some(refused) = self.not_a_window(token) {
            return Err(refused);
        }
        let pending = (self.config.own_config.as_ref())
            .filter(|own| own.staged.revision() == Some(revision))
            .and_then(|own| own.staged.pending.clone())
            .ok_or_else(|| {
                Error::Other(
                    "that change to the daemon's config isn't pending now: look again".to_owned(),
                )
            })?;
        let job = Job::Decide {
            token,
            apply,
            pending,
        };
        self.configuring.jobs.push_back(job);
        self.run_config_if_due();
        Ok(Response::Done)
    }

    /// Before an upgrade allowed to wait until `until`: whether, at `now`,
    /// it waits for what a window asked, which is saved, and, if the config
    /// doesn't run with it, put back. Past `until`, it goes on: the file is
    /// whole either way.
    pub(super) fn config_holds_upgrade(&self, until: Instant, now: Instant) -> bool {
        now < until
            && (self.configuring.running.as_ref()).is_some_and(|running| running.job.is_some())
    }

    /// As the upgrade is about to exec, past anything that could stop it:
    /// what a window asked that's still being done, past the time the
    /// upgrade waited, is cut off where it is, which the log says, and the
    /// next build runs the config as the files are then, or else what last
    /// ran; the window that asked, and those whose asks were waiting, are
    /// told theirs weren't done.
    pub(super) fn config_upgrading(&mut self) {
        let mut refused: Vec<Job> = self.configuring.jobs.drain(..).collect();
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
        for job in refused {
            match job {
                Job::Change(asked) => {
                    let saved = OptionSaved {
                        name: asked.info.name.to_owned(),
                        value: asked.value.as_ref().map(Value::text),
                        refused: Some(
                            "the daemon upgraded before it was saved: try again".to_owned(),
                        ),
                    };
                    self.tell_daemon_settings(Some(asked.token), Some(&saved));
                }
                Job::Decide { token, .. } => self.send_event(
                    token,
                    Event::Said(
                        "the daemon upgraded before it took your answer: answer again".to_owned(),
                    ),
                ),
            }
        }
    }
}

/// Does `job`, off the loop, with what runs programs `approved` as the user
/// approved it, then runs the config: a change saved; the user's answer to
/// what's pending kept in the state directory.
fn run_job(dirs: &settings::Dirs, mut approved: Guarded, job: Option<Job>) -> Ran {
    // What the user's answer, or the menu's change, kept.
    let mut kept = Ran::only(Err(String::new()));
    let state = dirs.state.as_deref();
    // With no state directory, it's kept only as the daemon runs.
    let approve =
        |approved: &Guarded| state.map_or(Ok(()), |state| staging::keep_approved(state, approved));
    let discard = |discarded: Option<&Guarded>| {
        state.map_or(Ok(()), |state| staging::keep_discarded(state, discarded))
    };
    let ran = match job {
        Some(Job::Change(asked)) => {
            let ran = settings::change(dirs, asked.info, asked.value);
            // The menu's change is the user's: a limit it raises, or the
            // web app's option it sets, is approved.
            let staged = ["subagents.", "web."];
            if let Ok(settings) = &ran
                && staged
                    .iter()
                    .any(|start| asked.info.name.starts_with(start))
            {
                staging::approve_option(&mut approved, asked.info.name, settings);
                match approve(&approved) {
                    Ok(()) => kept.approved = Some(approved),
                    Err(why) => kept.unkept = Some(why),
                }
            }
            ran
        }
        Some(Job::Decide { apply, pending, .. }) => {
            let decided = if apply {
                approve(&pending).and_then(|()| discard(None))
            } else {
                discard(Some(&pending))
            };
            match decided {
                Ok(()) if apply => {
                    kept.approved = Some(pending);
                    kept.discarded = Discarded::Nothing;
                }
                Ok(()) => kept.discarded = Discarded::Now(pending),
                Err(why) => kept.unkept = Some(format!("your answer couldn't be kept: {why}")),
            }
            settings::rerun(dirs)
        }
        None => settings::rerun(dirs),
    };
    // Made here, off the loop, as it reads files; the password only to
    // stage, as the web app takes it only as the daemon starts.
    kept.password = state.and_then(|state| settings::password(state).ok().flatten());
    kept.outcome = ran.map(|settings| {
        let config = Box::new(DaemonConfig::from_settings(&settings));
        (settings, config)
    });
    kept
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

    /// A change to what runs programs made outside clackatoo, as an agent
    /// writing daemon/init.lua makes, waits: every chat, the user's included,
    /// as one an agent's message wakes connects, runs the agent as approved,
    /// and no limit is raised; windows are told what's pending; and only a
    /// window may answer (CLACKA-237 review).
    #[test]
    fn a_change_outside_clackatoo_waits_and_chats_run_what_was_approved() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let dirs = settings::Dirs {
            daemon: dir.path().join("config/daemon"),
            shared: dir.path().join("config/lua"),
            state: Some(dir.path().join("state")),
        };
        let had = Guarded::of(&Settings::default(), None);
        let (staged, going) = staging::starting(&dirs, &had, staging::First::Migrated);
        daemon.config.agents.configured = going.configured;
        daemon.config.own_config = Some(settings::Own {
            dirs,
            settings: Settings::default(),
            failure: None,
            staged,
        });
        let approved = daemon.config.agents.find("claude").unwrap().launch;
        let limits = daemon.tools.subagent_limits();
        let written = Settings::from_lua(
            "daemon.agents.claude = { command = { \"evil\" } }\ndaemon.subagents.working = 64\n\
             daemon.terminals.notify_after = 30",
        )
        .unwrap();
        let fresh = Box::new(DaemonConfig::from_settings(&written));
        daemon.config_ran(None, Ran::only(Ok((written, fresh))));
        // What isn't what runs programs applies...
        assert_eq!(daemon.config.long_commands, Some(Duration::from_secs(30)));
        // ...but every chat runs claude as approved, and the limits stay.
        assert_eq!(
            daemon.config.agents.find("claude").unwrap().launch,
            approved
        );
        assert_ne!(approved, Launch::Command(vec!["evil".into()]));
        assert_eq!(daemon.tools.subagent_limits(), limits);
        let pending = daemon.daemon_settings().pending.expect("pending");
        let names: Vec<&str> = pending
            .changes
            .iter()
            .map(|change| change.name.as_str())
            .collect();
        // Claude Code given a command of the config's, which it hadn't.
        assert_eq!(names, ["agents.claude", "subagents.working"]);
        // Only a window answers: here, no client at all.
        let refused =
            daemon.serve_decide_daemon_config(Token(usize::MAX - 50), pending.revision, true);
        assert!(refused.is_err());
    }
}
