//! Keys collected in the same precedence order as `Desk::key_with`.
use super::Desk;
use super::bindings::{Action, Binding};
use crate::keymap::parse_keys;
use crate::shortcuts::Context;

impl Desk {
    #[allow(clippy::too_many_lines, reason = "context-specific key descriptions")]
    pub(crate) fn shortcut_context(&self) -> Context {
        if self.offer_asked() {
            return crate::shortcuts::bindings::from_presses(
                "Install language features",
                |press| self.describe_answer_offer(press),
            );
        }
        if let Some(input) = &self.input {
            let mut context = input.shortcut_context();
            crate::shortcuts::bindings::normal(&mut context, "Cancel prompt");
            crate::shortcuts::bindings::paste(&mut context, "Paste clipboard text");
            return context;
        }
        if let Some(page) = &self.start {
            let mut context = page.shortcut_context();
            crate::shortcuts::bindings::normal(&mut context, "Clear query, or close the page");
            crate::shortcuts::bindings::paste(&mut context, "Paste into query");
            return context;
        }
        if let Some(finder) = self.finder.as_ref().filter(|_| self.finding()) {
            let mut context = finder.shortcut_context();
            crate::shortcuts::bindings::normal(&mut context, "Close picker");
            crate::shortcuts::bindings::paste(&mut context, "Paste into query");
            return context;
        }
        let chat = self.focused_chat();
        if let Some(context) = chat.and_then(crate::chat::ChatPane::exclusive_shortcuts) {
            return context;
        }
        if !self.keymap.waiting().is_empty() {
            let mut context = Context::new(
                format!(
                    "After {}",
                    crate::shortcuts::sequence(self.keymap.waiting())
                ),
                &[("Esc", "Cancel the pending binding")],
            );
            self.mapped_shortcuts(&mut context);
            return context;
        }
        let mut context = if let Some((editor, _)) = self.focused_editor() {
            editor.shortcut_context()
        } else if let Some(explorer) = self.focused_explorer() {
            explorer.shortcut_context()
        } else if let Some(board) = self.focused_board() {
            board.shortcut_context()
        } else if let Some(review) = self.focused_review() {
            review.shortcut_context()
        } else if let Some(browser) = self.focused_browser() {
            browser.shortcut_context()
        } else if let Some(chat) = chat {
            chat.shortcut_context()
        } else if let Some(pane) = self.focused_terminal().and_then(|id| self.pane(id)) {
            pane.shortcut_context()
        } else {
            Context::new("Window", &[])
        };
        self.lesson_shortcuts(&mut context);
        self.listing_shortcuts(&mut context);
        if self.key_mode() == crate::keymap::KeyMode::Normal && !self.pane_pending() {
            self.resume_shortcuts(&mut context);
        }
        if self.recording().is_some() && self.key_mode() == crate::keymap::KeyMode::Normal {
            context.rows.extend(
                crate::shortcuts::bindings::list("", None, |stroke, _| {
                    if crate::vim::stops_recording(stroke) {
                        crate::shortcuts::bindings::Hint::Action("Stop recording macro")
                    } else {
                        crate::shortcuts::bindings::Hint::None
                    }
                })
                .rows,
            );
        }
        if !self.pane_pending() || self.pane_counting().is_some() {
            // Built-in rows use alternatives separated by ` / `. Hide each
            // alternative intercepted by the live Lua-configurable keymap.
            context.rows = context
                .rows
                .into_iter()
                .filter_map(|(keys, description)| {
                    let keys = keys
                        .split(" / ")
                        .filter(|key| {
                            let notation = crate::shortcuts::notation(key);
                            !parse_keys(&notation)
                                .is_ok_and(|keys| self.keymap.intercepts(self.key_mode(), &keys))
                        })
                        .collect::<Vec<_>>()
                        .join(" / ");
                    (!keys.is_empty()).then_some((keys, description))
                })
                .collect();
            self.mapped_shortcuts(&mut context);
        }
        context
    }

    fn mapped_shortcuts(&self, context: &mut Context) {
        for (keys, binding, label) in self.keymap.available(self.key_mode()) {
            if keys
                .iter()
                .any(|key| crate::shortcuts::requested(&key.kept().press()))
            {
                continue;
            }
            if matches!(
                binding,
                Binding::Window(
                    Action::Hunk(_) | Action::ToggleInlayHints | Action::ToggleMarkdown
                )
            ) && self.focused_editor().is_none()
            {
                continue;
            }
            let description = label.map_or_else(
                || match binding {
                    Binding::Keys(keys) => format!("Type {}", crate::shortcuts::sequence(keys)),
                    Binding::Lua(_) => "Custom Lua action".into(),
                    Binding::Window(_) => "Window action".into(),
                },
                str::to_owned,
            );
            context
                .rows
                .push((crate::shortcuts::sequence(keys), description));
        }
    }
}
