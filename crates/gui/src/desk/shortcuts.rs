//! Keys collected in the same precedence order as `Desk::key_with`.
use super::bindings::{Action, Binding};
use super::{Desk, Purpose};
use crate::keymap::parse_keys;
use crate::shortcuts::{Context, FINDER, PROMPT};

impl Desk {
    #[allow(clippy::too_many_lines, reason = "context-specific key descriptions")]
    pub(crate) fn shortcut_context(&self) -> Context {
        if self.offer_asked() {
            return Context::new(
                "Install language features",
                &[
                    ("y / Y / Enter", "Install the offered language features"),
                    ("n / N", "Decline future offers for this language"),
                    (
                        "Any other key",
                        "Dismiss the offer and use the key in the editor",
                    ),
                ],
            );
        }
        if let Some(input) = &self.input {
            if input.line.wants_register() {
                return Context::new(
                    "Prompt · insert register",
                    &[
                        (
                            "a–z / A–Z / 0–9 / \" / - / _ / + / *",
                            "Insert the register's contents",
                        ),
                        ("Esc / Ctrl+;", "Cancel the prompt"),
                    ],
                );
            }
            let mut context = Context::new(
                input
                    .purpose
                    .label()
                    .0
                    .unwrap_or_else(|| "Command prompt".into()),
                PROMPT,
            );
            if input.purpose == Purpose::Command {
                context.add(&[
                    ("Tab / Shift+Tab", "Complete forward / backward"),
                    ("Down / Ctrl+N", "Next suggestion or history entry"),
                    ("Up / Ctrl+P", "Previous suggestion or history entry"),
                ]);
            }
            if input.purpose == Purpose::Workspace {
                context.add(&[
                    ("Down / Tab", "Next workspace"),
                    ("Up / Shift+Tab", "Previous workspace"),
                ]);
            }
            return context;
        }
        if self.finding() {
            let mut context = Context::new("Picker", FINDER);
            context.add(&[
                ("Ctrl+V", "Open in a split to the right"),
                ("Ctrl+X", "Open in a split below"),
                ("Ctrl+T", "Open in a new tab"),
            ]);
            if self
                .finder
                .as_ref()
                .is_some_and(|finder| finder.kind() == crate::finder::Kind::Background)
            {
                context.add(&[
                    ("Ctrl+D", "Dismiss the background tab"),
                    ("Ctrl+F", "Forward its ports"),
                ]);
            }
            return context;
        }
        let pane = self.focused_terminal().and_then(|id| self.pane(id));
        if let Some(context) = pane.and_then(crate::pane::Pane::exclusive_shortcuts) {
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
        } else if self.focused_browser().is_some() {
            Context::new(
                "Browser pane",
                &[("q", "Close the pane, keeping the browser session")],
            )
        } else if let Some(pane) = pane {
            pane.shortcut_context()
        } else {
            Context::new("Window", &[])
        };
        self.lesson_shortcuts(&mut context);
        if self.key_mode() == crate::keymap::KeyMode::Normal && !self.pane_pending() {
            if let Some(shown) = self.shown
                && self.focused_editor().is_some()
                && self.file_in(shown.pane).is_some()
                && !self.is_lesson_pane(shown.pane)
            {
                context.rows.retain(|(key, _)| key != "-");
                context.add(&[("-", "Open parent directory")]);
                if self.is_explorer(shown.pane) {
                    context.rows.retain(|(key, _)| key != "Enter");
                    context.add(&[
                        ("Enter", "Open the entry at cursor"),
                        ("g.", "Toggle hidden directory entries"),
                    ]);
                }
            }
            self.resume_shortcuts(&mut context);
        }
        if self.recording().is_some() && self.key_mode() == crate::keymap::KeyMode::Normal {
            context.add(&[("q", "Stop recording the macro")]);
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
