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

use std::collections::VecDeque;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    DaemonOption, DaemonSettings, Error, Event, OptionPlace, OptionSaved, OptionSetBy,
    OptionSource, Response, wire,
};
use mio::Token;

use super::{Daemon, Link, Source};
use crate::DaemonConfig;
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
    /// Why it didn't run, as its files are now, if it didn't; until it's
    /// run again, as the daemon started.
    failure: Option<Option<String>>,
    /// When to run it again, as its files have changed.
    due: Option<Instant>,
    /// A run under way, off the loop.
    running: Option<Running>,
    /// Changes windows asked for, in turn, waiting for the run under way.
    asked: VecDeque<Asked>,
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
        match answer {
            Ok((settings, fresh)) => {
                self.take_live_config(&fresh);
                self.configuring.configured = Some(settings);
                self.configuring.failure = Some(None);
                // Modules it requires now are watched too.
                self.watch_config();
            }
            // A change refused leaves the config as it was, and as it ran.
            Err(_) if asked.is_some() => {}
            Err(said) => self.configuring.failure = Some(Some(said)),
        }
        self.tell_daemon_settings(asked.map(|asked| asked.token), saved);
    }

    /// Takes from `fresh`, the daemon's config as its own config is now,
    /// what changes as the daemon runs.
    fn take_live_config(&mut self, fresh: &DaemonConfig) {
        self.config.agents.configured = fresh.agents.configured.clone();
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
    fn tell_daemon_settings(&mut self, asker: Option<Token>, saved: Option<OptionSaved>) {
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
            let saved = (Some(token) == asker).then(|| saved.clone()).flatten();
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
        let failure = match &self.configuring.failure {
            Some(failure) => failure.clone(),
            None => own.failure.clone(),
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
