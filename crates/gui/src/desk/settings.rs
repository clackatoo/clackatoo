//! The desk's options: setting them, from `:set` and config, saving them
//! with `:setpersist`, and passing them on to the panes they change.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use super::Desk;
use crate::fonts::Fonts;
use crate::lua::ToLua;
use crate::options::persist::{self, Saved};
use crate::options::{self, Decimal, Info, SetBy, Setting, Store, Value};

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
    /// Waits for the next setting to be saved, for tests.
    pub(super) fn wait(&self) -> Saved {
        (self.answers)
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("saved")
    }
}

impl Desk {
    /// Starts from `options` rather than the defaults, as the command line
    /// and startup.toml set them, before config runs. Config leaves those
    /// in `fixed` alone, which the command line set.
    pub(crate) fn start_options(&mut self, options: options::Options, fixed: Vec<&'static Info>) {
        self.font_in_use = FontInUse::of(&options);
        self.options = Store::new(options, fixed);
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

    /// Sets `info`'s option to `value` for config, `by` its line, unless
    /// the config is loading and the command line set it: what's set where
    /// the window starts outdoes the config file, which may be shared
    /// between machines. Once it's loaded, the config's handlers set
    /// options as `:set` does, as you asked them to, now.
    pub(super) fn config_sets(&mut self, info: &'static Info, value: Value, by: SetBy) {
        if by.loading && self.options.fixed(info) {
            log::info!(
                "config sets {}, but the command line set it, so it stays {}",
                info.name,
                self.options.show(info)
            );
            // Lua's copy has the value it set: put it right.
            self.tell_lua(ToLua::Option {
                option: info,
                value: self.options.get(info),
            });
            return;
        }
        self.set_option(info, value, by);
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
                self.set_option(font, Value::Text(family.clone()), SetBy::RUNTIME);
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

    /// Sets `info`'s option to `value`, which it can hold, noting `by` as
    /// what set it, and telling Lua and the panes if that changes it.
    pub(super) fn set_option(&mut self, info: &'static Info, value: Value, by: SetBy) {
        if let Ok(true) = self.options.set(info, value, by) {
            self.options_changed();
        }
        // Lua hears every change, even its own, so its copy ends up as the
        // window's whatever order changes cross in.
        self.tell_lua(ToLua::Option {
            option: info,
            value: self.options.get(info),
        });
    }

    /// Carries out `:set` with `words` after it, saving what it sets where
    /// it's kept too if `persist`, as `:setpersist` does, and saying what
    /// set each option it shows if `verbose`, as `:verbose set` does.
    pub(super) fn run_set(&mut self, words: &str, persist: bool, verbose: bool) {
        let settings = match options::parse_set(words) {
            Ok(settings) => settings,
            Err(err) => {
                self.warn(err);
                return;
            }
        };
        if settings.is_empty() {
            // As in Vim: the options not as they started.
            let start = self.options.start();
            let changed: Vec<String> = (options::ALL.iter())
                .filter(|info| self.options.get(info) != start.get(info))
                .map(|info| self.option_shown(info, verbose))
                .collect();
            self.say(if changed.is_empty() {
                "every option is as it started".into()
            } else {
                changed.join("  ")
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
            };
            if info.restart && persist {
                // Saved for the next window, as it's read only then.
                self.persist(info, value);
                continue;
            }
            if let Err(why) = info.settable_now() {
                self.warn(why);
                continue;
            }
            let now = value.clone().unwrap_or_else(|| info.default_value());
            self.set_option(info, now, by.clone());
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
        let said = self.options.set_by(info).said(self.config_dir());
        format!("{shown} · {said}")
    }

    /// Saves `info`'s option as `value`, as `:setpersist` does, unless the
    /// user's own config set it as it loaded, which would outdo what's
    /// saved: then it's set for now only, and the bar says where config
    /// sets it.
    fn persist_unless_configured(&mut self, info: &'static Info, value: Option<Value>) {
        let configured = (self.options.configured(info))
            .filter(|(_, by)| by.config_loading())
            .map(|(_, by)| by.place(self.config_dir()));
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
    fn persist(&mut self, info: &'static Info, value: Option<Value>) {
        let Some(start) = &self.script.start else {
            return self.warn("there's no config directory to save settings in".into());
        };
        let Some(dir) = start.source.dir() else {
            return self.warn("there's no config directory to save settings in".into());
        };
        persist::save(dir, info, value, self.saving.reply.clone(), (start.wake)());
    }

    /// clackatoo's config directory, if the window has one.
    pub(super) fn config_dir(&self) -> Option<&Path> {
        self.script.start.as_ref()?.source.dir()
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
            info,
            value,
            file,
            result,
        } = saved;
        if let Err(why) = result {
            return self.warn(format!("{} wasn't saved: {why}", info.name));
        }
        let later = if info.restart {
            ", for windows opened after"
        } else {
            ""
        };
        self.say(match value {
            Some(value) => format!(
                "saved {} in {file}{later}",
                options::show_value(info, &value)
            ),
            None => format!("{} is saved in {file} no more{later}", info.name),
        });
    }

    /// Passes the options on to the panes, as they've changed.
    fn options_changed(&mut self) {
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

    /// Puts the options back as they start, as a config run again from
    /// the start does, forgetting what set them.
    pub(super) fn reset_options(&mut self) {
        self.options.reset();
        self.options_changed();
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
