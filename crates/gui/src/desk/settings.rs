//! The desk's options: setting them, from `:set` and config, saving them
//! with `:setpersist`, and passing them on to the panes they change.

use crate::options::Window as _;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use super::Desk;
use crate::fonts::Fonts;
use crate::lua::ToLua;
use crate::options::persist::{self, Change, Saved};
use crate::options::{self, Decimal, Info, SetBy, Setting, Store, Value, View};

/// The `font` and `fontsize` the window draws text in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FontInUse {
    family: String,
    size: Decimal,
}

impl FontInUse {
    fn of(options: &options::Options) -> Self {
        Self {
            family: options.font.clone(),
            size: options.fontsize,
        }
    }

    /// The family, or `None` for the system's monospace font.
    fn family(&self) -> Option<&str> {
        Some(self.family.as_str()).filter(|family| !family.is_empty())
    }
}

impl Default for FontInUse {
    fn default() -> Self {
        Self::of(&options::Options::default())
    }
}

/// Where what `:setpersist` saves comes back, once it's written.
#[derive(Debug)]
pub(super) struct Saving {
    reply: Sender<Saved>,
    answers: Receiver<Saved>,
}

impl Default for Saving {
    fn default() -> Self {
        let (reply, answers) = mpsc::channel();
        Self { reply, answers }
    }
}

#[cfg(test)]
impl Saving {
    /// Waits for the next setting to be saved, for tests. One thread
    /// saves for every test, each save waiting on the disk twice, so a
    /// busy machine's tests can queue for a while.
    pub(super) fn wait(&self) -> Saved {
        (self.answers)
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("saved")
    }
}

impl Desk {
    /// Starts from `options` rather than the defaults, as the command line
    /// and startup.toml set them, before config runs: those in `fixed`,
    /// which the command line set, are the window's own, which what every
    /// window shares doesn't change. A window not in a client yet, as in
    /// tests, starts the options every window shares from `options` too.
    pub(crate) fn start_options(&mut self, options: options::Options, fixed: Vec<&'static Info>) {
        let fixed: Vec<(&'static Info, Value)> = (fixed.into_iter())
            .map(|info| (info, options.get(info)))
            .collect();
        if !self.in_client() {
            self.options = View::new(Store::new(options).shared(), self.options.window());
        }
        let ((), _) = self.options.change(|store, window| {
            for (info, value) in fixed {
                // The command line's are values the option can hold.
                let _ = store.set_local(window, info, value, SetBy::STARTUP);
            }
        });
        self.font_in_use = FontInUse::of(&self.options);
        self.options_changed();
    }

    /// Notes that `fonts` were loaded in `family` (`None` for the system's
    /// monospace font) at `points`, which may not be the font the options
    /// say: [`use_font`](Self::use_font) puts that right before the window
    /// opens.
    pub(crate) fn fonts_loaded(&mut self, family: Option<&str>, points: f32) {
        self.font_in_use = FontInUse::of(&options::Options::drawing_with(&crate::FontOptions {
            family: family.map(str::to_owned),
            size: points,
        }));
    }

    /// Draws text in `fonts` as the `font` and `fontsize` options say, if
    /// they've changed since last asked. A family that isn't installed is
    /// refused, putting `font` back as it was, and a new size is still
    /// taken. Returns whether `fonts` changed, so that glyphs drawn in the
    /// old must be thrown away, and the panes fitted to the new cell.
    ///
    /// The window asks once a frame, so however many times they're set
    /// between frames, the font changes once. Nor does it change while
    /// `init.lua` runs: a reload puts the options back as they started,
    /// and the config sets the font again, which mustn't change it twice.
    pub(crate) fn use_font(&mut self, fonts: &mut Fonts) -> bool {
        if self.config_running(Instant::now()) {
            return false;
        }
        // Asked every frame, so it doesn't copy the family to ask.
        let in_use = &self.font_in_use;
        if (&self.options.font, self.options.fontsize) == (&in_use.family, in_use.size) {
            return false;
        }
        let wanted = FontInUse::of(&self.options);
        let size = wanted.size.as_f32();
        let used = match fonts.set_terminal(wanted.family(), size) {
            Ok(()) => wanted,
            Err(err) => {
                let font = options::find("font").expect("an option");
                let family = self.font_in_use.family.clone();
                // Put back where it was set: this window's own, or every
                // window's.
                let own = self
                    .options
                    .store()
                    .local(self.options.window(), font)
                    .is_some();
                if own {
                    self.set_local_option(font, Value::Text(family.clone()), SetBy::RUNTIME);
                } else {
                    self.set_option(font, Value::Text(family.clone()), SetBy::RUNTIME);
                }
                self.warn(format!("{err:#}, so the font stays as it was"));
                let used = FontInUse {
                    family,
                    size: wanted.size,
                };
                // The family in use isn't looked for again, so this can't
                // fail.
                if fonts.set_terminal(used.family(), size).is_err() {
                    return false;
                }
                used
            }
        };
        let changed = used != self.font_in_use;
        self.font_in_use = used;
        changed
    }

    /// Sets `info`'s option to `value` for every window, which it can
    /// hold, noting `by` as what set it, as `:set` does: this window's own
    /// value of it goes, so it has what it set. Tells Lua and the panes if
    /// that changes it; the other windows follow.
    pub(super) fn set_option(&mut self, info: &'static Info, value: Value, by: SetBy) {
        let (value, changed) = self.options.change(|store, window| {
            store.drop_local(window, info);
            let _ = store.set(info, value, by);
            store.get(info)
        });
        if changed {
            self.options_changed();
        }
        // Lua hears every change, even its own, so its copy ends up as the
        // windows' whatever order changes cross in.
        self.tell_lua(ToLua::Option {
            option: info,
            value,
        });
    }

    /// Sets `info`'s option to `value` for this window alone, as
    /// `:setlocal` does, noting `by` as what set it. Says why not, if it
    /// can't be.
    fn set_local_option(&mut self, info: &'static Info, value: Value, by: SetBy) {
        let (set, changed) = self
            .options
            .change(|store, window| store.set_local(window, info, value, by));
        if let Err(why) = set {
            self.warn(why);
        }
        if changed {
            self.options_changed();
        }
    }

    /// Takes this window's own value of `info`'s option away, so it has
    /// every window's again, as `:set nu<` does.
    pub(super) fn drop_local_option(&mut self, info: &'static Info) {
        let (_, changed) = self
            .options
            .change(|store, window| store.drop_local(window, info));
        if changed {
            self.options_changed();
        }
    }

    /// Puts `info`'s option back as the process started it, for every
    /// window, forgetting what set it since, and this window's own value
    /// back as the window opened, as `:set nu&` does, telling Lua and the
    /// panes if that changes it.
    fn restore_option(&mut self, info: &'static Info) {
        let (value, changed) = self.options.change(|store, window| {
            store.restore_local(window, info);
            store.restore(info);
            store.get(info)
        });
        if changed {
            self.options_changed();
        }
        self.tell_lua(ToLua::Option {
            option: info,
            value,
        });
    }

    /// Carries out `:set` with `words` after it, saving what it sets where
    /// it's kept too if `persist`, as `:setpersist` does, and saying what
    /// set each option it shows if `verbose`, as `:verbose set` does. With
    /// `local`, as `:setlocal`, it sets this window's own values instead.
    pub(super) fn run_set(&mut self, words: &str, persist: bool, verbose: bool, local: bool) {
        let settings = match options::parse_set(words) {
            Ok(settings) => settings,
            Err(err) => {
                self.warn(err);
                return;
            }
        };
        if settings.is_empty() {
            // As in Vim: the options not as they started, or with
            // `:setlocal`, this window's own.
            let start = self.options.start();
            let store = self.options.store();
            let window = self.options.window();
            let changed: Vec<&Info> = (options::ALL.iter())
                .filter(|info| {
                    if local {
                        store.local(window, info).is_some()
                    } else {
                        self.options.get(info) != start.get(info)
                    }
                })
                .collect();
            drop(store);
            let changed: Vec<String> = (changed.into_iter())
                .map(|info| self.option_shown(info, verbose))
                .collect();
            self.say(match (changed.is_empty(), local) {
                (true, false) => "every option is as it started".into(),
                (true, true) => "this window has no options of its own".into(),
                (false, _) => changed.join("  "),
            });
            return;
        }
        // The config's own line, if its command runs this.
        let by = self.set_by.clone().unwrap_or(SetBy::RUNTIME);
        let mut shown = Vec::new();
        for setting in settings {
            let (info, value) = match setting {
                Setting::To(info, value) => (info, Some(value)),
                Setting::Flip(info) => match self.options.get(info) {
                    Value::Bool(on) => (info, Some(Value::Bool(!on))),
                    _ => continue,
                },
                Setting::Default(info) => (info, None),
                Setting::Show(info) => {
                    shown.push(self.option_shown(info, verbose));
                    continue;
                }
                Setting::Shared(info) => {
                    self.drop_local_option(info);
                    continue;
                }
            };
            if local {
                // As Vim's `:setlocal nu&`: this window's own, as it starts.
                let value = value.unwrap_or_else(|| self.options.start().get(info));
                if let Err(why) = info.settable_now() {
                    self.warn(why);
                    continue;
                }
                self.set_local_option(info, value, by.clone());
                continue;
            }
            if info.restart && persist {
                // Saved for the next window, as it's read only then.
                self.persist(info, value);
                continue;
            }
            if let Err(why) = info.settable_now() {
                self.warn(why);
                continue;
            }
            match &value {
                Some(now) => self.set_option(info, now.clone(), by.clone()),
                None => self.restore_option(info),
            }
            if persist {
                self.persist_unless_configured(info, value);
            }
        }
        if !shown.is_empty() {
            self.say(shown.join("  "));
        }
    }

    /// `info`'s option as `:set` shows it, with what set it if `verbose`.
    fn option_shown(&self, info: &Info, verbose: bool) -> String {
        let shown = self.options.show(info);
        if !verbose {
            return shown;
        }
        let dir = self.config_dir();
        let said = self.options.set_by(info).said(dir.as_deref());
        let own = self
            .options
            .store()
            .local(self.options.window(), info)
            .is_some();
        if own {
            format!("{shown} · this window's own, {said}")
        } else {
            format!("{shown} · {said}")
        }
    }

    /// Saves `info`'s option as `value`, as `:setpersist` does, unless the
    /// user's own config set it as it loaded, which would outdo what's
    /// saved: then it's set for now only, and the bar says where config
    /// sets it.
    fn persist_unless_configured(&mut self, info: &'static Info, value: Option<Value>) {
        let dir = self.config_dir();
        let configured = (self.options.store().configured(info))
            .filter(|(_, by)| by.config_loading())
            .map(|(_, by)| by.place(dir.as_deref()));
        match configured {
            Some(place) => {
                let place = place.unwrap_or_else(|| "your config".into());
                self.warn(format!(
                    "{place} sets {}, so it's set for now and not saved",
                    info.name
                ));
            }
            None => self.persist(info, value),
        }
    }

    /// Has `info`'s option saved as `value`, or with none, taken out of
    /// its file, off the window's thread; [`settings_saved`](Self::settings_saved) says what
    /// came of it.
    pub(super) fn persist(&mut self, info: &'static Info, value: Option<Value>) {
        self.save_change(Change::Option { info, value });
    }

    /// Has `change` saved in its file, off the window's thread;
    /// [`settings_saved`](Self::settings_saved) says what came of it.
    pub(super) fn save_change(&mut self, change: Change) {
        let (client, _) = self.client();
        let (Some(dir), Some(wake)) = (client.config_dir(), client.waker()) else {
            return self.warn("there's no config directory to save settings in".into());
        };
        persist::save(&dir, change, self.saving.reply.clone(), wake);
    }

    /// clackatoo's config directory, if the window has one.
    pub(super) fn config_dir(&self) -> Option<PathBuf> {
        self.client_ref()?.config_dir()
    }

    /// Says what came of saving settings since last asked. Returns whether
    /// anything did.
    pub(super) fn settings_saved(&mut self) -> bool {
        let saved: Vec<Saved> = self.saving.answers.try_iter().collect();
        let any = !saved.is_empty();
        for saved in saved {
            self.said_saved(saved);
        }
        any
    }

    pub(super) fn said_saved(&mut self, saved: Saved) {
        let Saved {
            change,
            file,
            result,
        } = saved;
        let (info, value) = match change {
            Change::Option { info, value } => (info, value),
            Change::Options(options) => return self.said_saved_together(options, file, result),
            Change::Keys { mapping, bound } => {
                let keys = crate::keymap::display(&mapping.keys);
                return match result {
                    Err(why) if !bound => {
                        self.warn_saved(format!("{keys} wasn't taken out: {why}"));
                    }
                    Err(why) => self.warn_saved(format!("{keys} wasn't saved: {why}")),
                    Ok(()) if bound => self.say_saved(format!("saved {keys} in {file}")),
                    Ok(()) => self.say_saved(format!("{keys} is saved in {file} no more")),
                };
            }
        };
        self.settings_save_came(info, result.is_ok());
        if let Err(why) = result {
            return self.warn_saved(format!("{} wasn't saved: {why}", info.name));
        }
        // What it's configured to, now, as the menu says it.
        let ((), _) = self
            .options
            .change(|store, _| store.saved(info, value.clone()));
        let later = if info.restart {
            ", for windows opened after"
        } else {
            ""
        };
        self.say_saved(match value {
            Some(value) => format!(
                "saved {} in {file}{later}",
                options::show_value(info, &value)
            ),
            None => format!("{} is saved in {file} no more{later}", info.name),
        });
    }

    /// Says what came of saving `options` together in `file`, as one
    /// line: `saved number norelativenumber in client/settings.lua`.
    fn said_saved_together(
        &mut self,
        options: Vec<(&'static options::Info, Option<options::Value>)>,
        file: &str,
        result: Result<(), String>,
    ) {
        let names: Vec<&str> = options.iter().map(|(info, _)| info.name).collect();
        for (info, _) in &options {
            self.settings_save_came(info, result.is_ok());
        }
        if let Err(why) = result {
            return self.warn_saved(format!("{} weren't saved: {why}", names.join(" and ")));
        }
        let mut shown = Vec::new();
        let mut gone = Vec::new();
        for (info, value) in options {
            let ((), _) = (self.options).change(|store, _| store.saved(info, value.clone()));
            match value {
                Some(value) => shown.push(options::show_value(info, &value)),
                None => gone.push(info.name),
            }
        }
        let mut said = Vec::new();
        if !shown.is_empty() {
            said.push(format!("saved {} in {file}", shown.join(" ")));
        }
        if !gone.is_empty() {
            said.push(format!("{} saved in {file} no more", gone.join(" and ")));
        }
        self.say_saved(said.join("; "));
    }

    /// Says what was saved, in the bar, and along the settings menu's foot
    /// if it's open.
    pub(super) fn say_saved(&mut self, said: String) {
        self.settings_changed();
        if let Some(menu) = &mut self.settings {
            menu.saved(&said, false);
        }
        self.say(said);
    }

    /// Says what wasn't saved, and why, as [`Desk::say_saved`] does.
    pub(super) fn warn_saved(&mut self, said: String) {
        self.settings_changed();
        if let Some(menu) = &mut self.settings {
            menu.saved(&said, true);
        }
        self.warn(said);
    }

    /// Passes the options on to the panes, as they've changed.
    pub(super) fn options_changed(&mut self) {
        self.stop_scrolling();
        self.editors.set_options(&self.options);
        self.explorer_options();
        self.board_options();
        self.review_options();
        for pane in self.panes.values_mut() {
            pane.set_options(&self.options);
        }
        for chat in self.chats.values_mut() {
            chat.set_options(&self.options);
        }
        // `notify` turned down takes back what it no longer tells of.
        self.notify(&[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The process of windows finds the fonts for its first window as it
    /// starts, and every window after starts from another's: whichever
    /// size or family they were found at, a window opens in its own, as
    /// `--font-size` and `--font` say.
    #[test]
    fn a_window_opens_in_its_own_font_whatever_the_fonts_were_found_for() {
        let mut desk = Desk::default();
        let wanted = crate::FontOptions {
            family: None,
            size: 17.0,
        };
        desk.start_options(options::Options::drawing_with(&wanted), Vec::new());
        let mut fonts = Fonts::load(&crate::FontOptions::default(), 1.0).expect("a font");
        assert!((fonts.points() - 12.0).abs() < f32::EPSILON);
        desk.fonts_loaded(fonts.mono_family.as_deref(), fonts.points());
        assert!(desk.use_font(&mut fonts));
        assert!((fonts.points() - 17.0).abs() < f32::EPSILON);
        // Found in the window's own font, there's nothing to change.
        desk.fonts_loaded(fonts.mono_family.as_deref(), fonts.points());
        assert!(!desk.use_font(&mut fonts));
    }
}
