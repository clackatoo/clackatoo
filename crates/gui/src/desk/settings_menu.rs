//! The settings menu in the window: `:Settings` opens it, over the panes,
//! as the finder is, and what it changes is set and saved here, as
//! `:setpersist` sets and saves (see [`crate::settings`] for the menu
//! itself). `:mappersist` binds keys and saves them, as the menu's keys
//! section does.

use clackatoo_protocol::{FileLocation, OpenFile, PaneTerminal, Request, Side};

use super::{Asked, Desk};
use crate::finder::view::FinderView;
use crate::input::KeyPress;
use crate::options::persist::Change;
use crate::options::settings_lua::Mapping;
use crate::options::{Case, SetBy};
use crate::settings::view::{EasyView, SettingsHit};
use crate::settings::{Facts, History, Mode, Outcome, SettingsMenu};

impl Desk {
    /// Runs `with` on the menu, if it's open, with what it needs to know
    /// of the window, and the changes it's made before.
    fn with_settings<R>(
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
            };
            SettingsMenu::new(query, case, mode, &facts, client.keys_changes())
        };
        self.settings = Some(menu);
        // The agents the daemon knows, for Easy's cards.
        if self.connected {
            let pane = self.shown.map(|shown| shown.pane);
            self.request(
                clackatoo_protocol::Request::ChatChoices { pane, on: None },
                Asked::ChatChoices,
            );
        }
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
                // Replaced whole, so a window reading it never sees half.
                let partial = path.with_extension("partial");
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

    /// Easy's page was drawn from its part `top`, as it scrolled.
    pub(crate) fn settings_drawn(&mut self, top: usize) {
        if let Some(menu) = &mut self.settings {
            menu.easy_drawn(top);
        }
    }

    /// What Easy shows, if the menu's open in Easy.
    pub(crate) fn easy_view(&mut self) -> Option<EasyView> {
        if self.settings_mode_shown() != Some(Mode::Easy) {
            return None;
        }
        self.with_settings(|menu, facts, _| menu.easy_view(facts))
    }

    /// Takes in the agents the daemon knows, for Easy's cards.
    pub(super) fn settings_agents(&mut self, choices: &clackatoo_protocol::ChatChoices) {
        let Some(menu) = &mut self.settings else {
            return;
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
            return None;
        }
        self.with_settings(|menu, facts, _| menu.view(facts, list, preview))
    }

    /// Whether the settings menu is open.
    pub(crate) fn settings_open(&self) -> bool {
        self.settings.is_some()
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
        if let Some(menu) = &mut self.settings {
            menu.fonts(families, &font);
        }
    }

    /// Carries out what a key or click did to the menu.
    fn settings_did(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Nothing | Outcome::Changed | Outcome::Fonts => {}
            Outcome::Close => self.settings = None,
            Outcome::Set(sets) => {
                for set in sets {
                    // One read only as a window opens is saved, for windows
                    // opened after.
                    if !set.info.restart {
                        self.set_option(set.info, set.live, SetBy::RUNTIME);
                    }
                    self.save_change(Change::Option {
                        info: set.info,
                        value: set.saved,
                    });
                }
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
                self.open_config_line(&place);
            }
        }
    }

    /// Opens the config's line at `place` in an editor beside the focused
    /// pane. The config is this machine's, so it's opened only by a daemon
    /// on this machine: another machine's would open a file of its own of
    /// that path, if it had one.
    fn open_config_line(&mut self, place: &crate::options::provenance::Place) {
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
        if self.remote || elsewhere {
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
