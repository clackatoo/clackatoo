//! The desk's options: setting them, from `:set` and config, and passing
//! them on to the panes they change.

use std::time::Instant;

use super::Desk;
use crate::fonts::Fonts;
use crate::lua::ToLua;
use crate::options::{self, Decimal, Info, Setting, Value};

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

impl Desk {
    /// Starts from `options` rather than the defaults, as the command line
    /// and startup.toml set them, before config runs. Config leaves those
    /// in `fixed` alone, which the command line set.
    pub(crate) fn start_options(&mut self, options: options::Options, fixed: Vec<&'static Info>) {
        self.font_in_use = FontInUse::of(&options);
        self.start_options = options.clone();
        self.options = options;
        self.fixed = fixed;
        self.options_changed();
    }

    /// Sets `info`'s option to `value` for config, unless `init.lua` is
    /// running and the command line set it: what's set where the window
    /// starts outdoes the config file, which may be shared between
    /// machines. Once it's run, the config's handlers set options as `:set`
    /// does, as you asked them to, now.
    pub(super) fn config_sets(&mut self, info: &'static Info, value: Value) {
        if self.config_running(Instant::now()) && self.fixed.contains(&info) {
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
        self.set_option(info, value);
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
                self.set_option(font, Value::Text(family.clone()));
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

    /// Sets `info`'s option to `value`, which it can hold, telling Lua
    /// and the panes if that changes it.
    pub(super) fn set_option(&mut self, info: &'static Info, value: Value) {
        if let Ok(true) = self.options.set(info, value) {
            self.options_changed();
        }
        // Lua hears every change, even its own, so its copy ends up as the
        // window's whatever order changes cross in.
        self.tell_lua(ToLua::Option {
            option: info,
            value: self.options.get(info),
        });
    }

    /// Carries out `:set` with `words` after it.
    pub(super) fn run_set(&mut self, words: &str) {
        let settings = match options::parse_set(words) {
            Ok(settings) => settings,
            Err(err) => {
                self.warn(err);
                return;
            }
        };
        if settings.is_empty() {
            // As in Vim: the options not as they started.
            let start = &self.start_options;
            let changed: Vec<String> = (options::ALL.iter())
                .filter(|info| self.options.get(info) != start.get(info))
                .map(|info| self.options.show(info))
                .collect();
            self.say(if changed.is_empty() {
                "every option is as it started".into()
            } else {
                changed.join("  ")
            });
            return;
        }
        let mut shown = Vec::new();
        for setting in settings {
            match setting {
                Setting::To(info, value) => self.set_option(info, value),
                Setting::Flip(info) => {
                    if let Value::Bool(on) = self.options.get(info) {
                        self.set_option(info, Value::Bool(!on));
                    }
                }
                Setting::Show(info) => shown.push(self.options.show(info)),
            }
        }
        if !shown.is_empty() {
            self.say(shown.join("  "));
        }
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
    }

    /// Puts the options back as they start, as a config run again from
    /// the start does.
    pub(super) fn reset_options(&mut self) {
        self.options = self.start_options.clone();
        self.options_changed();
    }
}
