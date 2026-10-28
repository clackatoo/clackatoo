//! A board pane: a board's entries (see `clackatoo_protocol`'s boards),
//! as the daemon keeps them, shown as they change, and written on as the
//! user.
//!
//! Each entry is a line saying what it is, who wrote it and when, then
//! what it says, wrapped to the pane's width, and what it points to. The
//! newest entry is at the bottom, as in a chat, and the pane opens there.
//! The cursor is on an entry, not a line.
//!
//! | Key | Does |
//! |---|---|
//! | `j` `k`, `gg` `G`, `H` `M` `L`, Ctrl+D Ctrl+U Ctrl+F Ctrl+E Ctrl+Y, `zt` `zz` `zb` | Moves between entries, with a count, as every list does (see [`list`]) |
//! | `/` `?` `n` `N` | Searches the entries, with the editor's patterns |
//! | `i`, `o`, Enter | Posts a note, typed in the bar |
//! | `r` | Posts a result |
//! | `e` | Edits the entry at the cursor |
//! | `x` | Releases the claim at the cursor, whoever holds it |
//! | `A` | Archives the board, or unarchives it |
//! | `q` | Closes the pane: the board stays |

use std::time::{SystemTime, UNIX_EPOCH};

use clackatoo_protocol::{
    Attributes, BoardAction, BoardAuthor, BoardEntry, BoardId, BoardLink, BoardScope, BoardSummary,
    BoardUpdate, Color, PostKind, Size, Style,
};
use clackatoo_term::{Row, RowBuilder};

use crate::colors;
use crate::motion::Motion;
use crate::options::Options;
use crate::render::{CursorStyle, Highlight, HighlightKind, SceneCursor};
use crate::vim::list::{self, ListRows, Searching};
use crate::vim::search;
use crate::vim::{Action, Command, Dialect, Keys, Mode, Paging, Parsed, Read, Reader, Stroke};

#[cfg(test)]
mod tests;

/// How far an entry's text is set in from its heading, in cells.
const INDENT: usize = 3;

/// What the board asks of the desk, which it can't do itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// Close the board's pane.
    Close,
}

/// What's being typed in the bar.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Prompt {
    /// An entry to post, of this kind.
    Post { kind: PostKind, text: String },
    /// What entry `entry` is to say.
    Edit { entry: u32, text: String },
}

/// One line drawn: of entry `entry`, by its index, or a gap between them.
#[derive(Clone, Debug)]
struct Line {
    entry: Option<usize>,
    /// The bytes of the entry's text it draws, if it draws them, after
    /// [`INDENT`].
    text: Option<std::ops::Range<usize>>,
    row: Row,
}

/// A pane showing a board.
#[derive(Debug)]
// Separate settings and flags, not states of one machine.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct BoardPane {
    board: BoardId,
    /// What the board is, once the daemon has said.
    summary: Option<BoardSummary>,
    entries: Vec<BoardEntry>,
    /// Why it can't be shown, if the daemon said so.
    gone: Option<String>,
    /// What the window's title says.
    title: String,
    /// The entry the cursor's on, by its index, and whether it follows
    /// the newest, as it does until it's moved off it.
    cursor: usize,
    at_end: bool,
    /// The first line in view.
    top: usize,
    /// Extra rows drawn only while the view is between whole rows.
    pub(crate) scroll_extra: usize,
    width: usize,
    height: usize,
    /// The lines laid out, as of the last settle, and the rows in view.
    lines: Vec<Line>,
    rows: Vec<Row>,
    /// Where the cursor is drawn, and the rows of its entry, in view.
    cursor_cell: Option<(usize, usize)>,
    cursor_rows: std::ops::Range<usize>,
    /// The entries, or the width, changed since the last settle.
    stale: bool,
    /// When the lines were laid out, in milliseconds since the Unix
    /// epoch: how long ago things were, and how long claims have left,
    /// are said as of then.
    laid_at: u64,
    /// The keys of a command half typed.
    keys: Keys,
    /// How far its scrolling keys go.
    paging: Paging,
    /// The search being typed, the last, and what's lit.
    search: list::ListSearch<Start>,
    prompt: Option<Prompt>,
    cursorline: bool,
    /// What to ask the daemon, oldest first.
    outbox: Vec<BoardAction>,
    asks: Vec<Ask>,
    /// What the bar says until the next key, such as why something
    /// couldn't be done.
    message: Option<String>,
    /// The last search's matches in view, as of the last settle.
    lit: Vec<Highlight>,
}

impl BoardPane {
    pub(crate) fn new(board: BoardId, options: &Options) -> Self {
        Self {
            board,
            summary: None,
            entries: Vec::new(),
            gone: None,
            title: "Board".to_owned(),
            cursor: 0,
            at_end: true,
            top: 0,
            scroll_extra: 0,
            width: 0,
            height: 0,
            lines: Vec::new(),
            rows: Vec::new(),
            cursor_cell: None,
            cursor_rows: 0..0,
            stale: true,
            laid_at: 0,
            keys: Keys::default(),
            paging: Paging::default(),
            search: list::ListSearch::of(options),
            prompt: None,
            cursorline: options.cursorline,
            outbox: Vec::new(),
            asks: Vec::new(),
            message: None,
            lit: Vec::new(),
        }
    }

    pub(crate) fn board(&self) -> BoardId {
        self.board
    }

    /// Takes in the options, as they've changed.
    pub(crate) fn set_options(&mut self, options: &Options) {
        self.cursorline = options.cursorline;
        self.search.set_options(options);
    }

    pub(crate) fn resize(&mut self, size: Size) {
        let (width, height) = (usize::from(size.cols()), usize::from(size.rows()));
        if (width, height) != (self.width, self.height) {
            self.stale |= width != self.width;
            if height != self.height {
                self.paging.resized();
            }
            self.width = width;
            self.height = height;
        }
    }

    /// Whether an entry, or a search, is being typed, in the bar.
    pub(crate) fn prompting(&self) -> bool {
        self.prompt.is_some() || self.search.typing()
    }

    /// Pastes `text` into what's typed in the bar, an entry or a search,
    /// if anything is. Returns whether it was.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        match &mut self.prompt {
            Some(Prompt::Post { text: typed, .. } | Prompt::Edit { text: typed, .. }) => {
                list::paste_into(typed, text);
                true
            }
            None => self.search.paste(text),
        }
    }

    /// Ends what was half done as the focus leaves the board: a command
    /// half typed, and a search being typed. An entry being typed stays,
    /// to finish.
    pub(crate) fn leave(&mut self) {
        self.keys.clear();
        search::give_up(&mut Searching(self));
    }

    /// What the window's title says: "Board:" and the board's name, as
    /// other panes' titles are capitalised.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// What the bar says: what's being typed, or the keys of a command
    /// half typed, then a message, or the board and the keys.
    pub(crate) fn label(&self) -> String {
        if let Some(label) = self.search.label() {
            return label;
        }
        match &self.prompt {
            Some(Prompt::Post { kind, text }) => {
                let what = if *kind == PostKind::Result {
                    "result"
                } else {
                    "note"
                };
                return format!("post a {what}: {text}▏");
            }
            Some(Prompt::Edit { entry, text }) => return format!("edit #{entry}: {text}▏"),
            None => {}
        }
        list::showing(&self.keys, self.place_label())
    }

    /// What the bar says but for what's typed: a message, or the board and
    /// the keys.
    fn place_label(&self) -> String {
        if let Some(message) = &self.message {
            return message.clone();
        }
        let Some(summary) = &self.summary else {
            return self.gone.clone().unwrap_or_else(|| "board".to_owned());
        };
        if summary.archived {
            return format!("{} (archived)  A unarchives", summary.name);
        }
        format!(
            "{}  i posts · r a result · e edits · x releases",
            summary.name
        )
    }

    // --- The daemon's news --------------------------------------------------

    /// Takes in what the daemon says about the board.
    pub(crate) fn update(&mut self, update: BoardUpdate) {
        match update {
            BoardUpdate::Snapshot { summary, entries } => {
                self.summary = Some(summary);
                self.entries = entries;
                self.gone = None;
            }
            BoardUpdate::Changed { summary, entries } => {
                self.summary = Some(summary);
                for entry in entries {
                    match (entry.id as usize).checked_sub(1) {
                        Some(index) if index < self.entries.len() => self.entries[index] = entry,
                        _ => self.entries.push(entry),
                    }
                }
            }
            BoardUpdate::Refused(why) => self.message = Some(why),
            BoardUpdate::Gone(why) => {
                self.summary = None;
                self.entries.clear();
                self.gone = Some(why);
            }
        }
        self.title = match &self.summary {
            Some(summary) => format!("Board: {}", summary.name),
            None => "Board".to_owned(),
        };
        if self.at_end {
            self.cursor = self.entries.len().saturating_sub(1);
        }
        self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
        self.stale = true;
    }

    /// What to ask the daemon.
    pub(crate) fn take_outbox(&mut self) -> Vec<BoardAction> {
        std::mem::take(&mut self.outbox)
    }

    /// What to ask the desk.
    pub(crate) fn take_asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.asks)
    }

    // --- Keys ---------------------------------------------------------------

    /// Does what `stroke` does. Returns whether it should be drawn again.
    pub(crate) fn stroke(&mut self, stroke: Stroke) -> bool {
        let had_message = self.message.take().is_some();
        if self.prompt.is_some() {
            self.prompt_stroke(stroke);
            return true;
        }
        if list::search_stroke(self, stroke) {
            return true;
        }
        let was_pending = self.keys.pending();
        match self.keys.read(stroke, false, &BoardKeys) {
            Parsed::Incomplete => true,
            Parsed::Nothing => was_pending || had_message,
            Parsed::Command(command) => {
                self.execute(command);
                true
            }
        }
    }

    /// Does `command`, read whole.
    fn execute(&mut self, command: Command<BoardKey>) {
        let mut paging = self.paging;
        let moved = list::go(self, &mut paging, &command);
        self.paging = paging;
        if moved || list::search_command(self, &command) {
            return;
        }
        // Nothing else is taken (see `BoardKeys::takes`).
        let Action::Own(own) = command.action else {
            return;
        };
        match own {
            BoardKey::Go(motion) => {
                let mut paging = self.paging;
                list::go_own(self, &mut paging, motion, command.count);
                self.paging = paging;
            }
            BoardKey::Post(kind) => self.ask_post(kind),
            BoardKey::Edit => self.ask_edit(),
            BoardKey::Release => self.release(),
            BoardKey::Archive => {
                if let Some(summary) = &self.summary {
                    self.outbox.push(BoardAction::Archive(!summary.archived));
                }
            }
            BoardKey::Close => self.asks.push(Ask::Close),
        }
    }

    /// Starts typing an entry of `kind` to post, unless the board can't
    /// take one.
    fn ask_post(&mut self, kind: PostKind) {
        match &self.summary {
            Some(summary) if summary.archived => {
                self.message = Some("the board is archived: A unarchives it".to_owned());
            }
            Some(_) => {
                self.prompt = Some(Prompt::Post {
                    kind,
                    text: String::new(),
                });
            }
            None => {}
        }
    }

    /// Starts editing the entry at the cursor, with what it says now.
    fn ask_edit(&mut self) {
        if let Some(entry) = self.entries.get(self.cursor) {
            self.prompt = Some(Prompt::Edit {
                entry: entry.id,
                text: entry.text.replace('\n', " "),
            });
        }
    }

    /// Releases the claim at the cursor.
    fn release(&mut self) {
        match self.entries.get(self.cursor) {
            Some(entry) if entry.holds(now()) => {
                self.outbox.push(BoardAction::Release { entry: entry.id });
            }
            Some(_) => self.message = Some("there's no claim held here".to_owned()),
            None => {}
        }
    }

    /// Takes `stroke` for what's being typed in the bar.
    fn prompt_stroke(&mut self, stroke: Stroke) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let text = match prompt {
            Prompt::Post { text, .. } | Prompt::Edit { text, .. } => text,
        };
        match crate::cmdline::simple::key(text, stroke) {
            crate::cmdline::simple::Outcome::Accept => {
                match self.prompt.take().expect("a prompt") {
                    Prompt::Post { kind, text } if !text.trim().is_empty() => {
                        // What's posted is the newest, which the cursor follows.
                        self.at_end = true;
                        self.outbox.push(BoardAction::Post { kind, text });
                    }
                    Prompt::Edit { entry, text } if !text.trim().is_empty() => {
                        self.outbox.push(BoardAction::Edit { entry, text });
                    }
                    _ => {}
                }
            }
            crate::cmdline::simple::Outcome::Cancel => self.prompt = None,
            _ => {}
        }
    }

    /// Puts the cursor on entry `index`, from 0, or the last.
    fn go_to(&mut self, index: usize) {
        let last = self.entries.len().saturating_sub(1);
        self.cursor = index.min(last);
        self.at_end = self.cursor == last;
    }

    /// A click on row `row` of the pane: the cursor goes to the entry
    /// drawn there. Returns whether it should be drawn again.
    pub(crate) fn click(&mut self, row: usize) -> bool {
        let Some(line) = row.checked_sub(1).map(|row| self.top + row) else {
            return false;
        };
        let Some(entry) = self.lines.get(line).and_then(|line| line.entry) else {
            return false;
        };
        self.go_to(entry);
        true
    }

    /// Scrolls `lines` down, or up if it's negative, as the wheel does.
    pub(crate) fn scroll(&mut self, lines: i64) -> bool {
        let by = usize::try_from(lines.unsigned_abs()).unwrap_or(usize::MAX);
        let last = self.lines.len().saturating_sub(self.list_height());
        let top = if lines < 0 {
            self.top.saturating_sub(by)
        } else {
            self.top.saturating_add(by).min(last)
        };
        if top == self.top {
            return false;
        }
        self.top = top;
        // The cursor stays on an entry in view.
        let shown = &self.lines[top..(top + self.list_height()).min(self.lines.len())];
        let in_view = |index: usize| shown.iter().any(|line| line.entry == Some(index));
        if !in_view(self.cursor)
            && let Some(entry) = (if lines < 0 {
                shown.iter().rev().find_map(|line| line.entry)
            } else {
                shown.iter().find_map(|line| line.entry)
            })
        {
            self.go_to(entry);
        }
        true
    }

    fn list_height(&self) -> usize {
        self.height.saturating_sub(1).max(1)
    }

    pub(crate) fn scroll_region(&self) -> std::ops::Range<usize> {
        1..self.height.max(1)
    }

    pub(crate) fn native_scroll_room(&self, up: bool) -> bool {
        if up {
            self.top > 0
        } else {
            self.top < self.lines.len().saturating_sub(self.list_height())
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
        // What's said of times is kept to the half minute.
        if self.stale || now().saturating_sub(self.laid_at) > 30_000 {
            self.lay_out();
        }
        let height = self.list_height();
        // The cursor's entry in view, whole if it fits.
        let first = self
            .lines
            .iter()
            .position(|line| line.entry == Some(self.cursor));
        let last = self
            .lines
            .iter()
            .rposition(|line| line.entry == Some(self.cursor));
        if let (Some(first), Some(last)) = (first, last) {
            if last >= self.top + height {
                self.top = (last + 1).saturating_sub(height);
            }
            if first < self.top {
                self.top = first;
            }
        }
        self.top = self.top.min(self.lines.len().saturating_sub(1));
        let mut rows = Vec::with_capacity(self.height);
        rows.push(self.heading());
        let end = (self.top + height + self.scroll_extra).min(self.lines.len());
        self.cursor_cell = None;
        self.cursor_rows = 0..0;
        for (index, line) in self.lines[self.top..end].iter().enumerate() {
            if line.entry == Some(self.cursor) && !self.entries.is_empty() {
                if self.cursor_cell.is_none() {
                    self.cursor_cell = Some((index + 1, 0));
                    self.cursor_rows = (index + 1)..(index + 1);
                }
                self.cursor_rows.end = index + 2;
            }
            rows.push(line.row.clone());
        }
        while rows.len() < self.height + self.scroll_extra {
            rows.push(RowBuilder::new(self.width).finish(false));
        }
        rows.truncate(self.height + self.scroll_extra);
        self.rows = rows;
        self.lit = self.matches_in_view(end);
    }

    /// The last search's matches in the entries' text in view, the lines
    /// from the top to `end`, each on the row it's drawn in, while they're
    /// lit: every match, with `hlsearch`, and the entry the search went
    /// to's first brighter. Each entry's text is looked through only
    /// around what's drawn of it.
    fn matches_in_view(&self, end: usize) -> Vec<Highlight> {
        if self.search.lit().is_none() {
            return Vec::new();
        }
        let found = self.search.found();
        let mut highlights = Vec::new();
        let mut index = 0;
        // Each entry's lines in view, together.
        for lines in self.lines[self.top..end].chunk_by(|a, b| a.entry == b.entry) {
            let first = index;
            index += lines.len();
            let Some(entry) = lines[0].entry else {
                continue;
            };
            let pieces = || {
                (lines.iter().enumerate()).filter_map(|(at, line)| Some((at, line.text.clone()?)))
            };
            let Some(drawn) = pieces()
                .map(|(_, piece)| piece)
                .reduce(|a, b| a.start.min(b.start)..a.end.max(b.end))
            else {
                continue;
            };
            let text = &self.entries[entry].text;
            let lit = list::lit_in(&self.search, text, drawn, found == Some(entry));
            for (at, piece) in pieces() {
                for (bytes, brighter) in &lit {
                    let start = bytes.start.max(piece.start);
                    let stop = bytes.end.min(piece.end);
                    if start >= stop {
                        continue;
                    }
                    let col = INDENT + crate::render::mono_width(&text[piece.start..start]);
                    let cols = col..col + crate::render::mono_width(&text[start..stop]);
                    highlights.push(Highlight {
                        line: first + at + 1,
                        cols: cols.start.min(self.width)..cols.end.min(self.width),
                        kind: if *brighter {
                            HighlightKind::Match
                        } else {
                            HighlightKind::OtherMatch
                        },
                    });
                }
            }
        }
        highlights
    }

    /// The pane's first row: the board's name, whose it is, and how many
    /// entries it has.
    fn heading(&self) -> Row {
        let mut row = RowBuilder::new(self.width);
        let heading = colors::heading();
        let faint = colors::line_number(false);
        match &self.summary {
            Some(summary) => {
                row.push(&summary.name, &heading, true);
                let whose = match &summary.scope {
                    BoardScope::Global => "every workspace".to_owned(),
                    BoardScope::Workspace(name) => format!("workspace {name}"),
                };
                let mut about = format!("  {whose} · {} entries", summary.entries);
                if summary.archived {
                    about.push_str(" · archived");
                }
                row.push(&about, &faint, true);
                if !summary.purpose.is_empty() {
                    row.push(&format!(" · {}", summary.purpose), &faint, true);
                }
            }
            None => row.push(self.gone.as_deref().unwrap_or("…"), &faint, true),
        }
        row.finish(false)
    }

    /// Lays out every entry's lines at the pane's width.
    fn lay_out(&mut self) {
        self.stale = false;
        let now = now();
        self.laid_at = now;
        let mut lines = Vec::new();
        if self.entries.is_empty() && self.summary.is_some() {
            let mut row = RowBuilder::new(self.width);
            row.push(
                "Nothing here yet: i posts a note.",
                &colors::line_number(false),
                true,
            );
            lines.push(Line {
                entry: None,
                text: None,
                row: row.finish(false),
            });
        }
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                lines.push(Line {
                    entry: None,
                    text: None,
                    row: RowBuilder::new(self.width).finish(false),
                });
            }
            for (row, text) in self.entry_rows(entry, now) {
                lines.push(Line {
                    entry: Some(index),
                    text,
                    row,
                });
            }
        }
        self.lines = lines;
    }

    /// The rows of `entry` at `now`: its heading, what it says, and what
    /// it points to; with each row of what it says, the bytes of it drawn.
    fn entry_rows(
        &self,
        entry: &BoardEntry,
        now: u64,
    ) -> Vec<(Row, Option<std::ops::Range<usize>>)> {
        let faint = colors::line_number(false);
        let mut rows = Vec::new();
        let mut heading = RowBuilder::new(self.width);
        let (kind, colour) = match entry.kind {
            PostKind::Note => ("note", 4),
            PostKind::Claim => ("claim", 3),
            PostKind::Result => ("result", 2),
        };
        let kind_style = Style {
            fg: Color::Indexed(colour),
            attributes: Attributes::BOLD,
            ..Style::DEFAULT
        };
        heading.push(&format!("#{} ", entry.id), &faint, true);
        heading.push(kind, &kind_style, true);
        heading.push(&format!("  {}", author(&entry.by)), &Style::DEFAULT, true);
        let mut about = format!(" · {}", ago(entry.at, now));
        if !entry.edits.is_empty() {
            about.push_str(" · edited");
        }
        heading.push(&about, &faint, true);
        if entry.kind == PostKind::Claim {
            let (state, style) = match (&entry.released, entry.expires) {
                (Some((_, by)), _) => (format!("  released by {}", author(by)), faint),
                (None, Some(expires)) if expires > now => (
                    format!("  held, {} left", span(expires - now)),
                    Style {
                        fg: Color::Indexed(3),
                        ..Style::DEFAULT
                    },
                ),
                _ => ("  expired".to_owned(), faint),
            };
            heading.push(&state, &style, true);
        }
        rows.push((heading.finish(false), None));
        let wide = self.width.saturating_sub(INDENT).max(8);
        let indent = " ".repeat(INDENT);
        if let Some(subject) = &entry.subject {
            let style = Style {
                attributes: Attributes::BOLD,
                ..Style::DEFAULT
            };
            for piece in wrap(subject, wide) {
                let mut row = RowBuilder::new(self.width);
                row.push(&indent, &Style::DEFAULT, true);
                row.push(&subject[piece], &style, true);
                rows.push((row.finish(false), None));
            }
        }
        // Its lines, each as `str::lines` has them, wrapped.
        let mut at = 0;
        for chunk in entry.text.split_inclusive('\n') {
            let start = at;
            at += chunk.len();
            let line = chunk.strip_suffix('\n').unwrap_or(chunk);
            let line = line.strip_suffix('\r').unwrap_or(line);
            for piece in wrap(line, wide) {
                let mut row = RowBuilder::new(self.width);
                row.push(&indent, &Style::DEFAULT, true);
                row.push(&line[piece.clone()], &Style::DEFAULT, true);
                rows.push((
                    row.finish(false),
                    Some(start + piece.start..start + piece.end),
                ));
            }
        }
        for link in &entry.links {
            let text = match link {
                BoardLink::File {
                    path,
                    line: Some(line),
                } => format!("→ {path}:{line}"),
                BoardLink::File { path, line: None } => format!("→ {path}"),
                BoardLink::Turn { chat, turn } => format!("→ chat {chat}, turn {turn}"),
            };
            let mut row = RowBuilder::new(self.width);
            row.push(&indent, &Style::DEFAULT, true);
            row.push(&text, &faint, true);
            rows.push((row.finish(false), None));
        }
        rows
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

    /// What's highlighted in view: the cursor's entry, every line of it,
    /// and the last search's matches.
    pub(crate) fn highlights(&self) -> Vec<Highlight> {
        let cursor = (self.cursor_rows.clone())
            .filter(|_| self.cursorline)
            .map(|line| Highlight {
                line,
                cols: 0..self.width,
                kind: HighlightKind::CursorLine,
            });
        cursor.chain(self.lit.iter().cloned()).collect()
    }
}

/// `author`, as the pane shows it.
fn author(author: &BoardAuthor) -> String {
    match author {
        BoardAuthor::User => "you".to_owned(),
        BoardAuthor::Chat { chat, agent, .. } => format!("chat {chat} · {agent}"),
    }
}

/// How long ago `at` was, at `now`, both in milliseconds: "now", "5m
/// ago", "2h ago", "3d ago".
fn ago(at: u64, now: u64) -> String {
    let seconds = now.saturating_sub(at) / 1000;
    if seconds < 60 {
        "now".to_owned()
    } else {
        format!("{} ago", span(now - at))
    }
}

/// `ms` milliseconds, roughly: "40s", "5m", "1h 52m", "3d".
fn span(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 if seconds % 3600 >= 60 => {
            format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
        }
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// `text` cut into lines of at most `width` cells, at spaces where it
/// can be, and anywhere where it can't: the bytes of each.
fn wrap(text: &str, width: usize) -> Vec<std::ops::Range<usize>> {
    let mut pieces = Vec::new();
    // The piece being filled, and how wide it is.
    let (mut piece, mut cells) = (0..0, 0);
    let mut at = 0;
    for word in text.split(' ') {
        let start = at;
        at += word.len() + 1;
        let wide = crate::render::mono_width(word);
        let gap = usize::from(!piece.is_empty());
        if cells + gap + wide <= width {
            if piece.is_empty() {
                piece = start..start;
            }
            piece.end = start + word.len();
            cells += gap + wide;
            continue;
        }
        if !piece.is_empty() {
            pieces.push(std::mem::replace(&mut piece, 0..0));
            cells = 0;
        }
        // Too long for a line of its own: cut where it reaches the edge.
        for (offset, ch) in word.char_indices() {
            let wide = crate::render::mono_width(ch.encode_utf8(&mut [0; 4]));
            if cells + wide > width && !piece.is_empty() {
                pieces.push(std::mem::replace(&mut piece, 0..0));
                cells = 0;
            }
            if piece.is_empty() {
                piece = start + offset..start + offset;
            }
            piece.end = start + offset + ch.len_utf8();
            cells += wide;
        }
    }
    if !piece.is_empty() || pieces.is_empty() {
        pieces.push(piece);
    }
    pieces
}

/// Now, in milliseconds since the Unix epoch.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// A command of a board's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoardKey {
    /// Home, End, Ctrl+N and Ctrl+P (see [`list::own_move`]).
    Go(Motion),
    /// `i`, `o`, `a` and Enter post a note, `r` a result.
    Post(PostKind),
    /// `e`: edit the entry at the cursor.
    Edit,
    /// `x`: release the claim at the cursor.
    Release,
    /// `A`: archive the board, or unarchive it.
    Archive,
    /// `q`: close the pane.
    Close,
}

/// How a board reads keys: a list's (see [`list`]), with its own for
/// posting and the like, and `/` `?` `n` `N` searching its entries.
struct BoardKeys;

impl Dialect for BoardKeys {
    type Own = BoardKey;

    fn describe(&self, action: BoardKey) -> String {
        match action {
            BoardKey::Go(motion) => return list::describe_move(motion),
            BoardKey::Post(PostKind::Result) => "Post a result",
            BoardKey::Post(_) => "Post a note",
            BoardKey::Edit => "Edit entry",
            BoardKey::Release => "Release claim",
            BoardKey::Archive => "Archive or unarchive board",
            BoardKey::Close => "Close pane",
        }
        .into()
    }

    fn own(&self, stroke: Stroke, _keys: &mut Reader<'_>, _visual: bool) -> Option<Read<BoardKey>> {
        if let Some(motion) = list::own_move(stroke) {
            return Some(Read::Yes(BoardKey::Go(motion)));
        }
        let key = match stroke {
            Stroke::Enter => BoardKey::Post(PostKind::Note),
            Stroke::Char { ch, ctrl: false } => match ch {
                'i' | 'o' | 'a' => BoardKey::Post(PostKind::Note),
                'r' => BoardKey::Post(PostKind::Result),
                'e' => BoardKey::Edit,
                'x' => BoardKey::Release,
                'A' => BoardKey::Archive,
                'q' => BoardKey::Close,
                _ => return None,
            },
            _ => return None,
        };
        Some(Read::Yes(key))
    }

    fn takes(&self, command: &Command<BoardKey>) -> bool {
        command.register.is_none()
            && (list::moves(command)
                || matches!(
                    command.action,
                    Action::Own(_) | Action::Search { .. } | Action::SearchAgain { .. }
                ))
    }
}

impl BoardPane {
    /// The lines laid out of entry `entry`, by its index.
    fn lines_of(&self, entry: usize) -> Option<std::ops::Range<usize>> {
        let first = (self.lines.iter()).position(|line| line.entry == Some(entry))?;
        let after = (self.lines[first..].iter())
            .position(|line| line.entry != Some(entry))
            .map_or(self.lines.len(), |past| first + past);
        Some(first..after)
    }
}

/// Where a board's cursor and view were as a search started, to put back.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Start {
    cursor: usize,
    /// Whether the cursor was on the last entry, following new ones.
    at_end: bool,
    /// The first line in view, and the entry it's of with how far into
    /// it, if it's of one, to find again as the entries are laid out
    /// afresh.
    top: usize,
    top_in: Option<(usize, usize)>,
}

/// A board's search goes through what its entries say.
impl list::Searched for BoardPane {
    type Start = Start;

    fn search(&self) -> &list::ListSearch<Start> {
        &self.search
    }

    fn search_mut(&mut self) -> &mut list::ListSearch<Start> {
        &mut self.search
    }

    fn matches(&self, entry: usize, pattern: &crate::editor::Pattern) -> bool {
        pattern.find_at(&self.entries[entry].text, 0).is_some()
    }

    fn missed(&self, query: &str) -> String {
        format!("no entry matches {query}")
    }

    fn say(&mut self, said: String) {
        self.message = Some(said);
    }

    fn keep(&self) -> Start {
        let top_in = (self.lines.get(self.top))
            .and_then(|line| line.entry)
            .and_then(|entry| Some((entry, self.top - self.lines_of(entry)?.start)));
        Start {
            cursor: self.cursor,
            at_end: self.at_end,
            top: self.top,
            top_in,
        }
    }

    /// Back on the last entry, if it was on the last, whichever that is
    /// now, following new ones again.
    fn put_back(&mut self, start: &mut Start, keep_view: bool) {
        if start.at_end {
            self.go_to(self.entries.len());
        } else {
            self.go_to(start.cursor);
        }
        if keep_view {
            return;
        }
        let in_entry = start.top_in.and_then(|(entry, into)| {
            let lines = self.lines_of(entry)?;
            Some(lines.start + into.min(lines.len() - 1))
        });
        self.top = in_entry
            .unwrap_or(start.top)
            .min(self.lines.len().saturating_sub(1));
    }
}

/// A board is a list of its entries, each as many lines as it's laid out
/// in, as of the last settle.
impl ListRows for BoardPane {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn cursor(&self) -> usize {
        self.cursor
    }

    fn set_cursor(&mut self, item: usize) {
        self.go_to(item);
    }

    fn top(&self) -> usize {
        (self.lines.get(self.top..).unwrap_or_default().iter())
            .find_map(|line| line.entry)
            .unwrap_or(self.cursor)
    }

    fn set_top(&mut self, item: usize) {
        if let Some(lines) = self.lines_of(item) {
            self.top = lines.start;
        }
    }

    fn shown(&self) -> std::ops::Range<usize> {
        let end = (self.top + self.list_height()).min(self.lines.len());
        let top = self.top.min(end);
        let shown = &self.lines[top..end];
        // Entries starting in view, then those of them ending in it too.
        let first = shown.iter().enumerate().find_map(|(at, line)| {
            let entry = line.entry?;
            let starts = top + at == 0 || self.lines[top + at - 1].entry != Some(entry);
            starts.then_some(entry)
        });
        let last = shown.iter().enumerate().rev().find_map(|(at, line)| {
            let entry = line.entry?;
            let ends = self
                .lines
                .get(top + at + 1)
                .is_none_or(|next| next.entry != Some(entry));
            ends.then_some(entry)
        });
        if let Some((first, last)) = first.zip(last).filter(|(first, last)| first <= last) {
            first..last + 1
        } else {
            // One entry taller than the view.
            let at = ListRows::top(self);
            at..at + 1
        }
    }
}

impl crate::vim::ListKeys for BoardPane {
    fn keys(&self) -> &Keys {
        &self.keys
    }

    fn keys_mut(&mut self) -> &mut Keys {
        &mut self.keys
    }

    fn prompting(&self) -> bool {
        BoardPane::prompting(self)
    }
}

impl BoardPane {
    pub(crate) fn shortcut_context(&self) -> crate::shortcuts::Context {
        if let Some(prompt) = &self.prompt {
            let text = match prompt {
                Prompt::Post { text, .. } | Prompt::Edit { text, .. } => text,
            };
            return crate::cmdline::simple::help("Board entry", text);
        }
        if let Some(context) = self.search.shortcuts("Board search") {
            return context;
        }
        crate::vim::shortcuts::context("Board", &self.keys, Mode::Normal, &BoardKeys)
    }
}
