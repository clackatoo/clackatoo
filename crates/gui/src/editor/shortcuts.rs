use super::{Editor, Prompt};
use crate::shortcuts::{
    Context,
    bindings::{self, Hint},
};
use crate::vim::Mode;

impl Editor {
    #[allow(clippy::too_many_lines, reason = "context-specific key descriptions")]
    pub(crate) fn shortcut_context(&self) -> Context {
        if self.rendered() {
            return super::rendered::shortcuts(&self.keys);
        }
        if let Some(prompt) = &self.prompt {
            return match prompt {
                Prompt::Confirm(_) => bindings::list("Confirm substitution", None, |stroke, _| {
                    self.describe_confirm_stroke(stroke)
                }),
                Prompt::Search { line, .. } => {
                    crate::shortcuts::prompt("Editor search", line, true)
                }
                Prompt::Rename { name, .. } => {
                    crate::shortcuts::prompt("Rename symbol", name, false)
                }
            };
        }
        let mut context = if self.mode == Mode::Insert {
            bindings::list("Editor · insert mode", None, |stroke, _| {
                let completion = if self.replaying {
                    Hint::None
                } else {
                    self.describe_completion_stroke(stroke)
                };
                if completion != Hint::None {
                    return completion;
                }
                let snippet = self.describe_snippet_jump(stroke);
                if snippet != Hint::None {
                    return snippet;
                }
                self.describe_insert_bound(stroke)
            })
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
        if !self.keys.pending() {
            for stroke in bindings::strokes() {
                let hint = if self.actions.is_some() && self.mode == Mode::Normal && !self.replaying
                {
                    self.describe_action_menu_key(stroke)
                } else {
                    Hint::None
                };
                let hint = if hint == Hint::None && self.panel.is_some() {
                    self.describe_panel_stroke(stroke)
                } else {
                    hint
                };
                if hint != Hint::None {
                    let key = bindings::label(stroke);
                    for (keys, _) in &mut context.rows {
                        *keys = keys
                            .split(" / ")
                            .filter(|old| *old != key)
                            .collect::<Vec<_>>()
                            .join(" / ");
                    }
                    context.rows.retain(|(keys, _)| !keys.is_empty());
                    bindings::add(&mut context, key, hint);
                }
            }
        }
        crate::shortcuts::bindings::normal(&mut context, "Enter normal mode");
        crate::shortcuts::bindings::paste(&mut context, "Paste clipboard text");
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
        let dialect = EditorKeys { tildeop: false };
        let visual = matches!(mode, Mode::Visual(_));
        for stroke in to_strokes(pending) {
            keys.read(stroke, visual, &dialect);
        }
        context("Editor", &keys, mode, &dialect)
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
        assert!(has(&help("\"", Mode::Normal), "Register name"));
        assert!(has(&help("3", Mode::Normal), "w"));
        assert!(!has(&help("", Mode::Normal), "iw"));
        assert!(has(
            &help("", Mode::Visual(crate::vim::Selection::Characters)),
            "iw"
        ));
    }
}
