//! A review pane: what agents changed, hunk by hunk, by file, kept by the
//! daemon (see `clackatoo_protocol`'s review) and shown and acted on in
//! here.
//!
//! Each hunk shows the lines around it, then what its lines were, on red,
//! and what they are, on green, as a chat's cards show a change; lines it
//! left as they were between its changes show as they are. The cursor and
//! what's scrolled to are the window's own: the daemon sends the whole
//! list again whenever it changes, and the cursor stays on the hunk it was
//! on, or the nearest.
//!
//! The keys:
//!
//! | Key | Does |
//! |---|---|
//! | `j` `k`, `gg` `G`, `H` `M` `L`, Ctrl+D Ctrl+U Ctrl+F Ctrl+E Ctrl+Y, `zt` `zz` `zb` | Moves, with a count, as every list does (see [`list`]) |
//! | `]h` `[h`, `]f` `[f` | To the next hunk or the one before, the next file or the one before |
//! | `/` `?` `n` `N` | Searches the lines, the files' names and the comments, with the editor's patterns |
//! | Enter | Goes to the line in an editor |
//! | `a`, `A` | Keeps the hunk, or every hunk of its file |
//! | `x`, `X` | Rejects the hunk, or every hunk of its file; on a comment, takes it back |
//! | `gc` | Comments on the line, for the agent |
//! | `S` | Sends the comments, as one message |
//! | `R` | Works the list out afresh |
//! | `q` | Closes the review |
//!
//! `a` is "accept"; `x` is Vim's delete, for a change rather than a
//! character, where `d` would wait for a motion, by habit. Keeping or
//! rejecting a whole turn is `:reviewkeep turn` or `:reviewreject turn`.
//!
//! A review of a turn or a chat lists its subagents' changes too, each
//! hunk saying whose, as `· explore (chat 7)`. The subagents are listed
//! under the heading, with what each was asked, and a line saying that
//! undoing the turn leaves their changes: rejecting them here takes them
//! out.

use clackatoo_protocol::{
    Attributes, Hunks, ReviewAction, ReviewFile, ReviewHunk, ReviewId, ReviewList, ReviewScope,
    Size, Style, TerminalId,
};
use clackatoo_term::{Row, RowBuilder};

use crate::colors;
use crate::hlsearch::Memo;
use crate::motion::Motion;
use crate::options::{Case, Options};
use crate::render::{CursorStyle, Highlight, HighlightKind, SceneCursor};
use crate::vim::list::{self, ListRows, Searching};
use crate::vim::search;
use crate::vim::{Action, Command, Dialect, Keys, Mode, Paging, Parsed, Read, Reader, Stroke};

#[cfg(test)]
mod tests;

/// The cells a line's number takes, with the space after it.
const NUMBER: usize = 6;

/// What a review asks of the desk, which it can't do itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Ask the daemon this, and say how it went.
    Act(ReviewAction),
    /// Go to line `line`, from 0, of the file at `path`, in an editor.
    Open { path: String, line: u64 },
    /// Ask for a comment on line `line`, from 1, of the file at `path`,
    /// for chat `chat`.
    Comment {
        chat: TerminalId,
        path: String,
        line: u32,
    },
    /// Close the review's pane.
    Close,
}

/// Whether a line of a hunk is as it was, added or removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sign {
    Kept,
    Added,
    Removed,
}

/// One line of the review, as shown.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    /// What's reviewed, and of which chat.
    Heading,
    /// What the keys do.
    Keys,
    /// What the daemon says, as why there's nothing to show.
    Note(String),
    /// A subagent whose changes are listed, by its place among the
    /// list's chats: who it is, and what it was asked.
    Subagent {
        chat: usize,
    },
    Blank,
    /// A file's path, and how many hunks it has.
    File {
        file: usize,
    },
    /// A hunk's first line, and who made it.
    Hunk {
        file: usize,
        hunk: usize,
    },
    /// A line of a hunk, or around it: its number in the file now, if it
    /// has one, and its text.
    Text {
        file: usize,
        hunk: usize,
        number: Option<u32>,
        sign: Sign,
        text: String,
    },
    /// The heading of the comments waiting to be sent.
    Comments,
    /// A comment, by its place among them.
    Comment {
        comment: usize,
    },
}

impl Line {
    /// The file and hunk it's of, if it's of one.
    fn hunk(&self) -> Option<(usize, usize)> {
        match *self {
            Self::Hunk { file, hunk } | Self::Text { file, hunk, .. } => Some((file, hunk)),
            _ => None,
        }
    }

    /// The file it's of, if any.
    fn file(&self) -> Option<usize> {
        match *self {
            Self::File { file } | Self::Hunk { file, .. } | Self::Text { file, .. } => Some(file),
            _ => None,
        }
    }
}

/// A pane showing a review.
#[derive(Debug)]
pub(crate) struct Review {
    id: ReviewId,
    list: ReviewList,
    /// What the window's title says.
    title: String,
    /// Whether the daemon has sent the list yet.
    heard: bool,
    lines: Vec<Line>,
    /// The line the cursor's on, and the first in view.
    cursor: usize,
    top: usize,
    /// Extra rows drawn only while the view is between whole rows.
    pub(crate) scroll_extra: usize,
    width: usize,
    height: usize,
    tabstop: usize,
    cursorline: bool,
    /// The rows drawn, as of the last settle, with how each is lit, and
    /// where the cursor is in them.
    rows: Vec<Row>,
    lit: Vec<Option<HighlightKind>>,
    cursor_cell: Option<(usize, usize)>,
    /// The keys of a command half typed.
    keys: Keys,
    /// How far its scrolling keys go.
    paging: Paging,
    /// The search being typed, the last, and what's lit.
    search: list::ListSearch,
    /// The last search's matches in view, as of the last settle.
    matches_lit: Vec<Highlight>,
    /// What the last search's matches in view were last found in, and
    /// them.
    lit_kept: Memo<LitKey, LitAt>,
    /// Which time the lines were made, which changes as what they show
    /// does.
    revision: u64,
    /// What to ask the desk, oldest first.
    asks: Vec<Ask>,
    /// What the bar says until the next key.
    message: Option<String>,
}

/// What the matches lit in view were last found in: which search, which
/// lines, as of which time they were made, and how wide they're drawn.
#[derive(Debug, PartialEq, Eq)]
struct LitKey {
    search: (String, bool, Case),
    top: usize,
    lines: usize,
    revision: u64,
    width: usize,
    tabstop: usize,
}

/// A match lit in view: on which of the lines, in which columns, and
/// whether it's its line's first, the one a search going to the line goes
/// to.
type LitAt = (usize, std::ops::Range<usize>, bool);

/// Something on a review's line, to find it again in a new list (see
/// [`Review::place_of`]).
#[derive(Debug, PartialEq, Eq)]
enum Place {
    /// A file's line, or a hunk's: the file's path, the hunk's first
    /// line in it, and how far past the file's or the hunk's first line.
    In {
        path: String,
        hunk: Option<u32>,
        past: usize,
    },
    /// A comment: the file it's on, its line and what it says, as two
    /// may say the same.
    Comment {
        path: String,
        line: u32,
        text: String,
    },
    Note(String),
}

impl Review {
    pub(crate) fn new(review: ReviewId, options: &Options) -> Self {
        let mut made = Self {
            id: review,
            list: ReviewList::default(),
            title: format!("Review {}", scope_words(None)),
            heard: false,
            lines: Vec::new(),
            cursor: 0,
            top: 0,
            scroll_extra: 0,
            width: 0,
            height: 0,
            tabstop: options.tabstop as usize,
            cursorline: options.cursorline,
            rows: Vec::new(),
            lit: Vec::new(),
            cursor_cell: None,
            keys: Keys::default(),
            paging: Paging::default(),
            search: list::ListSearch::of(options),
            matches_lit: Vec::new(),
            lit_kept: Memo::default(),
            revision: 0,
            asks: Vec::new(),
            message: None,
        };
        made.flatten();
        made
    }

    pub(crate) fn review(&self) -> ReviewId {
        self.id
    }

    /// The chat it's of, if the daemon has said.
    pub(crate) fn chat(&self) -> Option<TerminalId> {
        self.list.chat
    }

    /// What it lists.
    pub(crate) fn scope(&self) -> Option<ReviewScope> {
        self.list.scope
    }

    /// Takes in the options, as they've changed.
    pub(crate) fn set_options(&mut self, options: &Options) {
        self.tabstop = options.tabstop as usize;
        self.cursorline = options.cursorline;
        self.search.set_options(options);
    }

    pub(crate) fn resize(&mut self, size: Size) {
        let height = usize::from(size.rows());
        if height != self.height {
            self.paging.resized();
        }
        self.height = height;
        self.width = usize::from(size.cols());
    }

    /// Whether a search is being typed, in the bar.
    pub(crate) fn prompting(&self) -> bool {
        self.search.typing()
    }

    /// Pastes `text` into the search being typed, if one is. Returns
    /// whether one was.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        self.search.paste(text)
    }

    /// What the window's title says.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// What the bar says: a search being typed, or the keys of a command
    /// half typed, then a message, or where the cursor is.
    pub(crate) fn label(&self) -> String {
        if let Some(label) = self.search.label() {
            return label;
        }
        list::showing(&self.keys, self.place_label())
    }

    /// Ends what was half done as the focus leaves the review: a command
    /// half typed, and a search being typed.
    pub(crate) fn leave(&mut self) {
        self.keys.clear();
        search::give_up(&mut Searching(self));
    }

    /// What the bar says but for keys half typed: a message, or where the
    /// cursor is.
    fn place_label(&self) -> String {
        if let Some(message) = &self.message {
            return message.clone();
        }
        let total: usize = self.list.files.iter().map(|file| file.hunks.len()).sum();
        let line = self.lines.get(self.cursor);
        let Some((file, hunk)) = line.and_then(Line::hunk) else {
            return match line.and_then(Line::file) {
                Some(file) => {
                    let file = &self.list.files[file];
                    format!("{}  {}", file.shown, counted(file.hunks.len(), "hunk"))
                }
                None => format!("{} to review", counted(total, "hunk")),
            };
        };
        let before: usize = (self.list.files[..file].iter())
            .map(|file| file.hunks.len())
            .sum();
        format!(
            "{}  hunk {} of {total}",
            self.list.files[file].shown,
            before + hunk + 1
        )
    }

    /// The agent hunks of the hunk at the cursor, or of every hunk of its
    /// file if `file`, or of every hunk listed if `all`.
    pub(crate) fn hunks_here(&self, file: bool, all: bool) -> Vec<u64> {
        let line = self.lines.get(self.cursor);
        let ids = |files: &[ReviewFile]| -> Vec<u64> {
            (files.iter())
                .flat_map(|file| &file.hunks)
                .flat_map(|hunk| hunk.agents.iter().copied())
                .collect()
        };
        if all {
            return ids(&self.list.files);
        }
        if file {
            return line
                .and_then(Line::file)
                .map(|file| ids(&self.list.files[file..=file]))
                .unwrap_or_default();
        }
        (line.and_then(Line::hunk))
            .map(|(file, hunk)| self.list.files[file].hunks[hunk].agents.clone())
            .unwrap_or_default()
    }

    /// The turn of the hunk at the cursor, if an agent made it: for a
    /// subagent's, the turn of the review's chat that started it.
    pub(crate) fn turn_here(&self) -> Option<u32> {
        let (file, hunk) = self.lines.get(self.cursor)?.hunk()?;
        let made = self.list.files[file].hunks[hunk].by?;
        let started_in = (self.list.chats.iter())
            .find(|chat| chat.chat == made.chat)
            .and_then(|chat| chat.turn);
        Some(started_in.unwrap_or(made.turn))
            .filter(|&turn| turn != clackatoo_protocol::TURN_NOT_SHOWN)
    }

    // --- The daemon's news --------------------------------------------------

    /// Takes the list the daemon sent, keeping the cursor on the hunk it
    /// was on, or as near as it can.
    pub(crate) fn update(&mut self, list: ReviewList) {
        let at = self.place();
        let kept = list::KeptRows::of(&self.search, |row| self.place_of(row));
        self.list = list;
        self.title = format!("Review {}", scope_words(self.list.scope));
        self.heard = true;
        self.flatten();
        self.go_back_to(at);
        let found = kept.find(self.lines.len(), |place| self.find_place(place));
        found.put(&mut self.search);
    }

    /// What's on line `row`, to find again in a new list: a file's or a
    /// hunk's line by the file's path and the hunk's first line, a comment
    /// by its file, line and what it says, a note by what it says; nothing
    /// else.
    fn place_of(&self, row: usize) -> Option<Place> {
        let line = self.lines.get(row)?;
        if let Line::Comment { comment } = line {
            let comment = self.list.comments.get(*comment)?;
            return Some(Place::Comment {
                path: comment.path.clone(),
                line: comment.line,
                text: comment.text.clone(),
            });
        }
        if let Line::Note(note) = line {
            return Some(Place::Note(note.clone()));
        }
        let file = line.file()?;
        let hunk = line
            .hunk()
            .map(|(file, hunk)| self.list.files[file].hunks[hunk].line);
        let first = self.lines[..=row]
            .iter()
            .rposition(|line| matches!(line, Line::File { .. } | Line::Hunk { .. }))?;
        Some(Place::In {
            path: self.list.files[file].path.clone(),
            hunk,
            past: row - first,
        })
    }

    /// The line `place` is on, if it's there.
    fn find_place(&self, place: &Place) -> Option<usize> {
        let Place::In { path, hunk, past } = place else {
            return (self.lines.iter().enumerate()).position(|(row, line)| {
                matches!(line, Line::Comment { .. } | Line::Note(_))
                    && self.place_of(row).as_ref() == Some(place)
            });
        };
        let file = self.list.files.iter().position(|file| &file.path == path)?;
        let hunk = match hunk {
            Some(line) => Some(
                self.list.files[file]
                    .hunks
                    .iter()
                    .position(|hunk| hunk.line == *line)?,
            ),
            None => None,
        };
        let first = self.lines.iter().position(|line| match (line, hunk) {
            (Line::Hunk { file: f, hunk: h }, Some(hunk)) => *f == file && *h == hunk,
            (Line::File { file: f }, None) => *f == file,
            _ => false,
        })?;
        let row = first + past;
        let line = self.lines.get(row)?;
        let same = match hunk {
            Some(hunk) => line.hunk() == Some((file, hunk)),
            None => *past == 0,
        };
        same.then_some(row)
    }

    /// Where the cursor is, to find again in a new list: its file's path,
    /// and the line in it of its hunk, and its place among the lines.
    fn place(&self) -> (Option<(String, Option<u32>)>, usize) {
        let line = self.lines.get(self.cursor);
        let file = line.and_then(Line::file).map(|file| {
            let hunk = line
                .and_then(Line::hunk)
                .map(|(f, h)| self.list.files[f].hunks[h].line);
            (self.list.files[file].path.clone(), hunk)
        });
        (file, self.cursor)
    }

    /// Puts the cursor back where [`place`](Self::place) said it was.
    fn go_back_to(&mut self, (file, cursor): (Option<(String, Option<u32>)>, usize)) {
        let found = file.and_then(|(path, line)| {
            let file = self.list.files.iter().position(|file| file.path == path)?;
            let hunk = line.and_then(|line| {
                let hunks = &self.list.files[file].hunks;
                // The same hunk, or the next, or the last.
                hunks
                    .iter()
                    .position(|hunk| hunk.line >= line)
                    .or_else(|| hunks.len().checked_sub(1))
            });
            self.lines.iter().position(|shown| match (shown, hunk) {
                (Line::Hunk { file: f, hunk: h }, Some(hunk)) => *f == file && *h == hunk,
                (Line::File { file: f }, None) => *f == file,
                _ => false,
            })
        });
        self.cursor = found
            .unwrap_or(cursor)
            .min(self.lines.len().saturating_sub(1));
        if found.is_none() {
            // Off the hunks, as at the top: to the first.
            if self.lines.get(self.cursor).and_then(Line::hunk).is_none() {
                self.next_of(true, 1, |line| matches!(line, Line::Hunk { .. }));
            }
        }
    }

    /// Lays the list out as lines.
    fn flatten(&mut self) {
        self.revision += 1;
        let mut lines = vec![Line::Heading, Line::Keys, Line::Blank];
        if !self.heard {
            lines.push(Line::Note("Waiting for the daemon…".to_owned()));
        } else if let Some(note) = &self.list.note {
            lines.push(Line::Note(note.clone()));
        }
        let subagents: Vec<usize> = (self.list.chats.iter().enumerate())
            .filter(|(_, chat)| chat.turn.is_some())
            .map(|(index, _)| index)
            .collect();
        if !subagents.is_empty() {
            lines.extend(subagents.into_iter().map(|chat| Line::Subagent { chat }));
            let undoing = match self.list.scope {
                Some(ReviewScope::Turn(Some(turn))) => Some(format!("Undoing turn {}", turn + 1)),
                Some(ReviewScope::Chat) => Some("Undoing the chat's turns".to_owned()),
                _ => None,
            };
            if let Some(undoing) = undoing {
                lines.push(Line::Note(format!(
                    "{undoing} leaves subagents' changes: reject them here."
                )));
            }
        }
        for (index, file) in self.list.files.iter().enumerate() {
            if index > 0 || lines.len() > 3 {
                lines.push(Line::Blank);
            }
            lines.push(Line::File { file: index });
            for (number, hunk) in file.hunks.iter().enumerate() {
                lines.push(Line::Hunk {
                    file: index,
                    hunk: number,
                });
                lines.extend(hunk_lines(index, number, hunk));
            }
        }
        if !self.list.comments.is_empty() {
            lines.push(Line::Blank);
            lines.push(Line::Comments);
            lines.extend((0..self.list.comments.len()).map(|comment| Line::Comment { comment }));
        }
        self.lines = lines;
    }

    // --- Keys ---------------------------------------------------------------

    /// Does what `stroke` does. Returns whether it should be drawn again.
    pub(crate) fn stroke(&mut self, stroke: Stroke) -> bool {
        let had_message = self.message.take().is_some();
        if list::search_stroke(self, stroke) {
            return true;
        }
        let was_pending = self.keys.pending();
        match self.keys.read(stroke, false, &ReviewKeys) {
            Parsed::Incomplete => true,
            Parsed::Nothing => was_pending || had_message,
            Parsed::Command(command) => {
                self.execute(command);
                true
            }
        }
    }

    /// Does `command`, read whole.
    fn execute(&mut self, command: Command<ReviewKey>) {
        let mut paging = self.paging;
        let moved = list::go(self, &mut paging, &command);
        self.paging = paging;
        if moved || list::search_command(self, &command) {
            return;
        }
        let count = command.times();
        // Nothing else is taken (see `ReviewKeys::takes`).
        let Action::Own(own) = command.action else {
            return;
        };
        match own {
            ReviewKey::Go(motion) => {
                let mut paging = self.paging;
                list::go_own(self, &mut paging, motion, command.count);
                self.paging = paging;
            }
            ReviewKey::Open => self.open_here(),
            ReviewKey::Comment => self.comment_here(),
            ReviewKey::Next { file, forward } => {
                self.next_of(forward, count, |line| match line {
                    Line::File { .. } => file,
                    Line::Hunk { .. } => !file,
                    _ => false,
                });
            }
            ReviewKey::Keep { file } => self.act_on(file, true),
            ReviewKey::Reject => self.reject_or_uncomment(),
            ReviewKey::RejectFile => self.act_on(true, false),
            ReviewKey::Send => match self.list.chat {
                Some(chat) => self.asks.push(Ask::Act(ReviewAction::Send(chat))),
                None => self.message = Some("this review is of no chat".to_owned()),
            },
            ReviewKey::Refresh => {
                let review = self.id;
                self.asks.push(Ask::Act(ReviewAction::Refresh(review)));
            }
            ReviewKey::Close => self.asks.push(Ask::Close),
        }
    }

    /// The text of `line` a search looks in: a changed line's, a file's
    /// name, a comment's.
    fn searched<'a>(&'a self, line: &'a Line) -> Option<&'a str> {
        match line {
            Line::Text { text, .. } => Some(text),
            Line::File { file } => Some(&self.list.files[*file].shown),
            Line::Comment { comment } => {
                (self.list.comments.get(*comment)).map(|comment| comment.text.as_str())
            }
            Line::Note(note) => Some(note),
            _ => None,
        }
    }

    /// To the `count`th line `wanted` takes after the cursor's, or before
    /// it, going round the ends. Says so if there's none.
    fn next_of(&mut self, forward: bool, count: usize, wanted: impl Fn(&Line) -> bool) {
        let found = list::round(self.lines.len(), self.cursor, count, !forward, |at| {
            wanted(&self.lines[at])
        });
        match found {
            Some((at, _)) => self.cursor = at,
            None => self.message = Some("there are no hunks".to_owned()),
        }
    }

    /// Asks to keep, or reject, the hunk at the cursor, or every hunk of
    /// its file if `file`.
    fn act_on(&mut self, file: bool, keep: bool) {
        let ids = self.hunks_here(file, false);
        if ids.is_empty() {
            self.message = Some(match self.lines.get(self.cursor).and_then(Line::hunk) {
                Some(_) => "this change is yours, not an agent's".to_owned(),
                None => "there's no hunk here".to_owned(),
            });
            return;
        }
        let hunks = Hunks::Ids(ids);
        let action = if keep {
            ReviewAction::Keep(hunks)
        } else {
            ReviewAction::Reject {
                hunks,
                force: false,
            }
        };
        self.asks.push(Ask::Act(action));
    }

    /// `x`: rejects the hunk at the cursor, or takes back the comment
    /// there.
    fn reject_or_uncomment(&mut self) {
        if let Some(Line::Comment { comment }) = self.lines.get(self.cursor) {
            let (Some(chat), Some(comment)) = (self.list.chat, self.list.comments.get(*comment))
            else {
                return;
            };
            let id = comment.id;
            self.asks
                .push(Ask::Act(ReviewAction::Uncomment { chat, id }));
            return;
        }
        self.act_on(false, false);
    }

    /// The file and line, from 0, the cursor's line is of: a line of a
    /// hunk's own, or the hunk's first.
    fn line_in_file(&self) -> Option<(&ReviewFile, u64)> {
        let line = self.lines.get(self.cursor)?;
        let file = &self.list.files[line.file()?];
        let number = match line {
            Line::Text {
                number: Some(number),
                ..
            } => *number,
            Line::Text { file, hunk, .. } | Line::Hunk { file, hunk } => {
                self.list.files[*file].hunks[*hunk].line
            }
            _ => file.hunks.first().map_or(1, |hunk| hunk.line),
        };
        Some((file, u64::from(number.saturating_sub(1))))
    }

    /// Enter: goes to the cursor's line in an editor.
    fn open_here(&mut self) {
        if let Some((file, line)) = self.line_in_file() {
            let path = file.path.clone();
            self.asks.push(Ask::Open { path, line });
        }
    }

    /// `gc`: asks for a comment on the cursor's line, for the chat.
    fn comment_here(&mut self) {
        let Some(chat) = self.list.chat else {
            self.message = Some("this review is of no chat to comment to".to_owned());
            return;
        };
        let Some((file, line)) = self.line_in_file() else {
            self.message = Some("there's no line here to comment on".to_owned());
            return;
        };
        let path = file.path.clone();
        let line = u32::try_from(line + 1).unwrap_or(u32::MAX);
        self.asks.push(Ask::Comment { chat, path, line });
    }

    /// What to ask the desk, oldest first.
    pub(crate) fn take_asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.asks)
    }

    /// A click on row `row`: the cursor goes to the line there, and a
    /// double click goes to it in an editor, as Enter does.
    pub(crate) fn click(&mut self, row: usize, double: bool) -> bool {
        let line = self.top + row;
        if line >= self.lines.len() {
            return false;
        }
        self.cursor = line;
        if double {
            self.open_here();
        }
        true
    }

    /// Scrolls `lines` down, or up if negative, as the wheel does, the
    /// cursor staying in view.
    pub(crate) fn scroll(&mut self, lines: i64) -> bool {
        list::wheel(self, lines)
    }

    pub(crate) fn scroll_region(&self) -> std::ops::Range<usize> {
        0..self.height
    }

    pub(crate) fn native_scroll_room(&self, up: bool) -> bool {
        if up {
            self.top > 0
        } else {
            self.top < self.lines.len().saturating_sub(1)
        }
    }

    pub(crate) fn scroll_native(&mut self, lines: i64) -> i64 {
        let before = self.top;
        self.scroll(-lines);
        i64::try_from(before).unwrap_or(i64::MAX) - i64::try_from(self.top).unwrap_or(i64::MAX)
    }

    // --- Drawing ------------------------------------------------------------

    /// Lays out the rows shown, for [`scene`](Self::scene).
    pub(crate) fn settle(&mut self) {
        let height = self.height.max(1);
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + height {
            self.top = self.cursor + 1 - height;
        }
        self.top = self.top.min(self.lines.len().saturating_sub(1));
        let end = (self.top + height + self.scroll_extra).min(self.lines.len());
        let mut rows = Vec::with_capacity(height);
        let mut lit = Vec::with_capacity(height);
        // The column each line's text starts in.
        let mut starts = Vec::with_capacity(height);
        self.cursor_cell = None;
        for (index, line) in self.lines[self.top..end].iter().enumerate() {
            let (row, kind, col) = self.draw(line);
            if self.top + index == self.cursor {
                self.cursor_cell = Some((index, col));
            }
            rows.push(row);
            lit.push(kind);
            starts.push(col);
        }
        self.matches_lit = self.matches_in_view(&starts);
        while rows.len() < self.height + self.scroll_extra {
            rows.push(RowBuilder::new(self.width).finish(false));
            lit.push(None);
        }
        self.rows = rows;
        self.lit = lit;
    }

    /// The last search's matches drawn in the lines in view, from the
    /// top, each line's text from the column `starts` gives: with
    /// `hlsearch`, every match, looked for only as far along a line as is
    /// drawn and [`AROUND`] bytes past, and kept while the lines, the view
    /// and the search are as they were; and the line the search went to's
    /// first brighter.
    ///
    /// [`AROUND`]: crate::hlsearch::AROUND
    fn matches_in_view(&self, starts: &[usize]) -> Vec<Highlight> {
        let found = self.search.found();
        let lit = if self.search.every {
            let key = |lit: &list::ListLit| LitKey {
                search: lit.source.clone(),
                top: self.top,
                lines: starts.len(),
                revision: self.revision,
                width: self.width,
                tabstop: self.tabstop,
            };
            (self.search).lit_in_view(&self.lit_kept, key, |_| self.find_lit(starts, None))
        } else {
            // Only the one found is lit.
            let found = found.and_then(|found| found.checked_sub(self.top));
            found.map_or_else(Vec::new, |found| self.find_lit(starts, Some(found)))
        };
        (lit.into_iter())
            .map(|(index, cols, first)| Highlight {
                line: index,
                cols: cols.start.min(self.width)..cols.end.min(self.width),
                kind: if first && found == Some(self.top + index) {
                    HighlightKind::Match
                } else {
                    HighlightKind::OtherMatch
                },
            })
            .collect()
    }

    /// The matches drawn in the lines in view, or only in the `only`th of
    /// them (see [`matches_in_view`](Self::matches_in_view)).
    fn find_lit(&self, starts: &[usize], only: Option<usize>) -> Vec<LitAt> {
        let mut found = Vec::new();
        let lines = self.lines[self.top..].iter().zip(starts).enumerate();
        for (index, (line, &col)) in lines {
            if only.is_some_and(|only| only != index) {
                continue;
            }
            let Some(text) = self.searched(line) else {
                continue;
            };
            // A changed line's tabs as wide as they're drawn.
            let tabs = matches!(line, Line::Text { .. });
            let drawn = list::fitting(text, self.width.saturating_sub(col), self.cells(tabs));
            // Looked for from the line's start, the first is the one a
            // search goes to.
            let lit = list::lit_in(&self.search, text, 0..drawn, true);
            let cols = list::columns(text, &lit, self.cells(tabs));
            found.extend(
                (cols.into_iter())
                    .map(|(cols, first)| (index, col + cols.start..col + cols.end, first)),
            );
        }
        found
    }

    /// How many columns each character of a line takes, one after another
    /// from its start, as it's drawn: with `tabs`, a tab as far as the next
    /// tab stop, as [`expand_tabs`] has it.
    fn cells(&self, tabs: bool) -> impl FnMut(char) -> usize {
        let tabstop = self.tabstop.max(1);
        let mut chars = 0;
        move |ch| {
            if tabs && ch == '\t' {
                let to = (chars / tabstop + 1) * tabstop;
                let wide = to - chars;
                chars = to;
                wide
            } else {
                chars += 1;
                crate::render::mono_width(ch.encode_utf8(&mut [0; 4]))
            }
        }
    }

    /// Draws `line`. Returns its row, how it's lit, and the column the
    /// cursor goes in on it.
    fn draw(&self, line: &Line) -> (Row, Option<HighlightKind>, usize) {
        let mut builder = RowBuilder::new(self.width);
        let faint = Style {
            attributes: Attributes::DIM,
            ..Style::DEFAULT
        };
        let bold = Style {
            attributes: Attributes::BOLD,
            ..Style::DEFAULT
        };
        let mut kind = None;
        let mut col = 0;
        match line {
            Line::Heading => {
                let mut heading = format!("Review {}", scope_words(self.list.scope));
                if !self.list.chat_name.is_empty() {
                    heading.push_str(" · ");
                    heading.push_str(&self.list.chat_name);
                }
                let style = colors::heading();
                builder.push(&heading, &style, true);
            }
            Line::Keys => {
                let keys = "a keep  x reject  A X whole file  gc comment  S send  \
                            Enter go to  ]h [h hunks  q close";
                builder.push(&keys_that_fit(keys, self.width), &faint, true);
            }
            Line::Note(note) => builder.push(note, &faint, true),
            Line::Subagent { chat } => self.draw_subagent(&mut builder, *chat, &faint),
            Line::Blank => {}
            Line::File { file } => {
                let file = &self.list.files[*file];
                builder.push(&file.shown, &bold, true);
                let count = format!("  {}", counted(file.hunks.len(), "hunk"));
                builder.push(&count, &faint, true);
            }
            Line::Hunk { file, hunk } => {
                let hunk = &self.list.files[*file].hunks[*hunk];
                let mut said = format!("@@ line {}", hunk.line);
                if let Some(made) = hunk.by {
                    let name = self.chat_name(made.chat);
                    let style = Style {
                        fg: colors::agent(made.chat.0),
                        ..Style::DEFAULT
                    };
                    builder.push(&said, &faint, true);
                    said = match made.turn {
                        clackatoo_protocol::TURN_NOT_SHOWN => {
                            format!(" · {name}, on another branch")
                        }
                        turn => format!(" · {name}, turn {}", u64::from(turn) + 1),
                    };
                    builder.push(&said, &style, true);
                    if hunk.reviewed {
                        builder.push("  kept", &faint, true);
                    }
                } else {
                    said.push_str(" · yours");
                    builder.push(&said, &faint, true);
                }
            }
            Line::Text {
                number, sign, text, ..
            } => {
                let numbered = number.map_or_else(String::new, |number| number.to_string());
                let numbered = format!("{numbered:>0$} ", NUMBER - 1);
                builder.push(&numbered, &colors::line_number(false), true);
                let (mark, style) = match sign {
                    Sign::Kept => (" ", faint),
                    Sign::Added => ("+", Style::DEFAULT),
                    Sign::Removed => ("-", Style::DEFAULT),
                };
                builder.push(mark, &style, true);
                builder.push(" ", &Style::DEFAULT, true);
                col = builder.filled();
                builder.push(&expand_tabs(text, self.tabstop), &style, true);
                kind = match sign {
                    Sign::Kept => None,
                    Sign::Added => Some(HighlightKind::Added),
                    Sign::Removed => Some(HighlightKind::Removed),
                };
            }
            Line::Comments => {
                builder.push("Comments to send", &bold, true);
                builder.push("  S sends them  x takes one back", &faint, true);
            }
            Line::Comment { comment } => {
                let comment = &self.list.comments[*comment];
                let place = format!("✎ {}:{} ", comment.shown, comment.line);
                builder.push(&place, &faint, true);
                col = builder.filled();
                let text = comment.text.replace('\n', " ");
                builder.push(&text, &Style::DEFAULT, true);
            }
        }
        (builder.finish(false), kind, col)
    }

    /// Draws the `chat`th of the list's chats, a subagent: its name in its
    /// colour, what it was asked, and whether it has closed, `faint`.
    fn draw_subagent(&self, builder: &mut RowBuilder, chat: usize, faint: &Style) {
        let chat = &self.list.chats[chat];
        let style = Style {
            fg: colors::agent(chat.chat.0),
            ..Style::DEFAULT
        };
        builder.push("↳ ", faint, true);
        builder.push(&chat.name, &style, true);
        if let Some(task) = &chat.task {
            builder.push(&format!(": {task}"), &Style::DEFAULT, true);
        }
        if chat.closed {
            builder.push("  closed", faint, true);
        }
    }

    /// What chat `chat` is called, as far as the review knows.
    fn chat_name(&self, chat: TerminalId) -> String {
        if let Some(named) = self.list.chats.iter().find(|named| named.chat == chat) {
            named.name.clone()
        } else if self.list.chat == Some(chat) && !self.list.chat_name.is_empty() {
            self.list.chat_name.clone()
        } else {
            format!("chat {chat}")
        }
    }

    /// The rows to draw, and the cursor.
    pub(crate) fn scene(&self) -> (Vec<&Row>, Option<SceneCursor>) {
        let cursor = self.cursor_cell.map(|(line, col)| SceneCursor {
            line,
            col,
            style: CursorStyle::Block,
        });
        (self.rows.iter().collect(), cursor)
    }

    /// What's lit in view: lines added and removed, and the cursor's line.
    pub(crate) fn highlights(&self) -> Vec<Highlight> {
        let mut highlights: Vec<Highlight> = (self.lit.iter().enumerate())
            .filter_map(|(line, kind)| {
                Some(Highlight {
                    line,
                    cols: 0..self.width,
                    kind: (*kind)?,
                })
            })
            .collect();
        if self.cursorline
            && let Some((line, _)) = self.cursor_cell
        {
            highlights.push(Highlight {
                line,
                cols: 0..self.width,
                kind: HighlightKind::CursorLine,
            });
        }
        highlights.extend(self.matches_lit.iter().cloned());
        highlights
    }
}

/// The lines of `hunk`, the `hunk`th of the `file`th file: the lines
/// before it, what its lines were and are, compared line by line, and
/// the lines after it.
fn hunk_lines(file: usize, index: usize, hunk: &ReviewHunk) -> Vec<Line> {
    let text = |number: Option<u32>, sign: Sign, text: &str| Line::Text {
        file,
        hunk: index,
        number,
        sign,
        text: text.to_owned(),
    };
    let first = hunk.line;
    let before = u32::try_from(hunk.before.len()).unwrap_or(0);
    let mut lines: Vec<Line> = (hunk.before.iter().enumerate())
        .map(|(at, line)| {
            let number = (first + u32::try_from(at).unwrap_or(0)).checked_sub(before);
            text(number, Sign::Kept, line)
        })
        .collect();
    let (base, now): (Vec<&str>, Vec<&str>) =
        (hunk.base.lines().collect(), hunk.text.lines().collect());
    let mut number = first;
    let mut at = (0, 0);
    let push_kept =
        |lines: &mut Vec<Line>, upto: usize, at: &mut (usize, usize), number: &mut u32| {
            while at.1 < upto {
                lines.push(text(Some(*number), Sign::Kept, now[at.1]));
                *number += 1;
                at.0 += 1;
                at.1 += 1;
            }
        };
    for difference in clackatoo_text::diff(&hunk.base, &hunk.text) {
        push_kept(&mut lines, difference.after.start, &mut at, &mut number);
        for line in &base[difference.before.clone()] {
            lines.push(text(None, Sign::Removed, line));
        }
        for line in &now[difference.after.clone()] {
            lines.push(text(Some(number), Sign::Added, line));
            number += 1;
        }
        at = (difference.before.end, difference.after.end);
    }
    push_kept(&mut lines, now.len(), &mut at, &mut number);
    for line in &hunk.after {
        lines.push(text(Some(number), Sign::Kept, line));
        number += 1;
    }
    lines
}

/// What a review of `scope` is of, in words.
fn scope_words(scope: Option<ReviewScope>) -> String {
    match scope {
        Some(ReviewScope::Turn(Some(turn))) => format!("of turn {}", turn + 1),
        Some(ReviewScope::Turn(None)) => "of the last turn".to_owned(),
        Some(ReviewScope::Chat) | None => "of the chat".to_owned(),
        Some(ReviewScope::Base) => "since the branch's base".to_owned(),
    }
}

/// `keys`, a key and what it does at a time with two spaces between, in
/// `width` cells: as many as fit whole, then `…`, rather than cut off at
/// the pane's edge.
fn keys_that_fit(keys: &str, width: usize) -> String {
    use crate::render::mono_width;
    if mono_width(keys) <= width {
        return keys.to_owned();
    }
    let mut fit = String::new();
    for key in keys.split("  ") {
        let more = if fit.is_empty() {
            key.to_owned()
        } else {
            format!("{fit}  {key}")
        };
        // Room is kept for the `…` after them.
        if mono_width(&more) + 3 > width {
            break;
        }
        fit = more;
    }
    if fit.is_empty() {
        crate::finder::view::cut(keys, width, false)
    } else {
        fit + "  …"
    }
}

/// "3 hunks", or "1 hunk".
fn counted(count: usize, thing: &str) -> String {
    if count == 1 {
        format!("1 {thing}")
    } else {
        format!("{count} {thing}s")
    }
}

/// `line` with its tabs as spaces, to the next multiple of `tabstop`.
fn expand_tabs(line: &str, tabstop: usize) -> String {
    if !line.contains('\t') {
        return line.to_owned();
    }
    let tabstop = tabstop.max(1);
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    for ch in line.chars() {
        if ch == '\t' {
            let to = (col / tabstop + 1) * tabstop;
            out.extend(std::iter::repeat_n(' ', to - col));
            col = to;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// A command of a review's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReviewKey {
    /// Home, End, Ctrl+N and Ctrl+P (see [`list::own_move`]).
    Go(Motion),
    /// Enter: to the line in an editor.
    Open,
    /// `gc`: comment on the line.
    Comment,
    /// `]h`, `[h`, `]f` and `[f`: to the next hunk, or file, or the one
    /// before.
    Next { file: bool, forward: bool },
    /// `a`, or `A` for the whole file: keep.
    Keep { file: bool },
    /// `x`: reject the hunk, or take back the comment.
    Reject,
    /// `X`: reject the file's hunks.
    RejectFile,
    /// `S`: send the comments.
    Send,
    /// `R`: work the list out afresh.
    Refresh,
    /// `q`: close the review.
    Close,
}

/// How a review reads keys: a list's (see [`list`]), with its own for
/// what's reviewed, and `/` `?` `n` `N` searching its lines.
struct ReviewKeys;

impl Dialect for ReviewKeys {
    type Own = ReviewKey;

    fn describe(&self, action: ReviewKey) -> String {
        match action {
            ReviewKey::Go(motion) => return list::describe_move(motion),
            ReviewKey::Open => "Open file at this line",
            ReviewKey::Comment => "Comment on this line",
            ReviewKey::Next {
                file: false,
                forward: true,
            } => "Next hunk",
            ReviewKey::Next {
                file: false,
                forward: false,
            } => "Previous hunk",
            ReviewKey::Next {
                file: true,
                forward: true,
            } => "Next file",
            ReviewKey::Next {
                file: true,
                forward: false,
            } => "Previous file",
            ReviewKey::Keep { file: false } => "Keep hunk",
            ReviewKey::Keep { file: true } => "Keep file",
            ReviewKey::Reject => "Reject hunk or remove comment",
            ReviewKey::RejectFile => "Reject file",
            ReviewKey::Send => "Send review to agent",
            ReviewKey::Refresh => "Refresh review",
            ReviewKey::Close => "Close pane",
        }
        .into()
    }

    fn own(
        &self,
        stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<ReviewKey>> {
        if let Some(motion) = list::own_move(stroke) {
            return Some(Read::Yes(ReviewKey::Go(motion)));
        }
        let key = match stroke {
            Stroke::Enter => ReviewKey::Open,
            Stroke::Char { ch, ctrl: false } => match ch {
                'a' => ReviewKey::Keep { file: false },
                'A' => ReviewKey::Keep { file: true },
                'x' => ReviewKey::Reject,
                'X' => ReviewKey::RejectFile,
                'S' => ReviewKey::Send,
                'R' => ReviewKey::Refresh,
                'q' => ReviewKey::Close,
                _ => return None,
            },
            _ => return None,
        };
        Some(Read::Yes(key))
    }

    fn after_g(
        &self,
        stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<ReviewKey>> {
        (stroke.plain()? == 'c').then_some(Read::Yes(ReviewKey::Comment))
    }

    fn after_bracket(
        &self,
        backward: bool,
        stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<ReviewKey>> {
        let file = match stroke.plain()? {
            'h' => false,
            'f' => true,
            _ => return None,
        };
        Some(Read::Yes(ReviewKey::Next {
            file,
            forward: !backward,
        }))
    }

    fn takes(&self, command: &Command<ReviewKey>) -> bool {
        command.register.is_none()
            && (list::moves(command)
                || matches!(
                    command.action,
                    Action::Own(_) | Action::Search { .. } | Action::SearchAgain { .. }
                ))
    }
}

/// A review's search goes through its changed lines, its files' names and
/// its comments.
impl list::Searched for Review {
    type Start = list::Rows;

    fn search(&self) -> &list::ListSearch {
        &self.search
    }

    fn search_mut(&mut self) -> &mut list::ListSearch {
        &mut self.search
    }

    fn matches(&self, line: usize, pattern: &crate::editor::Pattern) -> bool {
        (self.searched(&self.lines[line])).is_some_and(|text| pattern.find_at(text, 0).is_some())
    }

    fn missed(&self, query: &str) -> String {
        format!("no line matches {query}")
    }

    fn say(&mut self, said: String) {
        self.message = Some(said);
    }

    fn keep(&self) -> list::Rows {
        list::keep_rows(self)
    }

    fn put_back(&mut self, start: &mut list::Rows, keep_view: bool) {
        list::put_rows_back(self, *start, keep_view);
    }
}

impl ListRows for Review {
    fn len(&self) -> usize {
        self.lines.len()
    }

    fn cursor(&self) -> usize {
        self.cursor
    }

    fn set_cursor(&mut self, item: usize) {
        self.cursor = item;
    }

    fn top(&self) -> usize {
        self.top
    }

    fn set_top(&mut self, item: usize) {
        self.top = item.min(self.lines.len().saturating_sub(1));
    }

    fn shown(&self) -> std::ops::Range<usize> {
        let end = (self.top + self.height.max(1)).min(self.lines.len());
        self.top..end.max(self.top + 1)
    }
}

impl crate::vim::ListKeys for Review {
    fn keys(&self) -> &Keys {
        &self.keys
    }

    fn keys_mut(&mut self) -> &mut Keys {
        &mut self.keys
    }

    fn prompting(&self) -> bool {
        Review::prompting(self)
    }
}

impl Review {
    pub(crate) fn shortcut_context(&self) -> crate::shortcuts::Context {
        if let Some(context) = self.search.shortcuts("Review search") {
            return context;
        }
        crate::vim::shortcuts::context("Review", &self.keys, Mode::Normal, &ReviewKeys)
    }
}
