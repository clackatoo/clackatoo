//! Opening the links in terminals (see [`crate::pane`]'s links): URLs with
//! the desktop's opener, files in editor panes, at their line.
//!
//! The desk doesn't run the opener itself. It collects the URLs, and the
//! window hands them to the desktop (see [`crate::desktop::open`]), so
//! nothing here starts a process, and tests see what would have opened.
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
use crate::links::Target;

/// The fewest columns a terminal needs for a file to open beside it, so
/// that each half is still as wide as a terminal usually is.
const SPLIT_COLS: u16 = 160;

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
        match target {
            Target::Url(url) => self.opened.push(url),
            Target::File { path, line, col } => {
                self.open_file_at(terminal, &path, cwd.as_deref(), line, col);
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
                    let place = FilePlace::at_line(&path, landing.line, landing.col, None);
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
            }
        } else {
            Request::NewTab {
                workspace: shown.workspace,
                terminal: file,
            }
        };
        self.request(request, Asked::PaneAt(landing));
    }
}

/// `path` made absolute against `cwd`, with its `.` and `..` worked out,
/// if it's relative and `cwd` is known. A path from the home directory
/// (`~/`) is left for the daemon, which knows where that is.
fn absolute(path: &str, cwd: Option<&str>) -> String {
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
