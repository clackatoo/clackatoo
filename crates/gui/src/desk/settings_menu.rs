//! The settings menu in the window: `:Settings` opens it, over the panes,
//! as the finder is, and what it changes is set and saved here, as
//! `:setpersist` sets and saves (see [`crate::settings`] for the menu
//! itself). `:mappersist` binds keys and saves them, as the menu's keys
//! section does.

use std::time::Instant;

use clackatoo_protocol::{
    ConfigChange, DaemonSettings, FileLocation, OpenFile, OptionSaved, PaneTerminal, Request,
    Response, Side, TerminalId, wire,
};

use super::{Asked, Desk};
use crate::finder::view::FinderView;
use crate::input::KeyPress;
use crate::options::persist::Change;
use crate::options::settings_lua::Mapping;
use crate::options::{Case, Scope, SetBy};
use crate::settings::view::{EasyView, Laid, SettingsHit};
use crate::settings::{DaemonFacts, Facts, History, Mode, Outcome, Set, SettingsMenu};

/// The most characters the bar is given of what the daemon's config
/// changed outside clackatoo.
const MOST_NOTICED: usize = 400;

impl Desk {
    /// Runs `with` on the menu, if it's open, with what it needs to know
    /// of the window, and the changes it's made before: something it does
    /// that may change what the menu shows.
    fn with_settings<R>(
        &mut self,
        with: impl FnOnce(&mut SettingsMenu, &Facts, &mut History) -> R,
    ) -> Option<R> {
        self.settings_changed();
        self.looking_at_settings(with)
    }

    /// The menu may show something else: what was drawn of it is old.
    pub(super) fn settings_changed(&mut self) {
        self.settings_revision += 1;
    }

    /// What the menu shows, as far as what's drawn of it goes: while this
    /// is the same, what was laid out of it last is still right, whatever
    /// the mouse is over (see `App::chrome`).
    pub(crate) fn settings_shown_key(&self) -> Option<(u64, u64, u64)> {
        self.settings.as_ref()?;
        let options = self.options.store().changes();
        let keys = self
            .client_ref()
            .map_or(0, crate::client::Client::keys_changes);
        Some((self.settings_revision, options, keys))
    }

    /// As [`Desk::with_settings`], for what only looks at the menu.
    fn looking_at_settings<R>(
        &mut self,
        with: impl FnOnce(&mut SettingsMenu, &Facts, &mut History) -> R,
    ) -> Option<R> {
        let mut menu = self.settings.take()?;
        let (client, _) = self.client();
        let dir = self.config_dir();
        let mut history = std::mem::take(&mut self.settings_history);
        let done = {
            let keys = client.keys();
            let store = self.options.store();
            let facts = Facts {
                store: &store,
                window: self.options.window(),
                keys: &keys,
                dir: dir.as_deref(),
                automating: self.automating,
                daemon: self.daemon_settings.as_ref(),
            };
            menu.follow_keys(&facts, client.keys_changes());
            with(&mut menu, &facts, &mut history)
        };
        self.settings_history = history;
        self.settings = Some(menu);
        Some(done)
    }

    /// Opens the settings menu, with `query` typed to search for.
    pub(super) fn open_settings(&mut self, query: &str) {
        self.close_finder();
        self.settings_scrolled_to_top();
        if let Some(shown) = self.shown {
            self.close_editor_panel(shown.pane);
        }
        let (client, _) = self.client();
        let dir = self.config_dir();
        let case = Case::of(&self.options);
        let mode = self.settings_mode();
        let menu = {
            let keys = client.keys();
            let store = self.options.store();
            let facts = Facts {
                store: &store,
                window: self.options.window(),
                keys: &keys,
                dir: dir.as_deref(),
                automating: self.automating,
                daemon: self.daemon_settings.as_ref(),
            };
            SettingsMenu::new(query, case, mode, &facts, client.keys_changes())
        };
        self.settings = Some(menu);
        self.settings_changed();
        // The agents the daemon knows, for Easy's cards. No pane, as no
        // chat's directory is wanted.
        if self.connected {
            self.request(
                Request::ChatChoices {
                    pane: None,
                    on: None,
                },
                Asked::SettingsAgents,
            );
        }
        self.ask_daemon_settings();
        self.ask_settings_specialists();
        self.ask_settings_mcp();
        if self.connected && self.daemon_has(wire::SKILLS) {
            let pane = self.shown.map(|shown| shown.pane);
            self.request(Request::Skills { pane }, Asked::SettingsSkills);
        } else if let Some(menu) = &mut self.settings {
            menu.skills_came(Err(
                "Connect to a current daemon to browse shared skills.".into()
            ));
        }
    }

    fn ask_settings_mcp(&mut self) {
        let Some(menu) = &mut self.settings else {
            return;
        };
        let id = menu.mcp_id();
        if self.connected && self.daemon_has(wire::MANAGED_MCP) {
            if let Some(menu) = &mut self.settings {
                menu.mcp_loading();
            }
            self.request(
                Request::Mcp {
                    pane: self.shown.map(|s| s.pane),
                    action: clackatoo_protocol::McpAction::List,
                },
                Asked::SettingsMcp(id),
            );
        } else if let Some(menu) = &mut self.settings {
            menu.mcp_came(
                id,
                Err("Connect to a current daemon to manage MCP servers.".into()),
            );
        }
    }
    pub(super) fn settings_mcp(&mut self, id: u64, response: Response) -> bool {
        let result = match response {
            Response::Mcp(reply) => Ok(reply),
            Response::Error(error) => Err(error.to_string()),
            _ => return false,
        };
        let url = self
            .settings
            .as_mut()
            .and_then(|menu| menu.mcp_came(id, result));
        if let Some(url) = url {
            self.open_url(url);
        }
        self.settings_changed();
        true
    }
    pub(super) fn tick_mcp_settings(&mut self, now: std::time::Instant) -> bool {
        if self.connected
            && self
                .settings
                .as_ref()
                .is_some_and(|menu| menu.mcp_poll_due(now))
        {
            self.ask_settings_mcp();
        }
        false
    }

    fn ask_settings_specialists(&mut self) {
        if self.connected && self.daemon_has(wire::SPECIALIST_FILES) {
            self.request(
                Request::SpecialistFiles {
                    pane: self.shown.map(|s| s.pane),
                },
                Asked::SettingsSpecialists,
            );
        } else if let Some(menu) = &mut self.settings {
            menu.specialists_came(Err(
                "Connect to a current daemon to edit specialists.".into()
            ));
        }
    }
    pub(super) fn settings_specialists(&mut self, response: Response) -> bool {
        if let Response::SpecialistFiles(files) = response {
            if let Some(menu) = &mut self.settings {
                menu.specialist_files_came(files);
            }
            self.settings_changed();
            return true;
        }
        let result = match response {
            Response::Specialists(s) => Ok(s),
            Response::Error(e) => Err(e.to_string()),
            _ => return false,
        };
        if let Some(menu) = &mut self.settings {
            menu.specialists_came(result);
        }
        self.settings_changed();
        true
    }
    pub(super) fn settings_specialist_saved(
        &mut self,
        edit: &clackatoo_protocol::SpecialistDefinition,
        response: Response,
    ) -> bool {
        let result = match response {
            Response::SpecialistSaved => Ok(()),
            Response::Error(e) => Err(format!("Could not save specialist: {e}")),
            _ => return false,
        };
        let worked = result.is_ok();
        if let Some(menu) = &mut self.settings {
            menu.specialist_saved(edit, result);
        }
        if worked {
            self.ask_settings_specialists();
        }
        self.settings_changed();
        true
    }
    pub(super) fn open_specialist_settings(&mut self) {
        self.open_settings("");
        if let Some(menu) = &mut self.settings {
            menu.show_specialists();
        }
        self.settings_changed();
    }

    pub(super) fn settings_skills(&mut self, response: Response) -> bool {
        let result = match response {
            Response::Skills(skills) => Ok(skills),
            Response::Error(err) => Err(format!("Could not read skills: {err}")),
            _ => return false,
        };
        if let Some(menu) = &mut self.settings {
            menu.skills_came(result);
        }
        self.settings_changed();
        true
    }

    /// Asks the daemon for its settings, for the menu, if it can say them:
    /// the menu lists them once it has, and as they change after.
    fn ask_daemon_settings(&mut self) {
        if self.connected && self.daemon_has(wire::DAEMON_SETTINGS) {
            self.request(Request::DaemonSettings, Asked::DaemonSettings);
        }
    }

    /// Takes in the daemon's settings it was asked for. Returns whether to
    /// draw.
    pub(super) fn daemon_settings_answered(&mut self, response: Response) -> bool {
        match response {
            Response::DaemonSettings(settings) => self.daemon_settings_came(&settings, None),
            Response::Error(err) => {
                log::warn!("the daemon wouldn't say its settings: {err}");
                false
            }
            _ => false,
        }
    }

    /// The daemon's answer to a change to its option `info`: refused at
    /// once, it's as it was, and the menu says why.
    pub(super) fn daemon_option_answered(
        &mut self,
        info: &'static crate::options::Info,
        response: Response,
    ) -> bool {
        let Response::Error(err) = response else {
            return false;
        };
        self.daemon_saved(info.name);
        self.warn_saved(format!("daemon.{} wasn't saved: {err}", info.name));
        // What the menu counted on goes, as the daemon says it is.
        self.ask_daemon_settings();
        true
    }

    /// The daemon's settings, `settings`, as it says them now its config
    /// has run again, and what came of this window's change, `saved`, if
    /// that's why. A config that's newly gone wrong is said, in the bar,
    /// whether the menu's open or not. Returns whether to draw.
    pub(crate) fn daemon_settings_came(
        &mut self,
        settings: &DaemonSettings,
        saved: Option<OptionSaved>,
    ) -> bool {
        let before = (self.daemon_settings.as_ref()).and_then(|daemon| daemon.failure.clone());
        let facts = DaemonFacts::new(settings, self.remote);
        let failed = facts
            .failure
            .clone()
            .filter(|failure| Some(failure) != before.as_ref());
        // A daemon without a config of its own has no settings to show.
        self.daemon_settings = (!settings.options.is_empty()).then_some(facts);
        self.settings_changed();
        // Only what this window asked for: another's is its own to say.
        let saved = saved.filter(|saved| self.daemon_saved(&saved.name));
        match saved {
            Some(OptionSaved {
                name,
                refused: Some(why),
                ..
            }) => self.warn_saved(format!("daemon.{name} wasn't saved: {why}")),
            Some(OptionSaved {
                name,
                value: Some(value),
                ..
            }) => self.say_saved(format!(
                "saved daemon.{name} = {value} in daemon/settings.lua"
            )),
            Some(OptionSaved { name, .. }) => {
                self.say_saved(format!(
                    "daemon.{name} saved in daemon/settings.lua no more"
                ));
            }
            None => {
                if let Some(failure) = failed {
                    self.warn(format!(
                        "the daemon's config doesn't run, so it goes by what last did: {failure}"
                    ));
                }
            }
        }
        true
    }

    /// The daemon's config ran as its files changed outside clackatoo, and
    /// changed `outside` of what runs programs, lets the web in, or keeps
    /// secret lines secret: the bar says what, what it was and what it is,
    /// for the user to see; nothing waits on it. Returns whether it said.
    pub(crate) fn config_changed_outside(&mut self, outside: &[ConfigChange]) -> bool {
        if outside.is_empty() {
            return false;
        }
        let shown = |text: &str| if text.is_empty() { "none" } else { text }.to_owned();
        let changes: Vec<String> = (outside.iter())
            .map(|change| {
                format!(
                    "{}: {} → {}",
                    change.name,
                    shown(&change.before),
                    shown(&change.after)
                )
            })
            .collect();
        let said = format!("config changed outside clackatoo: {}", changes.join("; "));
        // What an agent's config chose: shown as it would show, no more.
        let plain = said
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch });
        self.warn(clackatoo_term::visible(plain, MOST_NOTICED));
        true
    }

    /// Saves the daemon's option `info` as `value`, or takes it out, as
    /// the daemon does it: the menu shows it so at once, until the daemon
    /// says how it went.
    fn save_daemon_option(
        &mut self,
        info: &'static crate::options::Info,
        value: Option<&crate::options::Value>,
    ) {
        if !self.daemon_has(wire::DAEMON_SETTINGS) {
            return self.warn_saved("this daemon is too old to change its settings here".into());
        }
        if let Some(daemon) = &mut self.daemon_settings {
            daemon.counting_on(info, value.cloned());
        }
        *self.daemon_saving.entry(info.name).or_default() += 1;
        self.request(
            Request::SetDaemonOption {
                name: info.name.to_owned(),
                value: value.map(crate::options::Value::text),
            },
            Asked::DaemonOption(info),
        );
    }

    /// A change this window asked the daemon to save of its option called
    /// `name` came back, or went: whether one was waiting.
    fn daemon_saved(&mut self, name: &str) -> bool {
        let Some(count) = self.daemon_saving.get_mut(name) else {
            return false;
        };
        *count -= 1;
        if *count == 0 {
            self.daemon_saving.remove(name);
        }
        true
    }

    /// The mode the menu was used in last, as the file it's remembered in
    /// says: Easy the first time.
    fn settings_mode(&self) -> Mode {
        (self.settings_mode_file.as_deref())
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| Mode::read(&text))
            .unwrap_or(Mode::Easy)
    }

    /// Remembers `mode` as the one the menu opens in next, in any window.
    fn remember_settings_mode(&self, mode: Mode) {
        let Some(path) = self.settings_mode_file.as_deref() else {
            return;
        };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                // Replaced whole, so a window reading it never sees half,
                // from a name of its own, so two windows writing at once
                // don't write into one file.
                static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let count = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let partial =
                    path.with_extension(format!("{}-{count}.partial", std::process::id()));
                std::fs::write(&partial, format!("{}\n", mode.word()))?;
                std::fs::rename(&partial, path)
            });
        if let Err(err) = written {
            log::warn!(
                "couldn't remember the settings menu's mode in {}: {err}",
                path.display()
            );
        }
    }

    /// The menu's mode, if it's open.
    pub(crate) fn settings_mode_shown(&self) -> Option<Mode> {
        self.settings.as_ref().map(|menu| menu.mode)
    }

    /// Handles a key while the settings menu is open.
    pub(super) fn settings_key(&mut self, press: &KeyPress<'_>) {
        if let Some(outcome) =
            self.with_settings(|menu, facts, history| menu.key(press, facts, history))
        {
            self.settings_did(outcome);
        }
    }

    /// A click on `hit`, a part of the menu.
    pub(crate) fn settings_pointer(&mut self, hit: SettingsHit) {
        if let Some(outcome) =
            self.with_settings(|menu, facts, history| menu.pointer(hit, facts, history))
        {
            self.settings_did(outcome);
        }
    }

    /// Whether a slider of the menu's is being dragged.
    pub(crate) fn settings_sliding(&self) -> bool {
        self.settings.as_ref().is_some_and(SettingsMenu::sliding)
    }

    /// The slider being dragged is dragged to `at` thousandths along.
    pub(crate) fn settings_slide(&mut self, at: u16) {
        if let Some(outcome) = self.with_settings(|menu, _, _| menu.slide(at)) {
            self.settings_did(outcome);
        }
    }

    /// The slider being dragged is let go.
    pub(crate) fn settings_slid(&mut self) {
        if let Some(outcome) = self.with_settings(SettingsMenu::easy_slid) {
            self.settings_did(outcome);
        }
    }

    /// Easy's page was laid out again, `scrolling` so: it goes on to where
    /// it's to be scrolled now, smoothly if `smoothscroll`, or at once to a
    /// new page's top.
    pub(crate) fn settings_drawn(&mut self, laid: &Laid) {
        let smooth = self.options.smoothscroll;
        if let Some(menu) = &mut self.settings
            && let Some(scrolling) = laid.scrolling()
        {
            let (to, jump) = menu.easy_drawn(scrolling);
            let smooth = smooth && !jump;
            (self.settings_scroll.page).to(f64::from(to), smooth, Instant::now());
        }
        // The font picker's list, from where it was if it was open.
        match laid.fonts_top() {
            Some(top) => {
                self.settings_scroll.rows_from(true, top, smooth);
            }
            None => self.settings_scroll.fonts = None,
        }
    }

    /// What Easy shows, if the menu's open in Easy.
    pub(crate) fn easy_view(&mut self) -> Option<EasyView> {
        if self.settings_mode_shown() != Some(Mode::Easy) {
            return None;
        }
        self.looking_at_settings(|menu, facts, _| menu.easy_view(facts))
    }

    /// Takes in the agents the daemon knows, for Easy's cards, if the menu
    /// is still open. Returns whether to draw.
    pub(super) fn settings_agents(&mut self, response: clackatoo_protocol::Response) -> bool {
        let choices = match response {
            clackatoo_protocol::Response::ChatChoices(choices) => choices,
            clackatoo_protocol::Response::Error(err) => {
                log::warn!("the daemon wouldn't say which agents it knows: {err}");
                return false;
            }
            _ => return false,
        };
        self.settings_changed();
        let Some(menu) = &mut self.settings else {
            return false;
        };
        let agents = (choices.agents.iter())
            .map(|provider| {
                let installed = !matches!(
                    provider.models,
                    clackatoo_protocol::ProviderModels::NotInstalled
                );
                (provider.agent.clone(), provider.title.clone(), installed)
            })
            .collect();
        menu.set_agents(agents);
        true
    }

    /// A click on row `row` of the menu, or a double click if `open`.
    pub(super) fn settings_click(&mut self, row: usize, open: bool) {
        if let Some(outcome) =
            self.with_settings(|menu, facts, history| menu.click(row, open, facts, history))
        {
            self.settings_did(outcome);
        }
    }

    /// The wheel turned over the menu, moving what's picked `by` rows.
    pub(super) fn settings_wheel(&mut self, by: isize) {
        if let Some(outcome) = self.with_settings(|menu, _, _| menu.scroll(by)) {
            self.settings_did(outcome);
        }
    }

    /// Types `text` into the menu.
    pub(super) fn paste_into_settings(&mut self, text: &str) {
        if let Some(outcome) = self.with_settings(|menu, _, _| menu.paste(text)) {
            self.settings_did(outcome);
        }
    }

    /// What the menu shows, if it's open.
    pub(super) fn settings_view(
        &mut self,
        list: (usize, usize),
        preview: Option<(usize, usize)>,
    ) -> Option<FinderView> {
        if self.settings_mode_shown() != Some(Mode::Advanced) {
            self.settings_scroll.list = None;
            return None;
        }
        // Drawn from where its list has scrolled to, as it scrolls.
        let mut scroll = std::mem::take(&mut self.settings_scroll);
        let smooth = self.options.smoothscroll;
        let mut shown = [false; 2];
        let view = self.looking_at_settings(|menu, facts, _| {
            menu.view(facts, list, preview, &mut |fonts, top| {
                shown[usize::from(fonts)] = true;
                scroll.rows_from(fonts, top, smooth)
            })
        });
        // A list not shown starts where it's next shown, at once.
        if !shown[0] {
            scroll.list = None;
        }
        if !shown[1] {
            scroll.fonts = None;
        }
        self.settings_scroll = scroll;
        view
    }

    /// Whether the settings menu is open.
    pub(crate) fn settings_open(&self) -> bool {
        self.settings.is_some()
    }

    /// The top bar's cog: opens the settings menu, in the mode used last,
    /// or closes it if it's open.
    pub(crate) fn toggle_settings(&mut self) {
        if self.settings.take().is_none() {
            self.open_settings("");
        }
    }

    /// Whether the menu waits for the fonts installed, which
    /// [`Desk::font_families`] gives it.
    pub(crate) fn wants_font_families(&self) -> bool {
        self.settings
            .as_ref()
            .is_some_and(SettingsMenu::wants_fonts)
    }

    /// Takes in the font families installed, each with whether it's
    /// monospace, for the menu's font picker.
    pub(crate) fn font_families(&mut self, families: Vec<(String, bool)>) {
        let font = self.options.font.clone();
        self.settings_changed();
        if let Some(menu) = &mut self.settings {
            menu.fonts(families, &font);
        }
    }

    /// Signs in to managed MCP server `server`, which chat `chat`'s
    /// agent can't use until the user does, as Settings' Sign in does:
    /// the daemon opens the login and tells the chat how it goes.
    pub(super) fn sign_in_to_mcp(&mut self, chat: TerminalId, server: String) {
        if self.automating || self.controlled {
            return self.refuse(MCP_USERS_ONLY.into());
        }
        if !self.needs_daemon("signing in to MCP servers", wire::MCP_SIGN_IN) {
            return;
        }
        let Some(pane) = self.chat_pane_shown(chat) else {
            return self.refuse("the chat isn't in the tab shown".to_owned());
        };
        self.request(
            Request::Mcp {
                pane: Some(pane),
                action: clackatoo_protocol::McpAction::Authenticate { name: server },
            },
            Asked::ChatMcpSignIn,
        );
    }

    /// The daemon answered a chat's sign in: with the login to open in
    /// the browser, or why there's none. Returns whether to draw.
    pub(super) fn chat_mcp_signed_in(&mut self, response: Response) -> bool {
        match response {
            Response::Mcp(reply) => match reply.login_url {
                Some(url) => self.open_url(url),
                None if !reply.notice.is_empty() => self.refuse(reply.notice),
                None => {}
            },
            Response::Error(error) => self.refuse(error.to_string()),
            _ => return false,
        }
        true
    }

    /// Carries out what a key or click did to the menu.
    fn settings_mcp_action(&mut self, action: clackatoo_protocol::McpAction, id: u64) {
        if self.automating || self.controlled {
            if let Some(menu) = &mut self.settings {
                menu.mcp_came(id, Err(MCP_USERS_ONLY.into()));
            }
        } else {
            self.request(
                Request::Mcp {
                    pane: self.shown.map(|s| s.pane),
                    action,
                },
                Asked::SettingsMcp(id),
            );
        }
    }

    fn settings_did(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Nothing | Outcome::Changed | Outcome::Fonts => {}
            Outcome::Mcp { action, id } => self.settings_mcp_action(action, id),
            Outcome::Close => self.settings = None,
            Outcome::Set(sets) => {
                // The daemon's, and the window's that run programs, are
                // the user's to change: never an agent's input, nor what a
                // chat with Control of the window does.
                let theirs = |set: &Set| {
                    set.info.scope == Scope::Daemon || crate::options::runs_programs(set.info)
                };
                if (self.automating || self.controlled) && sets.iter().any(theirs) {
                    return self.warn_saved(
                        "an agent's input can't change that setting: it's yours".into(),
                    );
                }
                // The daemon's are saved by the daemon, which runs them.
                let (daemons, sets): (Vec<_>, Vec<_>) =
                    (sets.into_iter()).partition(|set| set.info.scope == Scope::Daemon);
                for set in daemons {
                    self.save_daemon_option(set.info, set.saved.as_ref());
                }
                if sets.is_empty() {
                    return;
                }
                let mut saved = Vec::new();
                for set in sets {
                    // One read only as a window opens is saved, for windows
                    // opened after.
                    if !set.info.restart {
                        self.set_option(set.info, set.live, SetBy::RUNTIME);
                    }
                    self.counting_on_saving(set.info, set.saved.clone());
                    saved.push((set.info, set.saved));
                }
                // Several, as one control sets them, in one write.
                match <[_; 1]>::try_from(saved) {
                    Ok([(info, value)]) => self.save_change(Change::Option { info, value }),
                    Err(saved) => self.save_change(Change::Options(saved)),
                }
            }
            Outcome::SaveSpecialist {
                specialist,
                scope,
                path,
            } => {
                if self.automating || self.controlled {
                    if let Some(menu) = &mut self.settings {
                        menu.specialist_saved(
                            &specialist,
                            Err(
                                "An agent's input can't save specialists: this setting is yours"
                                    .into(),
                            ),
                        );
                    }
                    return;
                }
                self.request(
                    Request::SaveSpecialistFile {
                        pane: self.shown.map(|s| s.pane),
                        scope,
                        path,
                        specialist: specialist.clone(),
                    },
                    Asked::SettingsSpecialistSaved(specialist),
                );
            }
            Outcome::Mode(mode) => self.remember_settings_mode(mode),
            Outcome::ClearLocal(info) => self.drop_local_option(info),
            Outcome::Keys(changes) => {
                for (mapping, bound) in changes {
                    self.map_keys(mapping, bound);
                }
            }
            Outcome::Open(place) => {
                self.settings = None;
                self.open_config_line(&place, false);
            }
            Outcome::OpenDaemons(place) => {
                self.settings = None;
                self.open_config_line(&place, true);
            }
            Outcome::OpenSkill(place) => {
                self.settings = None;
                let Some(shown) = self.shown else {
                    return;
                };
                let path = place.file.to_string();
                let landing = FileLocation {
                    path: path.clone(),
                    line: 0,
                    col: 0,
                    offset: None,
                    text: String::new(),
                };
                self.request(
                    Request::Split {
                        pane: shown.pane,
                        side: Side::Right,
                        terminal: PaneTerminal::File(OpenFile { path, cwd: None }),
                        on: None,
                    },
                    Asked::PaneAt(Some(landing)),
                );
            }
        }
    }

    /// Counts on `info`'s option being saved as `value`: the menu shows and
    /// steps from it as configured so at once, as keys pressed before the
    /// file's written must, and it's put back if the save fails (see
    /// [`Desk::settings_save_came`]).
    fn counting_on_saving(
        &mut self,
        info: &'static crate::options::Info,
        value: Option<crate::options::Value>,
    ) {
        let before = self.options.store().configured(info).cloned();
        let (count, _) = (self.settings_pending)
            .entry(info.name)
            .or_insert((0, before));
        *count += 1;
        let ((), _) = (self.options).change(|store, _| store.saved(info, value));
    }

    /// A save of `info`'s option the menu counted on came back, as
    /// `worked` says: if it failed, it's configured as it was before.
    pub(super) fn settings_save_came(&mut self, info: &'static crate::options::Info, worked: bool) {
        let Some((count, before)) = self.settings_pending.get_mut(info.name) else {
            return;
        };
        *count -= 1;
        if worked && *count > 0 {
            return;
        }
        let before = before.clone();
        self.settings_pending.remove(info.name);
        if !worked {
            let ((), _) = (self.options).change(|store, _| store.put_configured(info, before));
        }
    }

    /// Opens the config's line at `place` in an editor beside the focused
    /// pane. The window's config is this machine's, so it's opened only by
    /// a daemon on this machine: another machine's would open a file of its
    /// own of that path, if it had one. The daemon's, if `daemons`, is on
    /// the daemon's machine, wherever that is.
    fn open_config_line(&mut self, place: &crate::options::provenance::Place, daemons: bool) {
        let path = place.file.to_string();
        let name = (std::path::Path::new(&path).file_name())
            .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned());
        let Some(shown) = self.shown else {
            return self.refuse(format!("{name} needs a workspace to open in"));
        };
        let own = self.own_machine().map(|own| own.id);
        // A pane of another machine's makes panes there, unless the daemon
        // can be told this one.
        let elsewhere = own.is_none() && self.pane_machine(shown.pane).is_some();
        if elsewhere || self.remote && !daemons {
            return self.refuse(format!(
                "{name} is on this machine; the workspace's daemon is on another"
            ));
        }
        let landing = FileLocation {
            path: path.clone(),
            line: u64::from(place.line.saturating_sub(1)),
            col: 0,
            offset: None,
            text: String::new(),
        };
        self.request(
            Request::Split {
                pane: shown.pane,
                side: Side::Right,
                terminal: PaneTerminal::File(OpenFile { path, cwd: None }),
                on: own,
            },
            Asked::PaneAt(Some(landing)),
        );
    }

    /// Binds `mapping`'s keys for every window, and saves them in
    /// settings.lua; or if not `bound`, puts them back as the window starts
    /// them, and takes them out.
    fn map_keys(&mut self, mapping: Mapping, bound: bool) {
        if self.automating {
            return self.warn("an agent's input can't save keys".into());
        }
        let (client, _) = self.client();
        client.map_keys(&mapping, bound, &SetBy::SAVED);
        self.follow_client();
        self.save_change(Change::Keys { mapping, bound });
    }

    /// `:mappersist`: binds keys and saves them, unless the user's own
    /// config binds them as it loads, which would outdo what's saved: then
    /// the bar says where, and nothing changes, as what the config bound
    /// is still what's bound.
    pub(super) fn map_persist(&mut self, mapping: Mapping) {
        if self.automating {
            return self.warn("an agent's input can't save keys".into());
        }
        let dir = self.config_dir();
        let (client, _) = self.client();
        let locked = (mapping.modes.iter())
            .map(|&mode| client.keys().bound_by(mode, &mapping.keys))
            .find(SetBy::config_loading);
        let Some(by) = locked else {
            return self.map_keys(mapping, true);
        };
        let place = by
            .place(dir.as_deref())
            .unwrap_or_else(|| "your config".into());
        self.warn(format!(
            "{place} binds {}, which outdoes settings.lua: change it there",
            crate::keymap::display(&mapping.keys)
        ));
    }
}

/// Why an agent with native access to the window can't use MCP Settings,
/// nor sign in to a server.
const MCP_USERS_ONLY: &str = "Only the user may configure MCP servers or store credentials.";
