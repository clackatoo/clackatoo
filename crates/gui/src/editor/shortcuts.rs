use super::{Editor, Prompt};
use crate::shortcuts::Context;
use crate::vim::Mode;

impl Editor {
    #[allow(clippy::too_many_lines, reason = "context-specific key descriptions")]
    pub(crate) fn shortcut_context(&self) -> Context {
        if self.rendered() {
            let mut context = Context::new(
                "Rendered Markdown",
                &[
                    ("j / Down / Ctrl+E", "Scroll down"),
                    ("k / Up / Ctrl+Y", "Scroll up"),
                    ("Ctrl+D / Ctrl+U", "Scroll half a page"),
                    ("PageDown / Ctrl+F / Shift+Down", "Scroll a page down"),
                    ("PageUp / Shift+Up", "Scroll a page up"),
                    ("gg / G", "Start / end of document"),
                    ("gj / gk", "Scroll down / up"),
                    (
                        "Other editor keys",
                        "Show the source and apply the key there",
                    ),
                ],
            );
            if self.keys.pending() {
                context.title = format!(
                    "Rendered Markdown · after {}",
                    self.keys.shown().unwrap_or_default()
                );
            }
            return context;
        }
        if let Some(prompt) = &self.prompt {
            return if matches!(prompt, Prompt::Confirm(_)) {
                Context::new(
                    "Confirm substitution",
                    &[
                        ("y", "Replace this match"),
                        ("n", "Skip this match"),
                        ("a", "Replace all remaining matches"),
                        ("l", "Replace this match and stop"),
                        ("q / Esc / Ctrl+;", "Stop replacing"),
                    ],
                )
            } else {
                match prompt {
                    Prompt::Search { line, .. } => {
                        crate::shortcuts::prompt("Editor search", line, true)
                    }
                    Prompt::Rename { name, .. } => {
                        crate::shortcuts::prompt("Rename symbol", name, false)
                    }
                    Prompt::Confirm(_) => unreachable!(),
                }
            };
        }
        let mut context = if self.mode == Mode::Insert {
            Context::new(
                "Editor · insert mode",
                &[
                    ("Esc / Ctrl+;", "Enter normal mode"),
                    ("Enter", "New line"),
                    (
                        "Tab / Shift+Tab",
                        "Insert indentation, or move through snippet fields",
                    ),
                    ("Left / Right / Up / Down", "Move caret"),
                    ("Home / End", "Start / end of line"),
                    ("Ctrl+Home / Ctrl+End", "Start / end of file"),
                    (
                        "Ctrl+Left / Ctrl+Right / Shift+Left / Shift+Right",
                        "Move by words",
                    ),
                    ("PageUp / PageDown / Shift+Up / Shift+Down", "Move by pages"),
                    ("Backspace / Ctrl+H", "Delete previous character"),
                    ("Delete", "Delete next character"),
                    ("Ctrl+W", "Delete previous word"),
                    ("Ctrl+U", "Delete to start of inserted text or indentation"),
                    ("Ctrl+Space / Ctrl+N / Ctrl+P", "Request completions"),
                    ("Ctrl+Shift+V", "Paste clipboard text"),
                ],
            )
        } else {
            crate::vim::shortcuts::context(
                "Editor",
                &self.keys,
                self.mode,
                &super::keys::EditorKeys {
                    tildeop: self.tildeop,
                },
            )
        };
        if self.actions.is_some() && self.mode == Mode::Normal && !self.keys.pending() {
            context
                .rows
                .retain(|(key, _)| !matches!(key.as_str(), "j" | "k" | "Up" | "Down"));
            context.add(&[
                ("j / Down / Tab / Ctrl+N", "Next code action"),
                ("k / Up / Ctrl+P", "Previous code action"),
                ("Enter", "Apply code action"),
                ("1–9", "Apply numbered code action"),
                ("q / Esc / Ctrl+C", "Close code actions"),
            ]);
        }
        if self.panel.is_some() && !self.keys.pending() {
            context
                .rows
                .retain(|(key, _)| !matches!(key.as_str(), "K" | "Ctrl+E" | "Ctrl+Y"));
            context.add(&[
                ("K", "Next page of hover information"),
                ("Ctrl+E / Ctrl+Y", "Scroll hover information"),
                ("Esc", "Close hover information"),
            ]);
        }
        if self.completion.is_some() && self.mode == Mode::Insert {
            context
                .rows
                .retain(|(key, _)| key != "Ctrl+Space / Ctrl+N / Ctrl+P");
            context.add(&[("Ctrl+Space", "Request completions")]);
            if self.as_you_type.tab {
                context.add(&[("Tab", "Accept completion, except snippet choices")]);
            }
            match self.as_you_type.accept {
                crate::options::CompleteAccept::Enter => {
                    context.add(&[("Enter", "Accept completion")]);
                }
                crate::options::CompleteAccept::Picked => {
                    context.add(&[("Enter", "Accept a completion after moving through the menu")]);
                }
                crate::options::CompleteAccept::CtrlY => {}
            }
            context.add(&[
                ("Down / Ctrl+N", "Next completion"),
                ("Up / Ctrl+P", "Previous completion"),
                ("Ctrl+Y", "Accept completion"),
                ("Ctrl+E", "Close completions"),
            ]);
        }
        context
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::keys::EditorKeys;
    use crate::vim::shortcuts::context;
    use crate::vim::{Keys, Mode};

    fn to_strokes(text: &str) -> Vec<crate::vim::Stroke> {
        crate::keymap::read_macro(text)
            .iter()
            .flat_map(|chord| crate::vim::strokes(&chord.kept().press(), false))
            .collect()
    }

    fn help(pending: &str, mode: Mode) -> Context {
        let mut keys = Keys::default();
        for stroke in to_strokes(pending) {
            keys.push(stroke);
        }
        context("Editor", &keys, mode, &EditorKeys { tildeop: false })
    }
    fn has(context: &Context, key: &str) -> bool {
        context.rows.iter().any(|(label, _)| label == key)
    }

    #[test]
    fn shortcut_help_reads_pending_grammar_without_completing_or_cancelling_it() {
        let after_g = help("g", Mode::Normal);
        assert!(has(&after_g, "d"));
        assert!(has(&after_g, "rn"));
        assert!(!has(&after_g, "x"));
        assert!(!has(&after_g, "h"));
        let after_delete = help("d", Mode::Normal);
        assert!(has(&after_delete, "w"));
        assert!(has(&after_delete, "iw"));
        assert!(!has(&after_delete, "p"));
        let inner = help("di", Mode::Normal);
        assert!(
            inner
                .rows
                .iter()
                .any(|(key, desc)| key == "w" && desc == "Inner word")
        );
        assert!(!has(&inner, "j"));
        assert!(has(&help("f", Mode::Normal), "Any character"));
        assert!(has(
            &help("\"", Mode::Normal),
            "a–z / A–Z / 0–9 / \" / - / _ / + / *"
        ));
        assert!(has(&help("3", Mode::Normal), "w"));
        assert!(!has(&help("", Mode::Normal), "iw"));
        assert!(has(
            &help("", Mode::Visual(crate::vim::Selection::Characters)),
            "iw"
        ));
    }
}
