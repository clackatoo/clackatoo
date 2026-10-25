//! A read-only snapshot of the keys available where help was opened.
//! The window owns this overlay, so opening it never types into or dismisses
//! the prompt, popup, selection or unfinished command underneath it.

use winit::keyboard::{Key, NamedKey};

use crate::cmdline::Line;
use crate::finder::view::{FinderView, fit};
use crate::input::KeyPress;
use crate::render::Ink;

pub(crate) type Rows = Vec<(String, String)>;

#[derive(Clone, Debug, Default)]
pub(crate) struct Context {
    pub(crate) title: String,
    pub(crate) rows: Rows,
}

impl Context {
    pub(crate) fn new(title: impl Into<String>, rows: &[(&str, &str)]) -> Self {
        let mut context = Self {
            title: title.into(),
            rows: Vec::new(),
        };
        context.add(rows);
        context
    }

    pub(crate) fn add(&mut self, rows: &[(&str, &str)]) {
        self.rows.extend(
            rows.iter()
                .map(|(key, description)| ((*key).into(), (*description).into())),
        );
    }
}

/// Ctrl+? uses the layout's logical question mark, including Shift where
/// the layout requires it. Ctrl+/ remains available to the terminal.
pub(crate) fn requested(press: &KeyPress<'_>) -> bool {
    press.modifiers.ctrl
        && !press.modifiers.alt
        && matches!(press.key, Key::Character(text) if text == "?")
}

#[derive(Debug)]
pub(crate) struct Help {
    pub(crate) context: Context,
    query: Line,
    first: usize,
    page: usize,
}

impl Help {
    pub(crate) fn new(mut context: Context) -> Self {
        context.add(&[("Ctrl+?", "Show shortcuts for this state")]);
        Self {
            context,
            query: Line::default(),
            first: 0,
            page: 10,
        }
    }

    /// True closes help. Every other key belongs to help, not the UI below.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> bool {
        if requested(press)
            || crate::vim::enters_normal_mode(press)
            || *press.key == Key::Named(NamedKey::Escape)
        {
            return true;
        }
        let ctrl = match press.key {
            Key::Character(text) if press.modifiers.ctrl && !press.modifiers.alt => {
                text.chars().next()
            }
            _ => None,
        };
        match (press.key, ctrl) {
            (Key::Named(NamedKey::ArrowDown), _) | (_, Some('n')) => self.scroll(1),
            (Key::Named(NamedKey::ArrowUp), _) | (_, Some('p')) => self.scroll(-1),
            (Key::Named(NamedKey::PageDown), _) => {
                self.scroll(isize::try_from(self.page).unwrap_or(isize::MAX));
            }
            (Key::Named(NamedKey::PageUp), _) => {
                self.scroll(-(isize::try_from(self.page).unwrap_or(isize::MAX)));
            }
            _ => {
                let before = self.query.text().to_owned();
                self.query.key(press);
                // Help has no register-paste state: never borrow a register
                // from, or dispatch a key into, the underlying pane.
                self.query.forget_register();
                if self.query.text() != before {
                    self.first = 0;
                }
            }
        }
        false
    }

    pub(crate) fn paste(&mut self, text: &str) {
        self.query.put(text);
        self.first = 0;
    }

    pub(crate) fn scroll(&mut self, by: isize) {
        self.first = self.first.saturating_add_signed(by);
    }

    pub(crate) fn view(&mut self, rows: usize, cols: usize) -> FinderView {
        let words: Vec<_> = self
            .query
            .text()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let matches: Vec<_> = self
            .context
            .rows
            .iter()
            .filter(|(key, description)| {
                let text = format!("{key} {description}").to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .collect();
        // Two lines per binding keep long key sequences readable on small windows.
        self.page = (rows.saturating_sub(3) / 2).max(1);
        self.first = self.first.min(matches.len().saturating_sub(self.page));
        let mut shown = Vec::new();
        for text in [
            &self.context.title,
            "Type to filter · ↑↓ scroll · Esc / Ctrl+; closes",
            "",
        ] {
            shown.push(fit(
                text,
                &vec![Ink::Muted; text.chars().count()],
                cols,
                false,
            ));
        }
        for (key, description) in matches.iter().skip(self.first).take(self.page) {
            shown.push(fit(key, &vec![Ink::Key; key.chars().count()], cols, false));
            shown.push(fit(
                description,
                &vec![Ink::Muted; description.chars().count()],
                cols,
                false,
            ));
        }
        shown.truncate(rows);
        let query = self.query.shown();
        FinderView {
            title: "Keyboard shortcuts",
            place: None,
            query: fit(
                &query,
                &vec![Ink::Plain; query.chars().count()],
                cols.saturating_sub(24).max(1),
                true,
            )
            .text,
            count: format!(
                "{}–{} / {}",
                if matches.is_empty() {
                    0
                } else {
                    self.first + 1
                },
                (self.first + self.page).min(matches.len()),
                matches.len()
            ),
            rows: shown,
            selected: None,
            preview: None,
            empty: matches.is_empty().then(|| "No matching shortcuts".into()),
        }
    }
}

pub(crate) const PROMPT: &[(&str, &str)] = &[
    ("Enter", "Accept the text"),
    ("Esc / Ctrl+;", "Cancel the prompt"),
    ("Left / Right", "Move the caret"),
    ("Home / End", "Start / end of the line"),
    ("Ctrl+Left / Ctrl+Right", "Move by words"),
    ("Shift+Left / Shift+Right", "Move by words"),
    ("Backspace / Delete", "Delete before / under the caret"),
    ("Ctrl+W / Ctrl+Backspace", "Delete the previous word"),
    ("Ctrl+U", "Delete before the caret"),
    ("Ctrl+R, register", "Insert a register"),
    ("Ctrl+Shift+V", "Paste clipboard text"),
];

pub(crate) const FINDER: &[(&str, &str)] = &[
    ("Enter", "Open the selected item"),
    ("Esc / Ctrl+C / Ctrl+;", "Close the picker"),
    ("Down / Ctrl+N", "Next result"),
    ("Up / Ctrl+P", "Previous result"),
    ("PageDown / PageUp", "Next / previous page"),
    ("Backspace", "Delete the last query character"),
    ("Ctrl+W", "Delete the last query word"),
    ("Ctrl+U", "Clear the query"),
    ("Ctrl+Shift+V", "Paste into the query"),
];

pub(crate) const LIST: &[(&str, &str)] = &[
    ("j / Down / Ctrl+N", "Next item"),
    ("k / Up / Ctrl+P", "Previous item"),
    ("gg / Home", "First item"),
    ("G / End", "Last item"),
    ("Ctrl+D / Ctrl+U", "Half page down / up"),
    (
        "PageDown / PageUp / Ctrl+F / Ctrl+B",
        "Page through the list",
    ),
    (
        "1–9, then digits",
        "Repeat the next movement that many times",
    ),
];

/// Convert the compact labels used by built-in help to mapping notation.
/// Parameterised rows are deliberately left as labels, not guessed bindings.
pub(crate) fn notation(key: &str) -> String {
    let key = key.split(" + ").next().unwrap_or(key);
    let key = key.strip_suffix(", register").unwrap_or(key);
    if key.contains('+') {
        let mut parts: Vec<_> = key.split('+').collect();
        let last = parts.pop().unwrap_or_default();
        let modifiers = parts
            .iter()
            .map(|part| match *part {
                "Ctrl" => "C",
                "Shift" => "S",
                "Alt" => "A",
                other => other,
            })
            .collect::<Vec<_>>()
            .join("-");
        let last = if last.len() == 1 {
            last.to_lowercase()
        } else {
            last.to_owned()
        };
        format!("<{modifiers}-{last}>")
    } else if matches!(
        key,
        "Enter"
            | "Esc"
            | "Backspace"
            | "Delete"
            | "Tab"
            | "Home"
            | "End"
            | "Up"
            | "Down"
            | "Left"
            | "Right"
            | "PageUp"
            | "PageDown"
    ) {
        format!("<{key}>")
    } else {
        key.to_owned()
    }
}

pub(crate) const SIMPLE_PROMPT: &[(&str, &str)] = &[
    ("Enter", "Accept"),
    ("Esc / Ctrl+;", "Cancel"),
    ("Backspace", "Delete the last character; cancel if empty"),
    ("Ctrl+W", "Delete the last word"),
    ("Ctrl+U", "Clear the text"),
    ("Ctrl+Shift+V", "Paste clipboard text"),
];

pub(crate) fn list_context(
    title: &str,
    typed: &crate::vim::HalfTyped,
    rows: &[(&str, &str)],
) -> Context {
    let mut context = Context::new(title, LIST);
    context.add(rows);
    if let Some(prefix) = typed.key {
        context.title = format!("{title} · after {prefix}");
        context.rows = context
            .rows
            .into_iter()
            .filter_map(|(keys, description)| {
                let keys = keys
                    .split(" / ")
                    .filter_map(|key| {
                        if key.starts_with("Ctrl+")
                            || matches!(
                                key,
                                "Up" | "Down"
                                    | "Left"
                                    | "Right"
                                    | "Home"
                                    | "End"
                                    | "Enter"
                                    | "PageDown"
                                    | "PageUp"
                            )
                        {
                            Some(key)
                        } else {
                            key.strip_prefix(prefix)
                        }
                    })
                    .filter(|key| !key.is_empty())
                    .collect::<Vec<_>>()
                    .join(" / ");
                (!keys.is_empty()).then_some((keys, description))
            })
            .collect();
        context.add(&[("Esc / Ctrl+;", "Cancel the pending command")]);
    }
    context
}

/// Mapping notation is for config; the help panel spells chords out.
pub(crate) fn sequence(keys: &[crate::keymap::Chord]) -> String {
    keys.iter()
        .map(|key| {
            let label = key.spoken();
            if let crate::keymap::Base::Char(ch) = key.base
                && ch.is_uppercase()
                && (key.ctrl || key.alt)
            {
                label.replacen('+', "+Shift+", 1)
            } else {
                label
            }
        })
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Register insertion is a transient prompt state of its own.
pub(crate) fn prompt(title: &str, line: &Line, history: bool) -> Context {
    if line.wants_register() {
        return Context::new(
            format!("{title} · insert register"),
            &[
                ("Register name", "Insert the register's contents"),
                (": / /", "Insert last command / search"),
                ("Ctrl+W", "Insert word under the editor cursor"),
                ("Esc / Ctrl+;", "Cancel register insertion or prompt"),
            ],
        );
    }
    let mut context = Context::new(title, PROMPT);
    if history {
        context.add(&[("Up / Down", "Previous / next search in history")]);
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::parse_keys;

    #[test]
    fn shortcut_help_uses_the_question_mark_not_plain_slash() {
        for (keys, wanted) in [
            ("<C-?>", true),
            ("<C-S-?>", true),
            ("?", false),
            ("<C-/>", false),
            ("<C-A-?>", false),
        ] {
            assert_eq!(
                requested(&parse_keys(keys).unwrap()[0].kept().press()),
                wanted,
                "{keys}"
            );
        }
    }

    #[test]
    fn shortcut_help_searches_keys_and_descriptions_and_reaches_the_last_row() {
        let rows = (0..35)
            .map(|index| (format!("key {index}"), format!("action {index}")))
            .collect();
        let mut help = Help::new(Context {
            title: "test".into(),
            rows,
        });
        help.view(9, 40);
        for _ in 0..40 {
            help.key(&parse_keys("<C-n>").unwrap()[0].kept().press());
        }
        assert!(
            help.view(9, 40)
                .rows
                .iter()
                .any(|row| row.text.contains("Show shortcuts"))
        );
        help.paste("ACTION 12");
        let view = help.view(9, 40);
        assert_eq!(view.rows.len(), 5);
        assert!(view.rows[3].text.contains("key 12"));
        help.paste(" no match");
        assert!(help.view(9, 40).empty.is_some());
        help.key(&parse_keys("<C-u>").unwrap()[0].kept().press());
        assert_eq!(help.view(9, 40).rows[3].text, "key 0");
        assert!(help.view(1, 1).rows.len() <= 1);
        for close in ["<Esc>", "<C-;>", "<C-?>"] {
            assert!(help.key(&parse_keys(close).unwrap()[0].kept().press()));
        }
    }
}
