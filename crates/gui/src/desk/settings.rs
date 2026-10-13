//! The desk's options: setting them, from `:set` and config, and passing
//! them on to the panes they change.

use super::Desk;
use crate::lua::ToLua;
use crate::options::{self, Info, Setting, Value};

impl Desk {
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
                self.message = Some(err);
                return;
            }
        };
        if settings.is_empty() {
            // As in Vim: the options not as they started.
            let start = options::Options::default();
            let changed: Vec<String> = (options::ALL.iter())
                .filter(|info| self.options.get(info) != start.get(info))
                .map(|info| self.options.show(info))
                .collect();
            self.message = Some(if changed.is_empty() {
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
            self.message = Some(shown.join("  "));
        }
    }

    /// Passes the options on to the panes, as they've changed.
    fn options_changed(&mut self) {
        self.editors.set_options(&self.options);
        self.explorer_options();
        for pane in self.panes.values_mut() {
            pane.set_options(&self.options);
        }
    }

    /// Puts the options back as they start, as a config run again from
    /// the start does.
    pub(super) fn reset_options(&mut self) {
        self.options = options::Options::default();
        self.options_changed();
    }
}
