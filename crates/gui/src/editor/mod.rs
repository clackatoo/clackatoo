//! A pane editing a file: the Vim engine, shared with terminals' normal
//! mode (see [`crate::motion`] and [`crate::objects`]), over a buffer's
//! text.
//!
//! The text is the daemon's, and the window keeps a copy (see [`copy`]),
//! which the editor changes as you type. Each normal-mode command, and each
//! go of insert mode with the command that started it, is one undo step;
//! `u` and Ctrl+R ask the daemon, which keeps the history, so undo undoes
//! your own edits and not an agent's. `.` repeats the last change, with
//! what was typed in insert mode after it.
//!
//! Several panes can show one buffer, each with its own cursor, mode and
//! view. Edits made in one, or by anyone else, move the others' cursors
//! with the text around them.

mod copy;
mod keys;
mod text;

use std::time::{Duration, Instant};

use clackatoo_protocol::{BufferId, Edit, Size};
use clackatoo_term::{Row, RowBuilder, Style};
use clackatoo_text::Bias;
use winit::keyboard::{Key, NamedKey};

pub(crate) use copy::{BufferCopy, Heard};
use keys::{Action, Command, Entry, Operator, Parsed, Stroke, Target, ViewAt};
use text::{Laid, Text, tab_end};

use crate::colors;
use crate::input::KeyPress;
use crate::motion::{Class, Cursor, Lines, Motion, Point, Reach, before, class_at};
use crate::options::{Case, Options};
use crate::pane::{Mode, Selection, enters_normal_mode};
use crate::render::{CursorStyle, Highlight, HighlightKind, SceneCursor};

/// How long text copied stays highlighted, as in a terminal's normal mode.
const FLASH: Duration = Duration::from_millis(150);

/// The fewest cells left for the text beside the line numbers. A narrower
/// pane shows no numbers.
const LEAST_TEXT: usize = 12;

/// How many lines either side of those indented are looked at to see
/// whether the file is indented with tabs.
const NEARBY: u64 = 100;

/// The deepest `>` indents a line, in columns: far deeper than any code,
/// but short of a count such as `100000000>` asking for gigabytes.
const MAX_INDENT: usize = 1000;

/// Text kept to put back with `p`: what was last copied or deleted, by
/// characters, or whole lines.
#[derive(Clone, Debug, Default)]
pub(crate) struct Register {
    pub(crate) text: String,
    pub(crate) lines: bool,
}

/// What a key did that the window must see to.
#[derive(Debug, Default)]
pub(crate) struct Done {
    /// The pane must be drawn again.
    pub(crate) redraw: bool,
    /// Text copied, for the clipboard.
    pub(crate) yanked: Option<String>,
}

/// The text an operator acts on.
#[derive(Clone, Copy, Debug)]
struct Span {
    start: usize,
    end: usize,
    /// Whole lines, `first` to `last`.
    lines: Option<(u64, u64)>,
}

/// A change being made, for `.` to make again: the command, and what was
/// typed in insert mode after it.
#[derive(Clone, Debug)]
struct Change {
    command: Command,
    typed: Vec<Stroke>,
}

/// How an editor numbers its lines, as the options say.
#[derive(Clone, Copy, Debug)]
struct Numbers {
    /// The cursor's line shows its own number (`number`)...
    absolute: bool,
    /// ...and the others how far they are from it (`relativenumber`).
    relative: bool,
    /// The fewest cells the numbers take, with the space after them.
    least: usize,
}

impl Numbers {
    fn of(options: &Options) -> Self {
        Self {
            absolute: options.number,
            relative: options.relativenumber,
            least: options.numberwidth as usize,
        }
    }

    /// The number shown on `line`, with the cursor on `cursor`.
    fn label(self, line: u64, cursor: u64) -> u64 {
        if self.relative && line != cursor {
            line.abs_diff(cursor)
        } else if self.absolute {
            line + 1
        } else {
            0
        }
    }
}

/// How an editor indents, as the options say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Indent {
    /// The columns tabs stop at multiples of (`tabstop`).
    tabstop: usize,
    /// The columns `>>`, `<<` and Tab indent by (`shiftwidth`, or
    /// `tabstop` for 0).
    step: usize,
    /// A line not indented yet is indented with spaces (`expandtab`).
    spaces: bool,
}

impl Indent {
    fn of(options: &Options) -> Self {
        let tabstop = options.tabstop as usize;
        Self {
            tabstop,
            step: match options.shiftwidth {
                0 => tabstop,
                shiftwidth => shiftwidth as usize,
            },
            spaces: options.expandtab,
        }
    }

    /// Whether lines `first` to `last` of `text` are indented with tabs:
    /// as the first of them that's indented is, or else the nearest
    /// indented line around them. So a file stays as it's indented,
    /// whatever `expandtab` says, which decides only where no line nearby
    /// is indented.
    fn tabs_near(self, text: &Text<'_>, first: u64, last: u64) -> bool {
        let indented = |line| {
            let line_text = text.line_text(line);
            let indent = indent_of(&line_text);
            (!indent.is_empty()).then(|| indent.contains('\t'))
        };
        let last = last.min(text.last_line());
        let above = (first.saturating_sub(NEARBY)..first).rev();
        let below = last + 1..=(last + NEARBY).min(text.last_line());
        ((first..=last).chain(above).chain(below))
            .find_map(indented)
            .unwrap_or(!self.spaces)
    }

    /// The columns `blanks`, the start of a line, reach.
    fn width(self, blanks: &str) -> usize {
        blanks.chars().fold(0, |col, ch| match ch {
            '\t' => tab_end(col, self.tabstop),
            _ => col + 1,
        })
    }

    /// An indent reaching `width` columns: as many tabs as fit and spaces
    /// for the rest, if `tabs`, or all spaces.
    fn blanks(self, width: usize, tabs: bool) -> String {
        if tabs {
            let mut blanks = "\t".repeat(width / self.tabstop);
            blanks.push_str(&" ".repeat(width % self.tabstop));
            blanks
        } else {
            " ".repeat(width)
        }
    }
}

/// A pane editing a buffer.
#[derive(Debug)]
pub(crate) struct Editor {
    buffer: BufferId,
    pub(crate) mode: Mode,
    cursor: Cursor,
    /// Where a visual selection started.
    anchor: Option<Point>,
    /// The first line and column shown.
    top: u64,
    left: usize,
    height: usize,
    width: usize,
    /// How lines are numbered.
    numbers: Numbers,
    /// The cells the line numbers take, at the pane's left, out of `width`.
    gutter: usize,
    /// How lines are indented, and tabs drawn.
    indent: Indent,
    /// The fewest lines kept in view above and below the cursor
    /// (`scrolloff`).
    scrolloff: usize,
    /// The cursor's line is lit (`cursorline`).
    cursorline: bool,
    /// The columns marked down the pane, counting from 0 (`colorcolumn`).
    columns: Vec<usize>,
    /// How searches treat case.
    case: Case,
    /// How many of the rows shown are the file's lines, not past its end.
    filled: usize,
    /// The keys of a command being typed in normal or visual mode.
    typed: Vec<Stroke>,
    /// Whether the next edit starts an undo step: it's the first of a
    /// command, or of a go of insert mode.
    new_step: bool,
    /// The change being made, while insert mode goes on after it.
    changing: Option<Change>,
    /// The last change, for `.`.
    last_change: Option<Change>,
    /// How many times what's typed in insert mode goes in, as after `3i`.
    insert_times: usize,
    /// The last `f`, `t`, `F` or `T`.
    last_find: Option<Motion>,
    /// A search being typed: what, and whether backward.
    prompt: Option<(String, bool)>,
    /// The last search.
    search: Option<(String, bool)>,
    /// The last match found, highlighted until Esc.
    found: Option<(Point, Point)>,
    /// A short message for the bar, until the next key.
    status: Option<String>,
    /// Text just copied, highlighted for a moment.
    flash: Option<(Point, Point, Selection, Instant)>,
    /// Where the cursor and selection's start were in the text, before
    /// an edit someone else made.
    held: Option<(usize, Option<usize>)>,
    /// The lines shown, laid out.
    rows: Vec<Row>,
}

impl Editor {
    pub(crate) fn new(buffer: BufferId, options: &Options) -> Self {
        Self {
            buffer,
            mode: Mode::Normal,
            cursor: Cursor::new(Point { line: 0, col: 0 }),
            anchor: None,
            top: 0,
            left: 0,
            height: 1,
            width: 1,
            numbers: Numbers::of(options),
            gutter: 0,
            indent: Indent::of(options),
            scrolloff: options.scrolloff as usize,
            cursorline: options.cursorline,
            columns: columns(options),
            case: Case::of(options),
            filled: 0,
            typed: Vec::new(),
            new_step: true,
            changing: None,
            last_change: None,
            insert_times: 1,
            last_find: None,
            prompt: None,
            search: None,
            found: None,
            status: None,
            flash: None,
            held: None,
            rows: Vec::new(),
        }
    }

    pub(crate) fn buffer(&self) -> BufferId {
        self.buffer
    }

    /// Whether a search is being typed, in the bar.
    pub(crate) fn prompting(&self) -> bool {
        self.prompt.is_some()
    }

    pub(crate) fn resize(&mut self, size: Size) {
        self.height = usize::from(size.rows());
        self.width = usize::from(size.cols());
        if self.width < self.gutter + LEAST_TEXT {
            // Until the next settle works out the numbers afresh.
            self.gutter = 0;
        }
    }

    /// Takes in the options, as they've changed.
    pub(crate) fn set_options(&mut self, options: &Options, copy: &BufferCopy) {
        let indent = Indent::of(options);
        if indent.tabstop != self.indent.tabstop {
            // A column is a cell, and tabs take more or fewer of them now:
            // the cursor, the selection and the match found stay on their
            // characters.
            let text = self.text(copy);
            let found = (self.found).map(|(start, last)| (text.byte_at(start), text.byte_at(last)));
            drop(text);
            self.hold(copy);
            self.indent = indent;
            self.place_held(copy);
            let text = self.text(copy);
            self.found = found.map(|(start, last)| (text.point_of(start), text.point_of(last)));
            self.flash = None;
        }
        self.indent = indent;
        self.numbers = Numbers::of(options);
        self.scrolloff = options.scrolloff as usize;
        self.cursorline = options.cursorline;
        self.columns = columns(options);
        self.case = Case::of(options);
        self.follow(copy);
    }

    /// The buffer's text, laid out as the options say.
    fn text<'c>(&self, copy: &'c BufferCopy) -> Text<'c> {
        Text::new(copy.text(), self.indent.tabstop)
    }

    /// Enters normal mode, as moving the focus here from a pane in normal
    /// mode does.
    pub(crate) fn enter_normal_mode(&mut self, copy: &mut BufferCopy) {
        if self.mode == Mode::Insert {
            self.leave_insert(copy);
        }
    }

    /// What the bar says: a search being typed, a message, or the file and
    /// where the cursor is in it.
    pub(crate) fn label(&self, copy: &BufferCopy) -> String {
        if let Some((query, backward)) = &self.prompt {
            return format!("{}{query}▏", if *backward { '?' } else { '/' });
        }
        if let Some(status) = &self.status {
            return status.clone();
        }
        let modified = if copy.modified() { " [+]" } else { "" };
        let at = self.cursor.at;
        format!("{}{modified}  {}:{}", copy.name(), at.line + 1, at.col + 1)
    }

    // --- Keys ---------------------------------------------------------------

    /// Does what `press` does. `register` is shared by every editor.
    pub(crate) fn key(
        &mut self,
        press: &KeyPress<'_>,
        copy: &mut BufferCopy,
        register: &mut Register,
    ) -> Done {
        let strokes = strokes(press, self.mode == Mode::Insert || self.prompt.is_some());
        let mut done = Done::default();
        for stroke in strokes {
            let one = self.stroke(stroke, copy, register);
            done.redraw |= one.redraw;
            done.yanked = one.yanked.or(done.yanked);
        }
        done
    }

    /// Puts `text` in at the cursor, as typed in insert mode.
    pub(crate) fn paste(&mut self, text: &str, copy: &mut BufferCopy) {
        let at = self.insert_byte(copy);
        self.new_step |= self.mode != Mode::Insert;
        self.insert_text(at, text, copy);
        if self.mode != Mode::Insert {
            self.new_step = true;
            self.normal_cursor(copy);
        }
    }

    fn stroke(&mut self, stroke: Stroke, copy: &mut BufferCopy, register: &mut Register) -> Done {
        self.status = None;
        let done = self.read_stroke(stroke, copy, register);
        self.follow(copy);
        done
    }

    fn read_stroke(
        &mut self,
        stroke: Stroke,
        copy: &mut BufferCopy,
        register: &mut Register,
    ) -> Done {
        if self.prompt.is_some() {
            return self.prompt_stroke(stroke, copy);
        }
        if self.mode == Mode::Insert {
            if let Some(change) = &mut self.changing
                && stroke != Stroke::Escape
            {
                change.typed.push(stroke);
            }
            self.insert_stroke(stroke, copy);
            return Done {
                redraw: true,
                yanked: None,
            };
        }
        if stroke == Stroke::Escape && self.typed.is_empty() {
            self.mode = Mode::Normal;
            self.anchor = None;
            self.found = None;
            return Done {
                redraw: true,
                yanked: None,
            };
        }
        self.typed.push(stroke);
        let visual = matches!(self.mode, Mode::Visual(_));
        match keys::parse(&self.typed, visual) {
            Parsed::Incomplete => Done::default(),
            Parsed::Nothing => {
                self.typed.clear();
                Done::default()
            }
            Parsed::Command(command) => {
                self.typed.clear();
                self.execute(command, copy, register, false)
            }
        }
    }

    /// Does `command`; again, for `.`, if `again`.
    fn execute(
        &mut self,
        command: Command,
        copy: &mut BufferCopy,
        register: &mut Register,
        again: bool,
    ) -> Done {
        let mut done = Done {
            redraw: true,
            yanked: None,
        };
        let changes = matches!(
            command.action,
            Action::Operate(
                Operator::Delete | Operator::Change | Operator::Indent | Operator::Dedent,
                _
            ) | Action::DeleteChars { .. }
                | Action::DeleteToEnd { .. }
                | Action::Substitute
                | Action::Put { .. }
                | Action::Join
                | Action::Replace(_)
                | Action::Insert(_)
        );
        let visual = matches!(self.mode, Mode::Visual(_));
        if changes && !again && !visual {
            self.changing = Some(Change {
                command,
                typed: Vec::new(),
            });
        }
        self.new_step = true;
        let times = command.times();
        match command.action {
            Action::Move(motion) => {
                let motion = self.remember_find(motion);
                self.cursor = self.moved(copy, motion, times);
            }
            Action::GoToLine => {
                let text = self.text(copy);
                let line = (times as u64).saturating_sub(1).min(text.last_line());
                self.cursor = Cursor::new(Point { line, col: 0 });
                self.cursor.apply(Motion::FirstNonBlank, &text);
            }
            Action::Operate(operator, target) => {
                done.yanked = self.operate(operator, target, times, copy, register);
            }
            Action::DeleteChars { before } => self.delete_chars(before, times, copy, register),
            Action::DeleteToEnd { change } => self.delete_to_end(change, times, copy, register),
            Action::Substitute => {
                self.delete_chars(false, times, copy, register);
                self.enter_insert(1);
            }
            Action::Put { before } => self.put(before, times, copy, register),
            Action::Join => self.join(times.max(2) - 1, copy),
            Action::Replace(ch) => self.replace_chars(ch, times, copy),
            Action::Insert(entry) => self.insert(entry, times, copy),
            Action::Undo | Action::Redo => {
                for _ in 0..times {
                    copy.reverse(command.action == Action::Undo);
                }
            }
            Action::Repeat => return self.repeat(command.count, copy, register),
            Action::Visual { lines } => self.visual(lines),
            Action::Search { backward } => self.prompt = Some((String::new(), backward)),
            Action::SearchAgain { reverse } => self.search_again(reverse, copy),
            Action::FindAgain { reverse } => {
                if let Some(Motion::Find { ch, forward, till }) = self.last_find {
                    let motion = Motion::Find {
                        ch,
                        forward: forward != reverse,
                        till,
                    };
                    self.cursor = self.moved(copy, motion, times);
                }
            }
            Action::HalfPage(direction) => self.page(i64::from(direction), self.height / 2, copy),
            Action::Page(direction) => {
                self.page(i64::from(direction), self.height.saturating_sub(2), copy);
            }
            Action::View(at) => self.view_at(at),
            Action::OtherEnd => {
                if let Some(anchor) = self.anchor {
                    self.anchor = Some(self.cursor.at);
                    self.cursor = Cursor::new(anchor);
                }
            }
            Action::Select { object, inner } => {
                let text = self.text(copy);
                if let Some((start, last)) = object.find(&text, self.cursor.at, inner) {
                    self.mode = Mode::Visual(Selection::Characters);
                    self.anchor = Some(start);
                    self.cursor = Cursor::new(last);
                }
            }
        }
        // A change done in normal mode is finished; one that went into
        // insert mode finishes when that does.
        if self.mode != Mode::Insert
            && let Some(change) = self.changing.take()
        {
            self.last_change = Some(change);
        }
        if self.mode != Mode::Insert {
            self.normal_cursor(copy);
        }
        done
    }

    /// `.`: makes the last change again, `count` times if given.
    fn repeat(
        &mut self,
        count: Option<usize>,
        copy: &mut BufferCopy,
        register: &mut Register,
    ) -> Done {
        let Some(mut change) = self.last_change.clone() else {
            return Done::default();
        };
        if count.is_some() {
            change.command.count = count;
        }
        let done = self.execute(change.command, copy, register, true);
        if self.mode == Mode::Insert {
            let times = std::mem::replace(&mut self.insert_times, 1);
            for _ in 0..times {
                for &stroke in &change.typed {
                    self.insert_stroke(stroke, copy);
                }
            }
            self.leave_insert(copy);
        }
        self.last_change = Some(change);
        done
    }

    /// Notes `motion` if it's a find, for `;` and `,`.
    fn remember_find(&mut self, motion: Motion) -> Motion {
        if matches!(motion, Motion::Find { .. }) {
            self.last_find = Some(motion);
        }
        motion
    }

    /// The cursor moved by `motion`, `times` times.
    fn moved(&self, copy: &BufferCopy, motion: Motion, times: usize) -> Cursor {
        let text = self.text(copy);
        let mut cursor = self.cursor;
        match motion {
            Motion::Up(_) => cursor.apply(Motion::Up(times as u64), &text),
            Motion::Down(_) => cursor.apply(Motion::Down(times as u64), &text),
            // A tab is several cells, but one character to step over.
            Motion::Left | Motion::Right => {
                for _ in 0..times {
                    let from = text.byte_at(cursor.at);
                    loop {
                        let before = cursor.at;
                        cursor.apply(motion, &text);
                        if cursor.at == before || text.byte_at(cursor.at) != from {
                            break;
                        }
                    }
                }
            }
            _ => {
                for _ in 0..times {
                    cursor.apply(motion, &text);
                }
            }
        }
        cursor
    }

    // --- Operators ----------------------------------------------------------

    /// Makes `operator` act on `target`, keeping the text in the register,
    /// unless it indents. Returns text copied, for the clipboard: what's
    /// deleted goes only to the register, so deleting doesn't lose what
    /// was copied elsewhere.
    fn operate(
        &mut self,
        operator: Operator,
        target: Target,
        times: usize,
        copy: &mut BufferCopy,
        register: &mut Register,
    ) -> Option<String> {
        let span = self.span(operator, target, times, copy)?;
        if let Operator::Indent | Operator::Dedent = operator {
            // A count is how many lines, or for a selection, how many steps.
            let steps = if target == Target::Selection {
                times
            } else {
                1
            };
            self.shift(span, steps, operator == Operator::Dedent, copy);
            return None;
        }
        let mut taken = self.text(copy).slice(span.start, span.end);
        if span.lines.is_some() && !taken.ends_with('\n') {
            taken.push('\n');
        }
        *register = Register {
            text: taken.clone(),
            lines: span.lines.is_some(),
        };
        match operator {
            Operator::Yank => {
                self.yanked(span, copy);
                return Some(taken);
            }
            Operator::Delete => self.delete_span(span, copy),
            Operator::Change => self.change_span(span, copy),
            Operator::Indent | Operator::Dedent => {}
        }
        None
    }

    /// `>` and `<`: indents the lines `span` is on by `steps` of
    /// `shiftwidth`, or takes as much off their indent if `out`, as one
    /// edit. Each indent is written afresh, with tabs or spaces as the line
    /// has them. Empty lines stay empty.
    ///
    /// The cursor goes to the first line's first character that isn't
    /// blank; a selection stays, on the same characters, so `>` can be
    /// pressed again.
    fn shift(&mut self, span: Span, steps: usize, out: bool, copy: &mut BufferCopy) {
        let text = self.text(copy);
        let (first, last) = span.lines.unwrap_or_else(|| {
            let last = span.end.saturating_sub(1).max(span.start);
            (text.point_of(span.start).line, text.point_of(last).line)
        });
        let by = self.indent.step.saturating_mul(steps);
        let tabs = self.indent.tabs_near(&text, first, last);
        let mut edit = Edit::new();
        // How far through the text the edit has gone.
        let mut through = 0;
        for line in first..=last.min(text.last_line()) {
            let line_text = text.line_text(line);
            if line_text.is_empty() {
                continue;
            }
            let old = indent_of(&line_text);
            let width = self.indent.width(old);
            let width = if out {
                width.saturating_sub(by)
            } else {
                width.saturating_add(by).min(MAX_INDENT)
            };
            let new = self.indent.blanks(width, tabs);
            // Only what differs changes, so marks in the rest stay put.
            let same = (old.bytes().zip(new.bytes()))
                .take_while(|(a, b)| a == b)
                .count();
            let start = text.line_start(line);
            edit.retain(start + same - through)
                .delete(&old[same..])
                .insert(&new[same..]);
            through = start + old.len();
        }
        let len = text.rope().len_bytes();
        edit.retain(len - through);
        // Where the selection's ends go, keeping to their characters.
        let anchor = (self.anchor).map(|anchor| edit.map(text.byte_at(anchor), Bias::After));
        let cursor = edit.map(text.byte_at(self.cursor.at), Bias::After);
        drop(text);
        self.make(edit, copy);
        let text = self.text(copy);
        if let Some(anchor) = anchor {
            self.anchor = Some(text.point_of(anchor));
            self.cursor = Cursor::new(text.point_of(cursor));
        } else {
            self.cursor = Cursor::new(Point {
                line: first,
                col: 0,
            });
            self.cursor.apply(Motion::FirstNonBlank, &text);
        }
    }

    /// Highlights `span`, just copied, and moves the cursor to its start
    /// if that's before it.
    fn yanked(&mut self, span: Span, copy: &BufferCopy) {
        let text = self.text(copy);
        let (start, last, kind) = match span.lines {
            Some((first, last)) => (
                Point {
                    line: first,
                    col: 0,
                },
                Point { line: last, col: 0 },
                Selection::Lines,
            ),
            None => (
                text.point_of(span.start),
                text.point_of(span.end.saturating_sub(1)),
                Selection::Characters,
            ),
        };
        self.flash = Some((start, last, kind, Instant::now() + FLASH));
        if start < self.cursor.at {
            let col = self.cursor.at.col;
            self.cursor = match span.lines {
                Some((first, _)) => Cursor::wanting(Point { line: first, col }, col),
                None => Cursor::new(start),
            };
        }
        self.leave_visual();
    }

    fn delete_span(&mut self, span: Span, copy: &mut BufferCopy) {
        let text = self.text(copy);
        let len = text.rope().len_bytes();
        // Whole lines at the end take the line break before them.
        let last_lines = span.lines.is_some()
            && span.end == len
            && span.start > 0
            && !text.slice(span.start, span.end).ends_with('\n');
        let start = if last_lines {
            span.start - 1
        } else {
            span.start
        };
        let edit = Edit::replace(len, start, &text.slice(start, span.end), "");
        drop(text);
        self.make(edit, copy);
        let text = self.text(copy);
        match span.lines {
            Some((first, _)) => {
                self.cursor = Cursor::new(Point {
                    line: first.min(text.last_line()),
                    col: 0,
                });
                self.cursor.apply(Motion::FirstNonBlank, &text);
            }
            None => self.cursor = Cursor::new(text.point_of(start)),
        }
        self.leave_visual();
    }

    fn change_span(&mut self, span: Span, copy: &mut BufferCopy) {
        let text = self.text(copy);
        let (start, end) = match span.lines {
            // Lines keep the first's indent, and one line break.
            Some((first, last)) => {
                let line = text.line_text(first);
                let indent = indent_of(&line).len();
                (text.line_start(first) + indent, text.line_end(last))
            }
            None => (span.start, span.end),
        };
        let edit = Edit::replace(text.rope().len_bytes(), start, &text.slice(start, end), "");
        drop(text);
        self.make(edit, copy);
        self.cursor = Cursor::new(self.text(copy).point_of(start));
        self.anchor = None;
        self.enter_insert(1);
    }

    /// What `operator` acting on `target` acts on.
    fn span(
        &mut self,
        operator: Operator,
        target: Target,
        times: usize,
        copy: &BufferCopy,
    ) -> Option<Span> {
        let text = self.text(copy);
        let at = self.cursor.at;
        let lines = |first: u64, last: u64| Span {
            start: text.line_start(first),
            end: text.line_start(last + 1),
            lines: Some((first, last)),
        };
        let chars = |start: Point, last: Point| Span {
            start: text.byte_at(start),
            end: text.char_end(last),
            lines: None,
        };
        match target {
            Target::Lines => {
                let last = (at.line + times as u64 - 1).min(text.last_line());
                Some(lines(at.line, last))
            }
            Target::Selection => {
                let anchor = self.anchor?;
                let (start, last) = if anchor <= at {
                    (anchor, at)
                } else {
                    (at, anchor)
                };
                match self.mode {
                    Mode::Visual(Selection::Lines) => Some(lines(start.line, last.line)),
                    _ => Some(chars(start, last)),
                }
            }
            Target::Object { object, inner } => {
                let Some((start, last)) = object.find(&text, at, inner) else {
                    self.status = Some(match object {
                        crate::objects::Object::Block => "a file has no shell blocks".into(),
                        _ => "nothing there".into(),
                    });
                    return None;
                };
                Some(chars(start, last))
            }
            Target::Motion(motion) => {
                let motion = self.remember_find(motion);
                // `cw` changes to the end of the word, not the next.
                let word = matches!(motion, Motion::WordStart | Motion::BigWordStart);
                let big = motion == Motion::BigWordStart;
                let row = text.row(at.line);
                let class = |col| {
                    let class = class_at(row.as_deref(), col);
                    if big && class == Class::Punctuation {
                        Class::Word
                    } else {
                        class
                    }
                };
                if operator == Operator::Change && word && class(at.col) != Class::Blank {
                    let end = if big {
                        Motion::BigWordEnd
                    } else {
                        Motion::WordEnd
                    };
                    // On a word's last character, `e` would go on to the
                    // next word's end: that character is the word.
                    let mut next = self.cursor;
                    next.apply(Motion::Right, &text);
                    let at_end = next.at == at || class(next.at.col) != class(at.col);
                    let mut cursor = self.cursor;
                    for time in 0..times {
                        if time > 0 || !at_end {
                            cursor.apply(end, &text);
                        }
                    }
                    return Some(chars(at, cursor.at));
                }
                let moved = self.moved(copy, motion, times);
                let (start, end) = if moved.at < at {
                    (moved.at, at)
                } else {
                    (at, moved.at)
                };
                match motion.reach() {
                    Reach::Lines => Some(lines(start.line, end.line)),
                    Reach::Inclusive => Some(chars(start, end)),
                    Reach::Exclusive => {
                        // `dw` on a line's last word stops at the line's
                        // end.
                        if word && end.line > start.line {
                            return Some(Span {
                                start: text.byte_at(start),
                                end: text.line_end(start.line).max(text.byte_at(start)),
                                lines: None,
                            });
                        }
                        let last = before(&text, start, end)?;
                        Some(chars(start, last))
                    }
                }
            }
        }
    }

    /// `x`, or `X` if `before`: `times` characters at the cursor, or
    /// before it, on its line.
    fn delete_chars(
        &mut self,
        before: bool,
        times: usize,
        copy: &mut BufferCopy,
        register: &mut Register,
    ) {
        let text = self.text(copy);
        let at = self.cursor.at;
        let Some(laid) = text.laid(at.line) else {
            return;
        };
        let line_start = text.line_start(at.line);
        let here = laid.byte_at(at.col);
        let (start, end) = if before {
            let mut start = here;
            for _ in 0..times {
                if start == 0 {
                    break;
                }
                start = laid.byte_at(laid.col_of(start - 1));
            }
            (start, here)
        } else {
            let mut end = here;
            for _ in 0..times {
                if end >= laid.text().len() {
                    break;
                }
                end = laid.char_end(end);
            }
            (here, end)
        };
        if start == end {
            return;
        }
        let removed = laid.text()[start..end].to_owned();
        let edit = Edit::replace(text.rope().len_bytes(), line_start + start, &removed, "");
        drop(text);
        *register = Register {
            text: removed,
            lines: false,
        };
        self.make(edit, copy);
        self.cursor = Cursor::new(self.text(copy).point_of(line_start + start));
    }

    /// `D`, or `C` if `change`: from the cursor to its line's end, and to
    /// the end of `times - 1` lines more.
    fn delete_to_end(
        &mut self,
        change: bool,
        times: usize,
        copy: &mut BufferCopy,
        register: &mut Register,
    ) {
        let text = self.text(copy);
        let at = self.cursor.at;
        let start = text.byte_at(at);
        let last = (at.line + times as u64 - 1).min(text.last_line());
        let end = text.line_end(last).max(start);
        let removed = text.slice(start, end);
        let edit = Edit::replace(text.rope().len_bytes(), start, &removed, "");
        drop(text);
        *register = Register {
            text: removed,
            lines: false,
        };
        self.make(edit, copy);
        self.cursor = Cursor::new(self.text(copy).point_of(start));
        if change {
            self.enter_insert(1);
        }
    }

    /// `p`, or `P` if `before`: what's in the register, `times` over.
    fn put(&mut self, before: bool, times: usize, copy: &mut BufferCopy, register: &Register) {
        if register.text.is_empty() {
            return;
        }
        let text = self.text(copy);
        let len = text.rope().len_bytes();
        let at = self.cursor.at;
        let mut put = register.text.repeat(times);
        let (byte, landing) = if register.lines {
            let line = if before { at.line } else { at.line + 1 };
            let mut byte = text.line_start(line);
            // After a last line with no line break, one goes before.
            if byte == len && len > 0 && text.rope().byte(len - 1) != b'\n' {
                put = format!("\n{}", put.trim_end_matches('\n'));
                byte = len;
                (byte, byte + 1)
            } else {
                (byte, byte)
            }
        } else {
            let empty = text.laid(at.line).is_none_or(|laid| laid.cells() == 0);
            let byte = if before || empty {
                text.byte_at(at)
            } else {
                text.char_end(at)
            };
            (byte, byte + put.len() - 1)
        };
        let edit = Edit::replace(len, byte, "", &put);
        drop(text);
        self.make(edit, copy);
        let text = self.text(copy);
        self.cursor = Cursor::new(text.point_of(landing));
        if register.lines {
            self.cursor.apply(Motion::FirstNonBlank, &text);
        }
    }

    /// `J`: joins the cursor's line with the `times` lines after it, a
    /// space between each, their indents gone.
    fn join(&mut self, times: usize, copy: &mut BufferCopy) {
        for _ in 0..times {
            let text = self.text(copy);
            let line = self.cursor.at.line;
            if line >= text.last_line() {
                break;
            }
            let end = text.line_end(line);
            let next = text.line_text(line + 1);
            let indent = indent_of(&next).len();
            let removed = text.slice(end, text.line_start(line + 1) + indent);
            let here = text.line_text(line);
            let rest = &next[indent..];
            let space = if rest.is_empty() || here.ends_with(' ') || rest.starts_with(')') {
                ""
            } else {
                " "
            };
            let edit = Edit::replace(text.rope().len_bytes(), end, &removed, space);
            drop(text);
            self.make(edit, copy);
            self.cursor = Cursor::new(self.text(copy).point_of(end));
        }
    }

    /// `r`: replaces `times` characters at the cursor with `ch`, if there
    /// are that many on the line.
    fn replace_chars(&mut self, ch: char, times: usize, copy: &mut BufferCopy) {
        let text = self.text(copy);
        let at = self.cursor.at;
        let Some(laid) = text.laid(at.line) else {
            return;
        };
        let start = laid.byte_at(at.col);
        let mut end = start;
        for _ in 0..times {
            if end >= laid.text().len() {
                return;
            }
            end = laid.char_end(end);
        }
        let line_start = text.line_start(at.line);
        let removed = &laid.text()[start..end];
        let with = ch.to_string().repeat(times);
        let edit = Edit::replace(text.rope().len_bytes(), line_start + start, removed, &with);
        drop(text);
        self.make(edit, copy);
        let last = line_start + start + with.len() - ch.len_utf8();
        self.cursor = Cursor::new(self.text(copy).point_of(last));
    }

    // --- Insert mode --------------------------------------------------------

    fn insert(&mut self, entry: Entry, times: usize, copy: &mut BufferCopy) {
        let text = self.text(copy);
        let at = self.cursor.at;
        let laid = text.laid(at.line);
        let cells = laid.as_ref().map_or(0, |laid| laid.cells());
        match entry {
            Entry::Before => {}
            Entry::After => {
                if cells > 0 {
                    self.cursor = Cursor::new(text.point_of(text.char_end(at)));
                }
            }
            Entry::LineStart => self.cursor.apply(Motion::FirstNonBlank, &text),
            Entry::LineEnd => {
                self.cursor = Cursor::new(Point {
                    line: at.line,
                    col: cells,
                });
            }
            Entry::Below | Entry::Above => {
                let line_text = text.line_text(at.line);
                let indent = indent_of(&line_text).to_owned();
                let len = text.rope().len_bytes();
                let (byte, inserted, line) = if entry == Entry::Below {
                    (text.line_end(at.line), format!("\n{indent}"), at.line + 1)
                } else {
                    (text.line_start(at.line), format!("{indent}\n"), at.line)
                };
                let edit = Edit::replace(len, byte, "", &inserted);
                drop(text);
                self.make(edit, copy);
                let text = self.text(copy);
                let col = text.laid(line).map_or(0, |laid| laid.cells());
                self.cursor = Cursor::new(Point { line, col });
            }
        }
        self.enter_insert(times);
    }

    fn enter_insert(&mut self, times: usize) {
        self.mode = Mode::Insert;
        self.anchor = None;
        self.insert_times = times.max(1);
    }

    /// Leaves insert mode, typing what was typed again if a count asked
    /// for it more than once, as `3ix` types `xxx`.
    fn leave_insert(&mut self, copy: &mut BufferCopy) {
        if self.insert_times > 1
            && let Some(change) = self.changing.clone()
        {
            for _ in 1..self.insert_times {
                for &stroke in &change.typed {
                    self.insert_stroke(stroke, copy);
                }
            }
        }
        self.insert_times = 1;
        self.mode = Mode::Normal;
        if let Some(change) = self.changing.take() {
            self.last_change = Some(change);
        }
        self.new_step = true;
        // As in Vim, the cursor steps back onto the character before.
        let mut cursor = self.cursor;
        cursor.apply(Motion::Left, &self.text(copy));
        self.cursor = Cursor::new(cursor.at);
        self.normal_cursor(copy);
    }

    fn insert_stroke(&mut self, stroke: Stroke, copy: &mut BufferCopy) {
        let at = self.insert_byte(copy);
        match stroke {
            Stroke::Escape => self.leave_insert(copy),
            Stroke::Char { ch, ctrl: false } => {
                self.insert_text(at, ch.encode_utf8(&mut [0; 4]), copy);
            }
            Stroke::Enter => {
                let text = self.text(copy);
                let line = text.point_of(at).line;
                let line_text = text.line_text(line);
                let before_cursor = at - text.line_start(line);
                let indent = indent_of(&line_text[..before_cursor.min(line_text.len())]).to_owned();
                drop(text);
                self.insert_text(at, &format!("\n{indent}"), copy);
            }
            Stroke::Tab => {
                let text = self.text(copy);
                let point = text.point_of(at);
                let tabs = self.indent.tabs_near(&text, point.line, point.line);
                drop(text);
                let step = self.indent.step;
                let indent = if tabs {
                    "\t".to_owned()
                } else {
                    " ".repeat(step - point.col % step)
                };
                self.insert_text(at, &indent, copy);
            }
            Stroke::Backspace
            | Stroke::Char {
                ch: 'h',
                ctrl: true,
            } => {
                let text = self.text(copy);
                let start = previous_char(&text, at);
                drop(text);
                self.delete_text(start, at, copy);
            }
            Stroke::Delete => {
                let text = self.text(copy);
                let end = next_char(&text, at);
                drop(text);
                self.delete_text(at, end, copy);
            }
            Stroke::Char {
                ch: 'w',
                ctrl: true,
            } => {
                let start = word_before(&self.text(copy), at);
                self.delete_text(start, at, copy);
            }
            Stroke::Char {
                ch: 'u',
                ctrl: true,
            } => {
                let start = indent_before(&self.text(copy), at);
                self.delete_text(start, at, copy);
            }
            Stroke::Char { ctrl: true, .. } => {}
            Stroke::Left
            | Stroke::Right
            | Stroke::Up
            | Stroke::Down
            | Stroke::Home
            | Stroke::End
            | Stroke::PageUp
            | Stroke::PageDown => {
                // Moving starts a new undo step, and stops what `.` makes
                // again, as in Vim.
                self.new_step = true;
                if let Some(change) = self.changing.take() {
                    self.last_change = Some(change);
                }
                let text = self.text(copy);
                let line = self.cursor.at.line;
                let cells = text.laid(line).map_or(0, |laid| laid.cells());
                match stroke {
                    Stroke::End => self.cursor = Cursor::new(Point { line, col: cells }),
                    Stroke::Right => {
                        let after = next_char(&text, at).min(text.line_end(line));
                        self.cursor = Cursor::new(text.point_of(after));
                    }
                    Stroke::Left => self.cursor.apply(Motion::Left, &text),
                    Stroke::Home => self.cursor.apply(Motion::LineStart, &text),
                    Stroke::Up | Stroke::PageUp => self.cursor.apply(Motion::Up(1), &text),
                    _ => self.cursor.apply(Motion::Down(1), &text),
                }
            }
        }
    }

    /// The byte the cursor is at, to type before.
    fn insert_byte(&self, copy: &BufferCopy) -> usize {
        self.text(copy).byte_at(self.cursor.at)
    }

    fn insert_text(&mut self, at: usize, inserted: &str, copy: &mut BufferCopy) {
        let len = copy.text().len_bytes();
        self.make(Edit::replace(len, at, "", inserted), copy);
        self.cursor = Cursor::new(self.text(copy).point_of(at + inserted.len()));
    }

    fn delete_text(&mut self, start: usize, end: usize, copy: &mut BufferCopy) {
        if start >= end {
            return;
        }
        let text = self.text(copy);
        let removed = text.slice(start, end);
        let edit = Edit::replace(text.rope().len_bytes(), start, &removed, "");
        drop(text);
        self.make(edit, copy);
        self.cursor = Cursor::new(self.text(copy).point_of(start));
    }

    /// Makes `edit` to the buffer, as part of the change being made.
    fn make(&mut self, edit: Edit, copy: &mut BufferCopy) {
        copy.edit(edit, self.new_step);
        self.new_step = false;
    }

    // --- Visual mode, search and the view -----------------------------------

    fn visual(&mut self, lines: bool) {
        let kind = if lines {
            Selection::Lines
        } else {
            Selection::Characters
        };
        match self.mode {
            Mode::Visual(now) if now == kind => self.leave_visual(),
            Mode::Visual(_) => self.mode = Mode::Visual(kind),
            _ => {
                self.mode = Mode::Visual(kind);
                self.anchor = Some(self.cursor.at);
            }
        }
    }

    fn leave_visual(&mut self) {
        if matches!(self.mode, Mode::Visual(_)) {
            self.mode = Mode::Normal;
        }
        self.anchor = None;
    }

    fn prompt_stroke(&mut self, stroke: Stroke, copy: &BufferCopy) -> Done {
        let Some((query, backward)) = &mut self.prompt else {
            return Done::default();
        };
        match stroke {
            Stroke::Escape => self.prompt = None,
            Stroke::Backspace => {
                if query.pop().is_none() {
                    self.prompt = None;
                }
            }
            Stroke::Enter => {
                let search = (std::mem::take(query), *backward);
                self.prompt = None;
                if !search.0.is_empty() {
                    self.search = Some(search);
                }
                self.search_again(false, copy);
            }
            Stroke::Char { ch, ctrl: false } => query.push(ch),
            _ => {}
        }
        Done {
            redraw: true,
            yanked: None,
        }
    }

    /// `n`, or `N` if `reverse`: the next match of the last search.
    fn search_again(&mut self, reverse: bool, copy: &BufferCopy) {
        let Some((query, backward)) = self.search.clone() else {
            self.status = Some("no search yet".into());
            return;
        };
        let text = self.text(copy);
        let from = text.byte_at(self.cursor.at);
        let whole = text.slice(0, text.rope().len_bytes());
        let exact = self.case.exact(&query);
        if let Some(start) = find(&whole, &query, from, backward != reverse, exact) {
            let last = start + query.len() - 1;
            self.found = Some((text.point_of(start), text.point_of(last)));
            self.cursor = Cursor::new(text.point_of(start));
        } else {
            self.found = None;
            self.status = Some(format!("{query} isn't in the file"));
        }
    }

    /// Ctrl+D and the like: `lines` lines down, or up if `direction` is
    /// negative, moving the view and the cursor together.
    fn page(&mut self, direction: i64, lines: usize, copy: &BufferCopy) {
        let lines = lines.max(1) as u64;
        let text = self.text(copy);
        let last = text.last_line();
        if direction > 0 {
            self.top = (self.top + lines).min(self.lowest_top(last));
            self.cursor.apply(Motion::Down(lines), &text);
        } else {
            self.top = self.top.saturating_sub(lines);
            self.cursor.apply(Motion::Up(lines), &text);
        }
        self.cursor_into_view(&text);
    }

    fn view_at(&mut self, at: ViewAt) {
        let line = self.cursor.at.line;
        let height = self.height.max(1) as u64;
        self.top = match at {
            ViewAt::Top => line,
            ViewAt::Middle => line.saturating_sub(height / 2),
            ViewAt::Bottom => line.saturating_sub(height - 1),
        };
    }

    // --- Following the text and the cursor ----------------------------------

    /// In normal or visual mode, puts the cursor on a character: back from
    /// past the line's end, and onto the start of a wide or tab character.
    fn normal_cursor(&mut self, copy: &BufferCopy) {
        let text = self.text(copy);
        let at = &mut self.cursor.at;
        at.line = at.line.min(text.last_line());
        let laid = text.laid(at.line);
        let cells = laid.as_ref().map_or(0, |laid| laid.cells());
        if cells == 0 {
            at.col = 0;
        } else if let Some(laid) = laid {
            at.col = laid.col_of(laid.byte_at(at.col.min(cells - 1)));
        }
        if let Some(anchor) = &mut self.anchor {
            anchor.line = anchor.line.min(text.last_line());
        }
    }

    /// Notes where the cursor is in the text, before an edit from
    /// elsewhere changes it: see [`follow_edit`](Self::follow_edit).
    pub(crate) fn hold(&mut self, copy: &BufferCopy) {
        let text = self.text(copy);
        let cursor = text.byte_at(self.cursor.at);
        let anchor = self.anchor.map(|anchor| text.byte_at(anchor));
        self.held = Some((cursor, anchor));
    }

    /// Moves the cursor with the text around it, through `edit`, made
    /// since [`hold`](Self::hold).
    pub(crate) fn follow_edit(&mut self, edit: &Edit, copy: &BufferCopy) {
        if let Some((cursor, anchor)) = &mut self.held {
            *cursor = edit.map(*cursor, Bias::Before);
            if let Some(anchor) = anchor {
                *anchor = edit.map(*anchor, Bias::Before);
            }
        }
        self.place_held(copy);
    }

    /// Takes in what an update from the daemon did: `jump` if this pane
    /// asked for the undo or redo it made.
    pub(crate) fn heard(&mut self, heard: &Heard, jump: bool, copy: &BufferCopy) {
        match heard {
            Heard::Nothing => {}
            Heard::Replaced => {
                self.held = None;
                self.normal_cursor(copy);
            }
            Heard::Edited(edit) => self.follow_edit(edit, copy),
            Heard::Reversed(edit) => {
                if jump {
                    // To where the step undone or redone was, as in Vim.
                    let at = edit.changes().next().map(|(at, _, _)| at);
                    if let (Some(at), Some((cursor, _))) = (at, &mut self.held) {
                        *cursor = at;
                    }
                    self.place_held(copy);
                } else {
                    self.follow_edit(edit, copy);
                }
            }
            Heard::NothingToReverse { undo } => {
                if jump {
                    self.status = Some(if *undo {
                        "already at the oldest change".into()
                    } else {
                        "already at the newest change".into()
                    });
                }
            }
        }
        if self.mode != Mode::Insert {
            self.normal_cursor(copy);
        }
        self.follow(copy);
    }

    fn place_held(&mut self, copy: &BufferCopy) {
        let Some((cursor, anchor)) = self.held.take() else {
            return;
        };
        let text = self.text(copy);
        self.cursor = Cursor::new(text.point_of(cursor));
        self.anchor = anchor.map(|anchor| text.point_of(anchor));
    }

    /// The lines kept in view above and below the cursor: `scrolloff`, but
    /// no more than will fit, so a big one keeps the cursor in the middle.
    fn margin(&self) -> u64 {
        let height = self.height.max(1) as u64;
        (self.scrolloff as u64).min((height - 1) / 2)
    }

    /// Scrolls the view to keep the cursor in it, with `scrolloff` lines
    /// above and below it, short of the file's ends.
    fn follow(&mut self, copy: &BufferCopy) {
        let height = self.height.max(1) as u64;
        let line = self.cursor.at.line;
        let margin = self.margin();
        let text = self.text(copy);
        let last = text.last_line();
        if line < self.top + margin {
            self.top = line.saturating_sub(margin);
        } else {
            // Not so far as to show past the file's end, unless the cursor
            // is on the line after it, being typed on.
            let below = (line + margin).min(last.max(line));
            if below >= self.top + height {
                self.top = below + 1 - height;
            }
        }
        self.top = self.top.min(last);
        self.gutter = self.gutter_for(text.lines());
        let col = self.cursor.at.col;
        let width = self.text_width().max(1);
        if col < self.left {
            self.left = col;
        } else if col >= self.left + width {
            self.left = col + 1 - width;
        }
    }

    /// Scrolls the view by `lines`, up if negative, as the mouse wheel does,
    /// keeping the cursor in it.
    pub(crate) fn wheel(&mut self, lines: i64, copy: &BufferCopy) {
        let text = self.text(copy);
        let last = text.last_line();
        self.top = if lines > 0 {
            self.top.saturating_sub(lines.unsigned_abs())
        } else {
            (self.top + lines.unsigned_abs()).min(self.lowest_top(last))
        };
        self.cursor_into_view(&text);
    }

    /// The furthest down the view scrolls, in a text whose last line is
    /// `last`: as far as leaves `scrolloff` lines above the last, which the
    /// cursor can then sit on without the view jumping back to show them.
    fn lowest_top(&self, last: u64) -> u64 {
        last.saturating_sub(self.margin())
    }

    /// Moves the cursor into the view where it is, `scrolloff` lines from
    /// its top and bottom unless that's the file's start or end, as
    /// scrolling the view does. So the next key doesn't scroll it back.
    fn cursor_into_view(&mut self, text: &Text<'_>) {
        let last = text.last_line();
        let margin = self.margin();
        let bottom = self.top + self.height.max(1) as u64 - 1;
        let highest = if self.top == 0 { 0 } else { self.top + margin };
        let lowest = if bottom >= last {
            last
        } else {
            bottom - margin
        };
        // Past the end, with only a few lines in view, the last.
        let line = self.cursor.at.line.max(highest).min(lowest);
        if line != self.cursor.at.line {
            let mut cursor = self.cursor;
            if line > cursor.at.line {
                cursor.apply(Motion::Down(line - cursor.at.line), text);
            } else {
                cursor.apply(Motion::Up(cursor.at.line - line), text);
            }
            self.cursor = cursor;
        }
    }

    /// Puts the cursor at `line` and `col` of the pane, as a click does. A
    /// click on a line's number goes to the line's start.
    pub(crate) fn click(&mut self, line: usize, col: usize, copy: &BufferCopy) {
        let text = self.text(copy);
        let line = (self.top + line as u64).min(text.last_line());
        let col = match col.checked_sub(self.gutter) {
            Some(col) => self.left + col,
            None => 0,
        };
        self.cursor = Cursor::new(Point { line, col });
        drop(text);
        if self.mode == Mode::Insert {
            let byte = self.insert_byte(copy);
            self.cursor = Cursor::new(self.text(copy).point_of(byte));
        } else {
            self.normal_cursor(copy);
        }
        // Near the view's edge, it scrolls to keep `scrolloff` lines in
        // view, as in Vim, rather than on the next key.
        self.follow(copy);
    }

    // --- Drawing ------------------------------------------------------------

    /// The cells line numbers take for a text of `lines` lines: the widest
    /// number and a space, at least `numberwidth`, or none if they're off
    /// or would leave the text too little room.
    fn gutter_for(&self, lines: u64) -> usize {
        let Numbers {
            absolute,
            relative,
            least,
        } = self.numbers;
        if !absolute && !relative {
            return 0;
        }
        let widest = if absolute {
            // The line after the last line break is numbered too, while
            // it's typed on.
            lines.max(self.cursor.at.line + 1)
        } else {
            // Distances from the cursor, which stays in view.
            self.height.max(1) as u64
        };
        let gutter = (widest.ilog10() as usize + 2).max(least);
        if self.width >= gutter + LEAST_TEXT {
            gutter
        } else {
            0
        }
    }

    /// The cells the text is shown in, right of the line numbers.
    fn text_width(&self) -> usize {
        self.width.saturating_sub(self.gutter)
    }

    /// Lays out the lines shown, for [`scene`](Self::scene).
    pub(crate) fn settle(&mut self, copy: &BufferCopy) {
        let text = self.text(copy);
        let lines = text.lines();
        self.gutter = self.gutter_for(lines);
        let cursor = self.cursor.at.line;
        let end = if cursor == lines { lines + 1 } else { lines };
        self.filled = usize::try_from(end.saturating_sub(self.top))
            .map_or(self.height, |filled| filled.min(self.height));
        self.rows = (0..self.height as u64)
            .map(|offset| {
                let line = self.top + offset;
                let mut builder = RowBuilder::new(self.width);
                // The line after the last line break, while typed on.
                let typing_after = line == lines && cursor == lines;
                if line < lines || typing_after {
                    if self.gutter > 0 {
                        let number = self.numbers.label(line, cursor);
                        let number = format!("{number:>0$} ", self.gutter - 1);
                        builder.push(&number, &colors::line_number(line == cursor), true);
                    }
                    let laid = Laid::new(text.line_text(line), self.indent.tabstop);
                    let (start, end) = (text.line_start(line), text.line_end(line));
                    let spans = copy.highlights().within(start, end);
                    laid.show(&mut builder, self.left, self.text_width(), spans, start);
                } else if copy.loaded() {
                    builder.push("~", &Style::DEFAULT, true);
                }
                builder.finish(false)
            })
            .collect();
    }

    /// The rows to draw, and the cursor.
    pub(crate) fn scene(&self) -> (Vec<&Row>, Option<SceneCursor>) {
        let rows = self.rows.iter().collect();
        let at = self.cursor.at;
        let shown = at.line >= self.top
            && at.line < self.top + self.height as u64
            && at.col >= self.left
            && at.col < self.left + self.text_width();
        let cursor = shown.then(|| SceneCursor {
            line: usize::try_from(at.line - self.top).unwrap_or(0),
            col: self.gutter + at.col - self.left,
            style: if self.mode == Mode::Insert {
                CursorStyle::Bar
            } else {
                CursorStyle::Block
            },
        });
        (rows, cursor)
    }

    /// When the highlight on text just copied goes.
    pub(crate) fn flash_ends(&self) -> Option<Instant> {
        self.flash.map(|(_, _, _, until)| until)
    }

    /// What's highlighted in view: the cursor's line and the columns
    /// marked, as the options say, the selection, the match found, and
    /// text just copied.
    pub(crate) fn highlights(&self) -> Vec<Highlight> {
        let mut highlights = Vec::new();
        if self.cursorline
            && let Some(line) = (self.cursor.at.line.checked_sub(self.top))
                .and_then(|line| usize::try_from(line).ok())
                .filter(|&line| line < self.height)
        {
            highlights.push(Highlight {
                line,
                cols: self.gutter..self.width,
                kind: HighlightKind::CursorLine,
            });
        }
        // Down the file's lines, not the rows past its end, as in Vim.
        for &column in &self.columns {
            if let Some(col) =
                (column.checked_sub(self.left)).filter(|&col| col < self.text_width())
            {
                let col = self.gutter + col;
                highlights.extend((0..self.filled).map(|line| Highlight {
                    line,
                    cols: col..col + 1,
                    kind: HighlightKind::ColorColumn,
                }));
            }
        }
        if let (Some(anchor), Mode::Visual(kind)) = (self.anchor, self.mode) {
            let (start, last) = if anchor <= self.cursor.at {
                (anchor, self.cursor.at)
            } else {
                (self.cursor.at, anchor)
            };
            self.highlight(start, last, kind, HighlightKind::Selection, &mut highlights);
        }
        if let Some((start, last)) = self.found {
            self.highlight(
                start,
                last,
                Selection::Characters,
                HighlightKind::Match,
                &mut highlights,
            );
        }
        if let Some((start, last, kind, until)) = self.flash
            && Instant::now() < until
        {
            self.highlight(start, last, kind, HighlightKind::Copied, &mut highlights);
        }
        highlights
    }

    fn highlight(
        &self,
        start: Point,
        last: Point,
        kind: Selection,
        why: HighlightKind,
        into: &mut Vec<Highlight>,
    ) {
        let bottom = self.top + self.height as u64;
        for line in start.line.max(self.top)..=last.line.min(bottom.saturating_sub(1)) {
            let (from, to) = match kind {
                Selection::Lines => (0, usize::MAX),
                Selection::Characters => (
                    if line == start.line { start.col } else { 0 },
                    if line == last.line {
                        last.col + 1
                    } else {
                        usize::MAX
                    },
                ),
            };
            let from = from.saturating_sub(self.left);
            let to = to.saturating_sub(self.left).min(self.text_width());
            if from < to {
                into.push(Highlight {
                    line: usize::try_from(line - self.top).unwrap_or(0),
                    cols: self.gutter + from..self.gutter + to,
                    kind: why,
                });
            }
        }
    }
}

/// The strokes `press` makes: the text it types, in insert mode or a
/// search, or the key.
fn strokes(press: &KeyPress<'_>, typing: bool) -> Vec<Stroke> {
    if enters_normal_mode(press) {
        return vec![Stroke::Escape];
    }
    let modifiers = press.modifiers;
    if modifiers.alt {
        return Vec::new();
    }
    let named = match press.key {
        Key::Named(NamedKey::Escape) => Some(Stroke::Escape),
        Key::Named(NamedKey::Enter) => Some(Stroke::Enter),
        Key::Named(NamedKey::Tab) => Some(Stroke::Tab),
        Key::Named(NamedKey::Backspace) => Some(Stroke::Backspace),
        Key::Named(NamedKey::Delete) => Some(Stroke::Delete),
        Key::Named(NamedKey::ArrowLeft) => Some(Stroke::Left),
        Key::Named(NamedKey::ArrowRight) => Some(Stroke::Right),
        Key::Named(NamedKey::ArrowUp) => Some(Stroke::Up),
        Key::Named(NamedKey::ArrowDown) => Some(Stroke::Down),
        Key::Named(NamedKey::Home) => Some(Stroke::Home),
        Key::Named(NamedKey::End) => Some(Stroke::End),
        Key::Named(NamedKey::PageUp) => Some(Stroke::PageUp),
        Key::Named(NamedKey::PageDown) => Some(Stroke::PageDown),
        Key::Named(NamedKey::Space) if !modifiers.ctrl => Some(Stroke::char(' ')),
        _ => None,
    };
    if let Some(named) = named {
        return vec![named];
    }
    let Key::Character(key) = press.key else {
        return Vec::new();
    };
    if modifiers.ctrl {
        return key.chars().next().map(Stroke::ctrl).into_iter().collect();
    }
    let text = if typing {
        press.text.unwrap_or(key)
    } else {
        key
    };
    text.chars()
        .filter(|ch| !ch.is_control())
        .map(Stroke::char)
        .collect()
}

/// Where the word before `byte` starts, on its line, taking the blanks
/// after it too, as Ctrl+W deletes back to in insert mode.
fn word_before(text: &Text<'_>, byte: usize) -> usize {
    let line_start = text.line_start(text.point_of(byte).line);
    let before = |at: usize| text.slice(previous_char(text, at), at);
    let blank = |at: usize| before(at).chars().all(char::is_whitespace);
    let word = |at: usize| (before(at).chars()).all(|ch| ch.is_alphanumeric() || ch == '_');
    let mut start = byte;
    while start > line_start && blank(start) {
        start = previous_char(text, start);
    }
    let kind = start > line_start && word(start);
    while start > line_start && word(start) == kind && !blank(start) {
        start = previous_char(text, start);
    }
    if start == byte {
        previous_char(text, byte)
    } else {
        start
    }
}

/// Where Ctrl+U deletes back to from `byte`: the end of the line's indent,
/// or from there, the line's start.
fn indent_before(text: &Text<'_>, byte: usize) -> usize {
    let line = text.point_of(byte).line;
    let start = text.line_start(line);
    let indent = start + indent_of(&text.line_text(line)).len();
    if byte > indent { indent } else { start }
}

/// The blanks a line starts with.
fn indent_of(line: &str) -> &str {
    let end = line.len() - line.trim_start_matches([' ', '\t']).len();
    &line[..end]
}

/// The byte of the character before `byte`, or 0.
fn previous_char(text: &Text<'_>, byte: usize) -> usize {
    if byte == 0 {
        return 0;
    }
    let rope = text.rope();
    rope.char_to_byte(rope.byte_to_char(byte) - 1)
}

/// The byte after the character at `byte`, or the text's end.
fn next_char(text: &Text<'_>, byte: usize) -> usize {
    let rope = text.rope();
    if byte >= rope.len_bytes() {
        return rope.len_bytes();
    }
    rope.char_to_byte(rope.byte_to_char(byte) + 1)
}

/// The columns `colorcolumn` marks, counting from 0.
fn columns(options: &Options) -> Vec<usize> {
    (options.colorcolumn.iter())
        .map(|&column| (column as usize).saturating_sub(1))
        .collect()
}

/// Where `query` next is in `text` after `from`, or before it if
/// `backward`, going round the ends. Case is ignored, for ASCII, unless
/// `exact`.
fn find(text: &str, query: &str, from: usize, backward: bool, exact: bool) -> Option<usize> {
    if query.is_empty() {
        return None;
    }
    let (haystack, needle) = if exact {
        (
            std::borrow::Cow::Borrowed(text),
            std::borrow::Cow::Borrowed(query),
        )
    } else {
        (
            std::borrow::Cow::Owned(text.to_ascii_lowercase()),
            std::borrow::Cow::Owned(query.to_ascii_lowercase()),
        )
    };
    let all: Vec<usize> = haystack.match_indices(&*needle).map(|(at, _)| at).collect();
    if backward {
        all.iter()
            .rev()
            .find(|&&at| at < from)
            .or(all.last())
            .copied()
    } else {
        all.iter().find(|&&at| at > from).or(all.first()).copied()
    }
}

#[cfg(test)]
mod tests;
