//! Opening the links in terminals (see [`crate::pane`]'s links): URLs with
//! the desktop's opener, files in editor panes, at their line.
//!
//! The desk doesn't run the opener itself. It collects the URLs, and the
//! window hands them to the desktop (see [`crate::desktop::open`]), so
//! nothing here starts a process, and tests see what would have opened.
//! A URL that isn't a plain web link or a mail address is opened only once
//! the bar has shown it whole and been answered `y` (see
//! [`Desk::open_url`]), for every pane's links alike.
//!
//! A file opens where it's easiest to read beside the output that named
//! it:
//!
//! - in an editor already in the tab, in place of its file, as `:e`
//!   would, so clicking down a compiler's errors doesn't pile up panes;
//! - else beside the terminal, if it's wide enough to halve;
//! - else in a tab of its own.
//!
//! A relative path is from the terminal's working directory, as its shell
//! last reported it (OSC 7).

use std::path::{Component, Path, PathBuf};

use clackatoo_protocol::{
    Content, FileLocation, OpenFile, PaneTerminal, Request, Side, TerminalId,
};

use super::jumps::FilePlace;
use super::{Asked, Desk};
use crate::chrome::{PaletteChoice, Status};
use crate::input::KeyPress;
use crate::links::{self, Target};
use crate::vim::{self, confirm::Confirm};

/// The fewest columns a terminal needs for a file to open beside it, so
/// that each half is still as wide as a terminal usually is.
const SPLIT_COLS: u16 = 160;

/// A link's target asked about: the question, answered by the key that
/// opens it, and where it goes, as the palette shows it under the
/// question, a row at a time.
#[derive(Debug)]
pub(super) struct LinkAsked {
    confirm: Confirm<String>,
    rows: Vec<String>,
}

/// The most columns of a link's target a row of the palette shows: few
/// enough to fit the palette of a window of half a usual screen's width,
/// so none is ever cut short.
const ROW_COLUMNS: usize = 32;

/// The most rows a link's target is shown in: a longer target isn't
/// asked about, as all of it couldn't be seen.
const MOST_ROWS: usize = 8;

/// `shown`, a link's target as it's shown, in rows of at most
/// [`ROW_COLUMNS`] columns, if it fits in [`MOST_ROWS`] of them.
fn target_rows(shown: &str) -> Option<Vec<String>> {
    let mut rows = vec![String::new()];
    let mut columns = 0;
    for ch in shown.chars() {
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        if columns + width > ROW_COLUMNS {
            rows.push(String::new());
            columns = 0;
        }
        rows.last_mut()?.push(ch);
        columns += width;
    }
    (rows.len() <= MOST_ROWS).then_some(rows)
}

impl Desk {
    /// Opens what the pane showing `terminal` was asked to open, if
    /// anything: a link clicked or `gx`ed.
    pub(super) fn open_pane_link(&mut self, terminal: TerminalId) {
        let Some(pane) = self.panes.get_mut(&terminal) else {
            return;
        };
        let Some(target) = pane.take_opening() else {
            return;
        };
        let cwd = pane.cwd().map(str::to_owned);
        self.open_link_of(terminal, target, cwd.as_deref());
    }

    /// Opens `target`, a link in the terminal or chat `terminal`: a
    /// relative path is from `cwd`, or if that isn't known, the daemon
    /// finds it from the pane's working directory.
    pub(super) fn open_link_of(&mut self, terminal: TerminalId, target: Target, cwd: Option<&str>) {
        match target {
            Target::Url(url) => self.open_url(url),
            Target::File { path, line, col } => {
                self.open_file_at(terminal, &path, cwd, line, col);
            }
            Target::Elsewhere { uri, host } => {
                self.say(format!("{uri} is on {host}, not this machine"));
            }
        }
    }

    /// Takes the URLs links asked to open, for the window to hand to the
    /// desktop's opener.
    pub(crate) fn take_opened(&mut self) -> Vec<String> {
        std::mem::take(&mut self.opened)
    }

    /// Opens `url`, a link's target, with the desktop's opener: at once if
    /// it's a plain web link or a mail address (see
    /// [`links::opens_unasked`]), or else once the bar has shown it whole
    /// and `y` says to. Every link a pane opens comes here: a terminal's,
    /// a chat's and rendered Markdown's.
    pub(super) fn open_url(&mut self, url: String) {
        if links::opens_unasked(&url) {
            self.opened.push(url);
            return;
        }
        let shown = links::shown_whole(&url);
        let Some(rows) = target_rows(&shown) else {
            let length = shown.chars().count();
            self.say(format!(
                "the link wasn't opened: where it goes is too long to show whole ({length} characters)"
            ));
            return;
        };
        let confirm = Confirm::new("Open this link? (y/n)").choice('y', "Open the link", url);
        self.link_asked = Some(LinkAsked { confirm, rows });
    }

    /// Takes `press` as the answer to whether to open a link, if that's
    /// asked: `y` opens it, and any other key gives up, as does a key not
    /// pressed just now, which can't have been pressed for the question:
    /// one held while something was waited for, or one a macro or a
    /// binding types (`pressed` is false). Returns whether it was asked.
    pub(super) fn link_key(&mut self, press: &KeyPress<'_>, pressed: bool) -> bool {
        let Some(asked) = self.link_asked.take() else {
            return false;
        };
        let answer = (vim::strokes(press, false).first())
            .filter(|_| pressed)
            .and_then(|&stroke| asked.confirm.answer(stroke));
        match answer {
            Some(url) => self.opened.push(url),
            None => self.say("the link wasn't opened".to_owned()),
        }
        true
    }

    /// The question asked about a link, for the bar, with where it goes
    /// under it.
    pub(super) fn link_status(&self) -> Option<Status> {
        let asked = self.link_asked.as_ref()?;
        Some(Status::Typing {
            label: None,
            text: asked.confirm.question().to_owned(),
            choices: (asked.rows.iter())
                .map(|row| PaletteChoice::from((row.clone(), false)))
                .collect(),
            selected: None,
            under: None,
        })
    }

    /// What Ctrl+? says the keys do while a link's asked about.
    pub(super) fn link_shortcuts(&self) -> Option<crate::shortcuts::Context> {
        let asked = self.link_asked.as_ref()?;
        Some(crate::shortcuts::bindings::list(
            "Open a link",
            None,
            |stroke, _| asked.confirm.hint(stroke),
        ))
    }

    /// Opens `path`, which the terminal `terminal` printed, at `line` and
    /// `col` (counted from 1), in an editor pane.
    fn open_file_at(
        &mut self,
        terminal: TerminalId,
        path: &str,
        cwd: Option<&str>,
        line: Option<u64>,
        col: Option<u64>,
    ) {
        let Some(shown) = self.shown else { return };
        let path = absolute(path, cwd);
        // Only an absolute path can be told apart from others in the
        // window, to put the cursor on its line once it's open. A relative
        // one is left to the daemon, which finds it from the pane it opens
        // beside: the terminal.
        let known = Path::new(&path).is_absolute();
        let landing = line.filter(|_| known).map(|line| FileLocation {
            path: path.clone(),
            line: line.saturating_sub(1),
            col: col.map_or(0, |col| usize::try_from(col.saturating_sub(1)).unwrap_or(0)),
            offset: None,
            text: String::new(),
        });
        let editor = self.shown_tab().and_then(|tab| {
            (tab.layout.panes().iter())
                .filter(|pane| matches!(pane.content, Content::Buffer(_)))
                .map(|pane| pane.id)
                .find(|&pane| self.takes_files(pane))
        });
        if let Some(editor) = editor.filter(|_| known) {
            self.record_jump(shown.pane, None);
            self.show_pane(editor);
            match landing {
                Some(landing) => {
                    let file = crate::editor::File {
                        machine: self.pane_machine(editor),
                        path: &path,
                    };
                    let place = FilePlace::at_line(file, landing.line, landing.col, None);
                    self.show_file_place(editor, &place);
                }
                None if self.file_in(editor) != Some(path.as_str()) => {
                    self.open_here(editor, path);
                }
                None => {}
            }
            return;
        }
        let wide = (self.panes.get(&terminal))
            .and_then(|pane| pane.screen())
            .is_some_and(|screen| screen.size.cols() >= SPLIT_COLS);
        let beside = self.shown_tab().and_then(|tab| {
            (tab.layout.panes().iter())
                .find(|pane| pane.content == Content::Terminal(terminal))
                .map(|pane| pane.id)
        });
        let file = PaneTerminal::File(OpenFile { path, cwd: None });
        let request = if let Some(pane) = beside.filter(|_| wide) {
            Request::Split {
                pane,
                side: Side::Right,
                terminal: file,
                on: None,
            }
        } else {
            Request::NewTab {
                workspace: shown.workspace,
                terminal: file,
                on: None,
            }
        };
        self.request(request, Asked::PaneAt(landing));
    }
}

/// `path` made absolute against `cwd`, with its `.` and `..` worked out,
/// if it's relative and `cwd` is known. A path from the home directory
/// (`~/`) is left for the daemon, which knows where that is.
pub(super) fn absolute(path: &str, cwd: Option<&str>) -> String {
    let relative = Path::new(path);
    if relative.is_absolute() || path.starts_with('~') {
        return path.to_owned();
    }
    let Some(cwd) = cwd.filter(|cwd| Path::new(cwd).is_absolute()) else {
        return path.to_owned();
    };
    let mut made = PathBuf::new();
    for component in Path::new(cwd).join(relative).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                made.pop();
            }
            other => made.push(other),
        }
    }
    made.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_from_the_working_directory() {
        assert_eq!(absolute("src/a.rs", Some("/p")), "/p/src/a.rs");
        assert_eq!(absolute("./a/../b.rs", Some("/p/q")), "/p/q/b.rs");
        assert_eq!(absolute("../../../x", Some("/p")), "/x");
        assert_eq!(absolute("/etc/hosts", Some("/p")), "/etc/hosts");
        assert_eq!(absolute("~/notes.md", Some("/p")), "~/notes.md");
        assert_eq!(absolute("src/a.rs", None), "src/a.rs");
    }
}
