//! Hints mode in the window (CLACKA-240): `Ctrl+B f`, or `:hints`, labels
//! everything worth picking in the focused pane, whatever its kind (see
//! [`crate::hints`]), and a label typed acts on what it labels:
//!
//! - its letters alone open it, as Ctrl+click does: a URL with the
//!   desktop's opener, asking first for one that isn't a plain web link
//!   (see [`Desk::open_url`]), a path in an editor at its line, a mail
//!   address in the mail program; what can't be opened, as a hash, is
//!   copied instead;
//! - with Shift on its last letter, it's copied, as Ctrl+Shift+C copies;
//! - with Alt on its last letter, it's typed into the pane, as a paste.
//!
//! Backspace takes back a letter, and Esc, or any other key, puts the
//! labels away. Labels are put where the pane drew the text when hints
//! started, which is what they pick, as kitty's hints do.

use clackatoo_protocol::{PaneId, TerminalId};
use winit::keyboard::{Key, NamedKey};

use super::Desk;
use super::view::PaneView;
use crate::chrome::Status;
use crate::editor::Register;
use crate::hints::{self, Hint, Kind};
use crate::input::KeyPress;
use crate::links::Target;
use crate::pane::{CopyTo, Yanked};

/// The hints shown, and the letters typed of a label so far.
#[derive(Debug)]
pub(super) struct Hinting {
    /// The pane they're in.
    pane: PaneId,
    /// The hints, each where it's drawn in the window.
    hints: Vec<Hint>,
    typed: String,
}

/// A hint as the window draws it: where, its label, and how many of its
/// letters are typed.
pub(crate) struct HintMark<'a> {
    pub(crate) rects: &'a [[f32; 4]],
    pub(crate) label: &'a str,
    pub(crate) typed: usize,
}

/// The most characters of what's copied the bar says.
const SAID: usize = 80;

/// `text`, copied, as the bar says it: as a link's target is shown, every
/// character that could hide or pass for another written out (see
/// [`crate::links::shown_whole`]), and cut short past [`SAID`]
/// characters.
fn shown_short(text: &str) -> String {
    let shown = crate::links::shown_whole(text);
    match shown.char_indices().nth(SAID - 1) {
        Some((cut, _)) if shown.chars().count() > SAID => format!("{}…", &shown[..cut]),
        _ => shown,
    }
}

/// What a label typed does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Open,
    Copy,
    Type,
}

impl Desk {
    /// Labels everything worth picking in the focused pane.
    pub(super) fn start_hints(&mut self) {
        let Some(shown) = self.shown else { return };
        let Some(area) = self.area else { return };
        let Some(tile) = self
            .placement()
            .and_then(|placement| placement.tile(shown.pane))
        else {
            return;
        };
        let extra = match self.hint_pattern() {
            Ok(extra) => extra,
            Err(err) => {
                self.warn(format!("hintpattern is left out: {err}"));
                None
            }
        };
        let Some(view) = self.pane_view(shown.pane) else {
            return;
        };
        // Below a subagent's band, as the pane draws it.
        let band = match view {
            PaneView::Terminal(id, _) | PaneView::Chat(id, _) => self.band_height(id),
            _ => 0.0,
        };
        let origin = [tile.origin[0], tile.origin[1] + band];
        let mut found = match view {
            PaneView::Chat(_, chat) => hints::hints(&chat.view().in_view(), extra.as_ref()),
            view => {
                let rows = if band > 0.0 {
                    tile.rows.saturating_sub(2).max(1)
                } else {
                    tile.rows
                };
                let Some(rows) = view.scene_rows(usize::from(rows)) else {
                    return;
                };
                let cells = hints::Cells::new(&rows, area.cell);
                hints::hints(&cells, extra.as_ref())
            }
        };
        if found.is_empty() {
            return self.say("nothing to pick here".into());
        }
        for hint in &mut found {
            for rect in &mut hint.rects {
                rect[0] += origin[0];
                rect[1] += origin[1];
            }
        }
        self.hinting = Some(Hinting {
            pane: shown.pane,
            hints: found,
            typed: String::new(),
        });
    }

    /// The `hintpattern` option's regular expression, if it's set: an
    /// error if it can't be read.
    fn hint_pattern(&self) -> Result<Option<regex::Regex>, String> {
        let pattern = &self.options.hintpattern;
        if pattern.is_empty() {
            return Ok(None);
        }
        regex::RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .build()
            .map(Some)
            .map_err(|err| err.to_string())
    }

    /// Takes `press` as a label's letter, if hints are shown: returns
    /// whether they were.
    pub(super) fn hint_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(hinting) = &mut self.hinting else {
            return false;
        };
        // Its pane left, as for another a macro went to, they're gone, and
        // the key is the pane's.
        if self.shown.map(|shown| shown.pane) != Some(hinting.pane) {
            self.hinting = None;
            return false;
        }
        let Some(hinting) = &mut self.hinting else {
            return false;
        };
        let letter = match press.key {
            Key::Named(NamedKey::Backspace) => {
                if hinting.typed.pop().is_none() {
                    self.hinting = None;
                }
                return true;
            }
            Key::Character(text) => (text.chars().next())
                .map(|letter| letter.to_ascii_lowercase())
                .filter(|letter| hints::ALPHABET.contains(*letter)),
            _ => None,
        };
        let (Some(letter), false) = (letter, press.modifiers.ctrl) else {
            self.hinting = None;
            return true;
        };
        hinting.typed.push(letter);
        let typed = hinting.typed.clone();
        let picked = (hinting.hints.iter()).position(|hint| hint.label == typed);
        if !(hinting.hints.iter()).any(|hint| hint.label.starts_with(&typed)) {
            self.hinting = None;
            self.say(format!("no hint is labelled {typed}"));
            return true;
        }
        let Some(picked) = picked else {
            return true;
        };
        let hinting = self.hinting.take().expect("hints are shown");
        let how = if press.modifiers.alt {
            Pick::Type
        } else if press.modifiers.shift {
            Pick::Copy
        } else {
            Pick::Open
        };
        let hint = hinting.hints.into_iter().nth(picked).expect("it was found");
        self.pick_hint(hinting.pane, hint, how);
        true
    }

    /// Does with `hint`, in `pane`, what `how` says.
    fn pick_hint(&mut self, pane: PaneId, hint: Hint, how: Pick) {
        let opens = match (how, &hint.target) {
            (Pick::Open, Some(target)) => Some(target.clone()),
            _ => None,
        };
        match (how, opens) {
            (Pick::Type, _) => self.paste(&hint.text),
            (Pick::Open, Some(target)) => self.open_hint(pane, target),
            (Pick::Copy | Pick::Open, _) => {
                self.say(format!("copied {}", shown_short(&hint.text)));
                self.pane_yanked(Yanked {
                    register: Register::chars(hint.text),
                    to: CopyTo::Clipboard,
                });
            }
        }
    }

    /// Opens `target`, a hint's in `pane`, as Ctrl+click opens a link
    /// there: a relative path from the pane's folder.
    fn open_hint(&mut self, pane: PaneId, target: Target) {
        let terminal = self.terminal_in(pane);
        let cwd = self.folder_of(pane);
        // Beside the terminal or chat it's in, or wherever an editor's
        // file would open.
        let from = terminal.unwrap_or(TerminalId(u32::MAX));
        self.open_link_of(from, target, cwd.as_deref());
    }

    /// The terminal or chat `pane` shows, if it shows one.
    fn terminal_in(&self, pane: PaneId) -> Option<TerminalId> {
        self.workspaces.find_pane(pane)?.2.content.terminal()
    }

    /// The folder a relative path in `pane` is from: a terminal's shell's,
    /// a chat's, or an editor's file's.
    fn folder_of(&self, pane: PaneId) -> Option<String> {
        match self.pane_view(pane)? {
            PaneView::Terminal(_, terminal) => terminal.cwd().map(str::to_owned),
            PaneView::Chat(_, chat) => {
                let view = chat.view();
                (view.paths().map(|paths| paths.cwd.as_str()))
                    .or_else(|| view.folder())
                    .map(str::to_owned)
            }
            // From the file's own folder, as a path in its text is.
            PaneView::Editor(_, _, copy) => (std::path::Path::new(copy.path()).parent())
                .and_then(std::path::Path::to_str)
                .filter(|folder| !folder.is_empty())
                .map(str::to_owned),
            PaneView::Explorer(..)
            | PaneView::Board(..)
            | PaneView::Review(..)
            | PaneView::Browser(..)
            | PaneView::Other(..) => None,
        }
    }

    /// The hints the window draws, if any are shown in the pane shown.
    pub(crate) fn hint_marks(&self) -> Vec<HintMark<'_>> {
        let Some(hinting) = &self.hinting else {
            return Vec::new();
        };
        if self.shown.map(|shown| shown.pane) != Some(hinting.pane) {
            return Vec::new();
        }
        let typed = hinting.typed.as_str();
        (hinting.hints.iter())
            .filter(|hint| hint.label.starts_with(typed))
            .map(|hint| HintMark {
                rects: &hint.rects,
                label: &hint.label,
                typed: typed.len(),
            })
            .collect()
    }

    /// Whether hints are shown, taking keys.
    pub(crate) fn hinting(&self) -> bool {
        self.hinting.is_some()
    }

    /// What the bar says while hints are shown.
    pub(super) fn hint_status(&self) -> Option<Status> {
        let hinting = self.hinting.as_ref()?;
        let count = (hinting.hints.iter())
            .filter(|hint| hint.label.starts_with(&hinting.typed))
            .count();
        let kinds = |kind: Kind| (hinting.hints.iter()).any(|hint| hint.kind == kind);
        let opens = if kinds(Kind::Url) || kinds(Kind::Path) || kinds(Kind::Email) {
            "opens"
        } else {
            "copies"
        };
        Some(Status::Typing {
            label: Some("HINTS".into()),
            text: format!(
                "{}  · {count} · a label {opens}, Shift+label copies, Alt+label types · Esc",
                hinting.typed
            ),
            choices: Vec::new(),
            selected: None,
            under: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_bar_says_was_copied_is_shown_whole_and_cut_short() {
        assert_eq!(shown_short("7e84c2e9"), "7e84c2e9");
        assert_eq!(shown_short("a\u{202e}b c"), "a<U+202E>b<U+0020>c");
        let long = "x".repeat(200);
        let said = shown_short(&long);
        assert_eq!(said.chars().count(), SAID);
        assert!(said.ends_with('…'));
        assert_eq!(shown_short(&"y".repeat(SAID)), "y".repeat(SAID));
    }
}
