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
//! | `j` `k`, `gg` `G`, Ctrl+D Ctrl+U | Moves between entries, with a count |
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
use crate::render::{CursorStyle, Highlight, HighlightKind, SceneCursor};
use crate::vim::Stroke;

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
    row: Row,
}

/// A pane showing a board.
#[derive(Debug)]
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
    /// A count typed, and `g`, waiting for the key after.
    typed: crate::vim::HalfTyped,
    prompt: Option<Prompt>,
    cursorline: bool,
    /// What to ask the daemon, oldest first.
    outbox: Vec<BoardAction>,
    asks: Vec<Ask>,
    /// What the bar says until the next key, such as why something
    /// couldn't be done.
    message: Option<String>,
}

impl BoardPane {
    pub(crate) fn new(board: BoardId, cursorline: bool) -> Self {
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
            typed: crate::vim::HalfTyped::default(),
            prompt: None,
            cursorline,
            outbox: Vec::new(),
            asks: Vec::new(),
            message: None,
        }
    }

    pub(crate) fn board(&self) -> BoardId {
        self.board
    }

    pub(crate) fn set_cursorline(&mut self, cursorline: bool) {
        self.cursorline = cursorline;
    }

    pub(crate) fn resize(&mut self, size: Size) {
        let (width, height) = (usize::from(size.cols()), usize::from(size.rows()));
        if (width, height) != (self.width, self.height) {
            self.stale |= width != self.width;
            self.width = width;
            self.height = height;
        }
    }

    /// Whether an entry is being typed, in the bar.
    pub(crate) fn prompting(&self) -> bool {
        self.prompt.is_some()
    }

    /// Whether a key is half typed, as `g` or a count is.
    pub(crate) fn pending(&self) -> bool {
        self.typed.pending()
    }

    /// What the window's title says: "Board:" and the board's name, as
    /// other panes' titles are capitalised.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// What the bar says: what's being typed, a message, or the board and
    /// the keys.
    pub(crate) fn label(&self) -> String {
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
        self.message = None;
        if self.prompt.is_some() {
            self.prompt_stroke(stroke);
            return true;
        }
        let count = self.typed.count.take();
        let pending = self.typed.key.take();
        let ch = match stroke {
            Stroke::Char { ch, ctrl: false } => ch,
            Stroke::Char { ch, ctrl: true } => {
                // Entries are a few lines each: half a pane is a few.
                let half = (self.list_height() / 6).max(1);
                match ch {
                    'd' | 'f' => self.move_by(count.unwrap_or(1) * half, true),
                    'u' | 'b' => self.move_by(count.unwrap_or(1) * half, false),
                    'n' => self.move_by(count.unwrap_or(1), true),
                    'p' => self.move_by(count.unwrap_or(1), false),
                    _ => return false,
                }
                return true;
            }
            Stroke::Down => {
                self.move_by(count.unwrap_or(1), true);
                return true;
            }
            Stroke::Up => {
                self.move_by(count.unwrap_or(1), false);
                return true;
            }
            Stroke::Home => {
                self.go_to(0);
                return true;
            }
            Stroke::End => {
                self.go_to(usize::MAX);
                return true;
            }
            Stroke::Enter => {
                self.ask_post(PostKind::Note);
                return true;
            }
            _ => return false,
        };
        match (pending, ch) {
            (None, '0'..='9') if ch != '0' || count.is_some() => {
                let digit = ch.to_digit(10).expect("a digit") as usize;
                let count = count.unwrap_or(0).saturating_mul(10).saturating_add(digit);
                self.typed.count = Some(count.min(1_000_000));
                return false;
            }
            (Some('g'), 'g') => self.go_to(count.map_or(0, |count| count - 1)),
            (Some(_), _) => {}
            (None, 'g') => {
                self.typed.count = count;
                self.typed.key = Some('g');
                return false;
            }
            (None, 'j') => self.move_by(count.unwrap_or(1), true),
            (None, 'k') => self.move_by(count.unwrap_or(1), false),
            (None, 'G') => self.go_to(count.map_or(usize::MAX, |count| count - 1)),
            (None, 'i' | 'o' | 'a') => self.ask_post(PostKind::Note),
            (None, 'r') => self.ask_post(PostKind::Result),
            (None, 'e') => self.ask_edit(),
            (None, 'x') => self.release(),
            (None, 'A') => {
                if let Some(summary) = &self.summary {
                    self.outbox.push(BoardAction::Archive(!summary.archived));
                }
            }
            (None, 'q') => self.asks.push(Ask::Close),
            _ => return false,
        }
        true
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
        match stroke {
            Stroke::Escape => self.prompt = None,
            Stroke::Backspace if text.is_empty() => self.prompt = None,
            Stroke::Backspace => {
                text.pop();
            }
            Stroke::Char {
                ch: 'u',
                ctrl: true,
            } => text.clear(),
            Stroke::Char {
                ch: 'w',
                ctrl: true,
            } => {
                let kept = text.trim_end_matches(|ch: char| !ch.is_alphanumeric());
                let kept = kept.trim_end_matches(char::is_alphanumeric).len();
                text.truncate(kept);
            }
            Stroke::Char { ch, ctrl: false } => text.push(ch),
            Stroke::Enter => match self.prompt.take().expect("a prompt") {
                Prompt::Post { kind, text } if !text.trim().is_empty() => {
                    // What's posted is the newest, which the cursor follows.
                    self.at_end = true;
                    self.outbox.push(BoardAction::Post { kind, text });
                }
                Prompt::Edit { entry, text } if !text.trim().is_empty() => {
                    self.outbox.push(BoardAction::Edit { entry, text });
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Moves the cursor `by` entries down, or up.
    fn move_by(&mut self, by: usize, down: bool) {
        let last = self.entries.len().saturating_sub(1);
        self.cursor = if down {
            self.cursor.saturating_add(by).min(last)
        } else {
            self.cursor.saturating_sub(by)
        };
        self.at_end = self.cursor == last;
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
                row: row.finish(false),
            });
        }
        for (index, entry) in self.entries.iter().enumerate() {
            if index > 0 {
                lines.push(Line {
                    entry: None,
                    row: RowBuilder::new(self.width).finish(false),
                });
            }
            for row in self.entry_rows(entry, now) {
                lines.push(Line {
                    entry: Some(index),
                    row,
                });
            }
        }
        self.lines = lines;
    }

    /// The rows of `entry` at `now`: its heading, what it says, and what
    /// it points to.
    fn entry_rows(&self, entry: &BoardEntry, now: u64) -> Vec<Row> {
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
        rows.push(heading.finish(false));
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
                row.push(&piece, &style, true);
                rows.push(row.finish(false));
            }
        }
        for piece in entry.text.lines().flat_map(|line| wrap(line, wide)) {
            let mut row = RowBuilder::new(self.width);
            row.push(&indent, &Style::DEFAULT, true);
            row.push(&piece, &Style::DEFAULT, true);
            rows.push(row.finish(false));
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
            rows.push(row.finish(false));
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

    /// What's highlighted in view: the cursor's entry, every line of it.
    pub(crate) fn highlights(&self) -> Vec<Highlight> {
        if !self.cursorline {
            return Vec::new();
        }
        (self.cursor_rows.clone())
            .map(|line| Highlight {
                line,
                cols: 0..self.width,
                kind: HighlightKind::CursorLine,
            })
            .collect()
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
/// can be, and anywhere where it can't.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut cells = 0;
    for word in text.split(' ') {
        let wide = crate::render::mono_width(word);
        let gap = usize::from(!line.is_empty());
        if cells + gap + wide <= width {
            if gap == 1 {
                line.push(' ');
            }
            line.push_str(word);
            cells += gap + wide;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            cells = 0;
        }
        // Too long for a line of its own: cut where it reaches the edge.
        for ch in word.chars() {
            let wide = crate::render::mono_width(ch.encode_utf8(&mut [0; 4]));
            if cells + wide > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                cells = 0;
            }
            line.push(ch);
            cells += wide;
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// Now, in milliseconds since the Unix epoch.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

impl crate::vim::ListPane for BoardPane {
    fn half_typed(&self) -> &crate::vim::HalfTyped {
        &self.typed
    }

    fn half_typed_mut(&mut self) -> &mut crate::vim::HalfTyped {
        &mut self.typed
    }

    fn prompting(&self) -> bool {
        BoardPane::prompting(self)
    }
}

impl BoardPane {
    pub(crate) fn shortcut_context(&self) -> crate::shortcuts::Context {
        use crate::shortcuts::{Context, SIMPLE_PROMPT, list_context};
        if self.prompt.is_some() {
            return Context::new("Board entry", SIMPLE_PROMPT);
        }
        let mut context = list_context(
            "Board",
            &self.typed,
            &[
                ("i / o / a", "Post a note"),
                ("r", "Post a result"),
                ("e", "Edit entry"),
                ("x", "Release claim"),
                ("A", "Archive or unarchive board"),
                ("q", "Close board"),
            ],
        );
        context.rows.retain(|(key, _)| !key.contains("PageDown"));
        context.add(&[("Ctrl+F / Ctrl+B", "Move down / up several entries")]);
        context
    }
}
