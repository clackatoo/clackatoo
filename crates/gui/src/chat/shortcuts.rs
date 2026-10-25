use super::{ChatView, read::Prefix};
use crate::shortcuts::Context;
use crate::vim::Mode;

impl ChatView {
    pub(crate) fn exclusive_shortcuts(&self) -> Option<Context> {
        if self.viewing_modal() {
            let mut context = Context::new(
                if self.usage_modal {
                    "Usage limits"
                } else {
                    "Image viewer"
                },
                &[("Esc / Ctrl+;", "Close the modal")],
            );
            if self.usage_modal {
                context.add(&[
                    ("Up / Down", "Scroll limits"),
                    ("PageUp / PageDown", "Scroll a page"),
                ]);
            }
            return Some(context);
        }
        self.menu.as_ref().map(|_| {
            let mut context = Context::new(
                if self.searching_models() {
                    "Model picker"
                } else {
                    "Agent settings"
                },
                &[
                    ("Up / Down", "Previous / next choice"),
                    ("Left / Right", "Adjust value or expand model family"),
                    ("Enter", "Choose the highlighted item"),
                    ("Esc / Ctrl+;", "Close the popup"),
                ],
            );
            if self.searching_models() {
                context.add(&[
                    ("Type", "Search all models, with matching favourites first"),
                    ("Backspace", "Delete the last search character"),
                    ("Ctrl+Shift+V", "Paste into model search"),
                ]);
            }
            context
        })
    }

    #[allow(clippy::too_many_lines, reason = "context-specific key descriptions")]
    pub(crate) fn shortcut_context(&self, mode: Mode) -> Context {
        let mut context = if mode == Mode::Insert {
            if self.node.question.is_some() {
                Context::new(
                    "Agent question",
                    &[
                        ("Esc", "Decline the request"),
                        ("Ctrl+;", "Enter normal mode"),
                    ],
                )
            } else {
                let mut context = Context::new(
                    "Agent composer",
                    &[
                        (
                            "Enter",
                            if self.busy() && self.queue_supported {
                                "Queue the message"
                            } else {
                                "Send the message"
                            },
                        ),
                        ("Shift+Enter", "New line"),
                        ("Shift+Tab", "Cycle agent modes"),
                        ("Esc / Ctrl+;", "Enter normal mode"),
                        ("Left / Right / Up / Down", "Move the caret"),
                        ("Ctrl+Left / Ctrl+Right", "Move by words"),
                        ("Home / End", "Start / end of line"),
                        ("Shift + movement", "Extend the selection"),
                        ("Ctrl+A", "Select the draft"),
                        ("Ctrl+C", "Copy selection; otherwise stop the working agent"),
                        (
                            "Backspace / Delete",
                            "Delete text; Backspace removes the last image when the draft is empty",
                        ),
                        ("Ctrl+Backspace / Ctrl+Delete", "Delete a word"),
                        ("Ctrl+Shift+V", "Paste text or attach an image"),
                    ],
                );
                if self.busy() && self.queue_supported {
                    context.add(&[("Ctrl+Enter", "Steer the current turn")]);
                }
                context
            }
        } else {
            let mut context = Context::new(
                "Agent conversation",
                &[
                    (
                        "h / j / k / l / Left / Down / Up / Right",
                        "Move through conversation text",
                    ),
                    ("w / b / e", "Move by words"),
                    ("0 / ^ / $", "Line start / first word / end"),
                    ("{ / }", "Previous / next block"),
                    ("gg / G", "Start / end of conversation"),
                    ("Ctrl+D / Ctrl+U", "Scroll half a view"),
                    ("Ctrl+E / Ctrl+Y", "Scroll one line"),
                    ("v / V", "Select characters / lines"),
                    ("y", "Copy selection or the code, message or card at cursor"),
                    (
                        "za / zo / zc / Enter",
                        "Toggle / open / fold work at cursor",
                    ),
                    ("zR / zM", "Expand / fold all work"),
                    (
                        "zl / zh / zL / zH",
                        "Scroll table horizontally, a little / half a view",
                    ),
                    ("u / Ctrl+R", "Undo / redo the turn's file changes"),
                    ("K", "Open context and usage limits"),
                    ("i / a / I / A", "Compose a message"),
                    ("Esc / Ctrl+;", "Clear selection and pending command"),
                ],
            );
            if mode == Mode::Normal {
                context.add(&[("o", "Compose a message")]);
            } else {
                context.rows.retain(|(key, _)| key != "K");
                context.add(&[("o", "Move to other end of selection")]);
            }
            context.add(&[
                ("PageDown / PageUp", "Scroll half a view"),
                ("Ctrl+Shift+C", "Copy conversation selection"),
                ("Ctrl+C", "Stop the working agent, or copy selection"),
            ]);
            if let Some(prefix) = self.read_prefix {
                context.title = format!(
                    "Agent conversation · after {}",
                    if prefix == Prefix::G { "g" } else { "z" }
                );
                if prefix == Prefix::Z {
                    context.rows = context
                        .rows
                        .into_iter()
                        .filter_map(|(key, description)| {
                            let key = key
                                .split(" / ")
                                .filter_map(|key| {
                                    if key.starts_with("Ctrl+") {
                                        Some(key)
                                    } else {
                                        key.strip_prefix('z')
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" / ");
                            (!key.is_empty()).then_some((key, description))
                        })
                        .collect();
                    context.add(&[("Esc / Ctrl+;", "Cancel the fold command")]);
                } else {
                    context.rows.retain(|(key, _)| key != "gg / G");
                    context.add(&[("g", "Start of conversation"), ("G", "End of conversation")]);
                }
            }
            context
        };
        if self.read_prefix.is_none()
            && let Some(question) = &self.node.question
        {
            for (index, choice) in question.choices.iter().take(9).enumerate() {
                context.rows.push((
                    (index + 1).to_string(),
                    if mode == Mode::Insert {
                        choice.name.clone()
                    } else {
                        format!("Select {}; press again or Enter to confirm", choice.name)
                    },
                ));
            }
            if self.armed.is_some() {
                context.add(&[("Enter", "Confirm the selected answer")]);
            }
        }
        context
    }
}
