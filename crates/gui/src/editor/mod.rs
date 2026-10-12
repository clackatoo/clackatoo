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
//!
//! In insert mode, the language server is asked what could finish the
//! word being typed, shown in a menu under the cursor (see [`complete`]),
//! and about the call whose arguments are being typed, whose signature
//! shows over it. Typing never waits for either: they're asked when typing
//! pauses, and their answers are shown when they come, if they're still
//! about where the cursor is.

mod complete;
mod copy;
mod keys;
mod panel;
mod text;

use std::time::{Duration, Instant};

use clackatoo_protocol::{Answer, BufferId, Edit, FileLocation, Query, Size, Trigger};
use clackatoo_term::{Row, RowBuilder, Style};
use clackatoo_text::{Bias, Diagnostic, Hunk, Severity};
use winit::keyboard::{Key, NamedKey};

pub use complete::Ranking;
use complete::{Completion, Resolved, is_word};
pub(crate) use copy::{BufferCopy, Heard};
use keys::{Action, Command, Entry, Operator, Parsed, Stroke, Target, ViewAt};
use panel::Panel;
pub(crate) use panel::PanelView;
use text::{Laid, Text, tab_end};

use crate::colors;
use crate::input::KeyPress;
use crate::motion::{Class, Cursor, Lines, Motion, Point, Reach, before, class_at};
use crate::options::{Case, CompleteAccept, Options};
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

/// What the bar says when there are no hunks to go to...
const NO_CHANGES: &str = "nothing's changed since git's index";

/// ...and when there's none at the cursor's line.
const NO_CHANGE_HERE: &str = "nothing's changed here since git's index";

/// What's done to the hunk at an editor's cursor, by the window's keys and
/// commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HunkAction {
    /// To the next hunk, or the one before, as `]c` and `[c` go.
    Next { forward: bool },
    /// Show its lines as they were, and as they are, by the cursor.
    Preview,
    /// Put its lines back as they are in git's index.
    Reset,
    /// Put its lines in git's index.
    Stage,
}

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
    typed: Vec<Typed>,
}

/// Something typed in insert mode, for `.` to type again.
#[derive(Clone, Debug)]
enum Typed {
    /// A key.
    Key(Stroke),
    /// A completion accepted: `replaced` characters before the cursor
    /// replaced with `text`, and the cursor `offset` bytes into it.
    Accepted {
        text: String,
        offset: usize,
        replaced: usize,
    },
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

/// How completion and help with calls behave, as the options say.
#[derive(Clone, Copy, Debug)]
struct AsYouType {
    /// Typing a word, or a character the server says should, asks what
    /// could be typed (`completion`).
    auto: bool,
    /// How long typing a word pauses before it asks (`completedelay`).
    delay: Duration,
    /// Which keys accept an item, besides Ctrl+Y (`completeaccept`)...
    accept: CompleteAccept,
    /// ...and whether Tab does (`completetab`).
    tab: bool,
    /// A call's signature is shown as its arguments are typed
    /// (`signaturehelp`).
    signature: bool,
}

impl AsYouType {
    fn of(options: &Options) -> Self {
        Self {
            auto: options.completion,
            delay: Duration::from_millis(u64::from(options.completedelay)),
            accept: options.completeaccept,
            tab: options.completetab,
            signature: options.signaturehelp,
        }
    }
}

/// A completion query asked, until it's answered.
#[derive(Clone, Copy, Debug)]
struct Requested {
    id: u64,
    /// Where the word it's for starts, moved along with every edit.
    start: usize,
    /// Whether it was asked for with a key, so an answer of nothing is
    /// worth saying.
    explicit: bool,
    /// Whether it asks again for what the menu shows.
    again: bool,
    /// How long the word was, in bytes.
    len: usize,
}

/// What's being typed in the bar.
#[derive(Debug)]
enum Prompt {
    /// A search, backward if `backward`.
    Search { query: String, backward: bool },
    /// A new name for what's under the cursor, which was `was`.
    Rename { name: String, was: String },
}

impl Prompt {
    /// A search being started, backward if `backward`.
    fn search(backward: bool) -> Self {
        Self::Search {
            query: String::new(),
            backward,
        }
    }
}

/// What the language server was last asked, until it answers. A rename
/// is kept apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asking {
    /// Where to go, as `gd` asks.
    Jump,
    /// What's at this place, as `K` asks: the answer's shown only if the
    /// cursor's still there.
    Hover(Point),
}

/// A pane editing a buffer.
#[derive(Debug)]
// Separate settings from the options, not states of one machine.
#[allow(clippy::struct_excessive_bools)]
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
    /// The cells the signs and line numbers take, at the pane's left, out
    /// of `width`.
    gutter: usize,
    /// How lines are indented, and tabs drawn.
    indent: Indent,
    /// The fewest lines kept in view above and below the cursor
    /// (`scrolloff`).
    scrolloff: usize,
    /// The cursor's line is lit (`cursorline`).
    cursorline: bool,
    /// What's wrong with a line is written after it (`virtualtext`).
    virtual_text: bool,
    /// A column of signs says which lines have changed (`gitsigns`).
    signs: bool,
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
    /// What's being typed in the bar.
    prompt: Option<Prompt>,
    /// The last search.
    search: Option<Searched>,
    /// The last match found, highlighted until Esc.
    found: Option<(Point, Point)>,
    /// A short message for the bar, until the next key.
    status: Option<String>,
    /// The number of the language server query asked last, and what it
    /// asked, until it's answered.
    asked: Option<(u64, Asking)>,
    /// The number of the rename asked last, until it's answered: a rename
    /// takes a while, and `K` or `gd` meanwhile mustn't lose its answer.
    renaming: Option<u64>,
    /// The numbers of the stages asked for, until each is answered.
    staging: Vec<u64>,
    /// What the language server says is at the cursor, with what's wrong
    /// there, shown by it.
    panel: Option<Panel>,
    /// A place in another file an answer goes to, for the desk to open.
    opening: Option<FileLocation>,
    /// The places an answer gave, when it gave several, for the desk to
    /// open the finder on.
    places: Option<Vec<FileLocation>>,
    /// The byte the cursor left, on a jump such as `G` or a search, for
    /// the desk's jump list.
    jumped_from: Option<usize>,
    /// Text just copied, highlighted for a moment.
    flash: Option<(Point, Point, Selection, Instant)>,
    /// Where the cursor and selection's start were in the text, before
    /// an edit someone else made.
    held: Option<(usize, Option<usize>)>,
    /// The lines shown, laid out.
    rows: Vec<Row>,
    /// How completion and help with calls behave.
    as_you_type: AsYouType,
    /// The word before the cursor being completed, once the language
    /// server has said what could finish it.
    completion: Option<Completion>,
    /// The completion query asked last, until it's answered.
    completing: Option<Requested>,
    /// A completion query to ask once typing pauses: when, and why.
    completion_due: Option<(Instant, Trigger)>,
    /// The signature of the call whose arguments are being typed, shown
    /// over the cursor.
    signature: Option<Panel>,
    /// The number of the signature help query asked last, until it's
    /// answered.
    signing: Option<u64>,
    /// When to ask about the call again, once typing pauses, while its
    /// signature is shown.
    signature_due: Option<Instant>,
    /// When to ask for the picked item's documentation, once picking
    /// pauses.
    resolve_due: Option<Instant>,
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
            virtual_text: options.virtualtext,
            signs: options.gitsigns,
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
            asked: None,
            renaming: None,
            staging: Vec::new(),
            panel: None,
            opening: None,
            jumped_from: None,
            places: None,
            flash: None,
            held: None,
            rows: Vec::new(),
            as_you_type: AsYouType::of(options),
            completion: None,
            completing: None,
            completion_due: None,
            signature: None,
            signing: None,
            signature_due: None,
            resolve_due: None,
        }
    }

    pub(crate) fn buffer(&self) -> BufferId {
        self.buffer
    }

    /// Whether a search or a new name is being typed, in the bar.
    pub(crate) fn prompting(&self) -> bool {
        self.prompt.is_some()
    }

    /// Whether a command is half typed, such as `d` waiting for its
    /// motion, which the next key is for.
    pub(crate) fn pending(&self) -> bool {
        !self.typed.is_empty()
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
        self.virtual_text = options.virtualtext;
        self.signs = options.gitsigns;
        self.columns = columns(options);
        self.case = Case::of(options);
        self.as_you_type = AsYouType::of(options);
        if !self.as_you_type.signature {
            self.close_signature();
        }
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

    /// What the bar says: a search or a name being typed, a message, or
    /// the file and where the cursor is in it.
    pub(crate) fn label(&self, copy: &BufferCopy) -> String {
        match &self.prompt {
            Some(Prompt::Search { query, backward }) => {
                return format!("{}{query}▏", if *backward { '?' } else { '/' });
            }
            Some(Prompt::Rename { name, .. }) => return format!("rename to: {name}▏"),
            None => {}
        }
        if let Some(status) = &self.status {
            return status.clone();
        }
        let modified = if copy.modified() { " [+]" } else { "" };
        let at = self.cursor.at;
        let label = format!("{}{modified}  {}:{}", copy.name(), at.line + 1, at.col + 1);
        match self.diagnostic_here(copy) {
            Some(diagnostic) => format!("{label}  {}", describe(diagnostic)),
            None => label,
        }
    }

    /// The diagnostic the bar tells of: the one under the cursor, or else
    /// the one on its line that matters most.
    fn diagnostic_here<'a>(&self, copy: &'a BufferCopy) -> Option<&'a Diagnostic> {
        if copy.diagnostics().all().is_empty() {
            return None;
        }
        let text = self.text(copy);
        let line = self.cursor.at.line;
        let (start, end) = (text.line_start(line), text.line_end(line));
        let at = text.byte_at(self.cursor.at);
        let on_line = copy.diagnostics().within(start, end);
        let mut under = None;
        let mut most: Option<&Diagnostic> = None;
        for diagnostic in on_line {
            if under.is_none()
                && diagnostic.start <= at
                && at < diagnostic.end.max(diagnostic.start + 1)
            {
                under = Some(diagnostic);
            }
            if most.is_none_or(|most| diagnostic.severity < most.severity) {
                most = Some(diagnostic);
            }
        }
        under.or(most)
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
        self.panel = None;
        self.close_completion();
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
        if self.panel.is_some()
            && self.typed.is_empty()
            && self.prompt.is_none()
            && let Some(done) = self.panel_stroke(stroke)
        {
            return done;
        }
        if self.mode == Mode::Insert
            && self.prompt.is_none()
            && let Some(done) = self.completion_stroke(stroke, copy)
        {
            self.follow(copy);
            return done;
        }
        let (mode, edits) = (self.mode, copy.edits_made());
        let at = self.cursor_byte(copy);
        let done = self.read_stroke(stroke, copy, register);
        self.follow(copy);
        if mode == Mode::Insert {
            let changed = copy.edits_made() != edits || self.cursor_byte(copy) != at;
            self.as_you_type(stroke, changed, copy);
        }
        // Moving the cursor, changing mode, editing or typing in the bar
        // closes the panel.
        if let Some(panel) = &self.panel
            && (panel.anchor != self.cursor.at
                || self.mode != mode
                || copy.edits_made() != edits
                || self.prompt.is_some())
        {
            self.panel = None;
        }
        done
    }

    /// The keys a panel by the cursor takes while it's shown: `K` again
    /// pages through it, and Ctrl+E and Ctrl+Y scroll it a line. `None`
    /// for any other key, which closes it if it moves the cursor. Esc
    /// closes it, and goes on to do what it does, such as leaving visual
    /// mode.
    fn panel_stroke(&mut self, stroke: Stroke) -> Option<Done> {
        let panel = self.panel.as_mut()?;
        match stroke {
            Stroke::Char {
                ch: 'K',
                ctrl: false,
            } => panel.page(),
            Stroke::Char {
                ch: 'e',
                ctrl: true,
            } => {
                panel.scroll(1);
            }
            Stroke::Char {
                ch: 'y',
                ctrl: true,
            } => {
                panel.scroll(-1);
            }
            Stroke::Escape => {
                self.panel = None;
                return None;
            }
            _ => return None,
        }
        Some(Done {
            redraw: true,
            yanked: None,
        })
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
                change.typed.push(Typed::Key(stroke));
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
        let visual = matches!(self.mode, Mode::Visual(_));
        if command.action.changes() && !again && !visual {
            self.changing = Some(Change {
                command,
                typed: Vec::new(),
            });
        }
        self.new_step = true;
        let times = command.times();
        let from = self.cursor.at;
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
            Action::Search { backward } => self.prompt = Some(Prompt::search(backward)),
            Action::SearchAgain { reverse } => self.search_again(reverse, copy),
            Action::SearchWord { backward } => self.search_word(backward, copy),
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
            Action::Diagnostic { backward } => self.next_diagnostic(backward, times, copy),
            Action::Hunk { backward } => self.next_hunk(backward, times, copy),
            Action::Ask(_) | Action::Hover | Action::ShowDiagnostics | Action::Rename => {
                self.language_server(command.action, copy);
            }
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
        if command.action.jumps() && self.cursor.at != from {
            self.left_from(from, copy);
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
                for typed in &change.typed {
                    self.retype(typed, copy);
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
                match motion.reach_from(at, moved.at, &text)? {
                    Reach::Lines => Some(lines(start.line, end.line)),
                    Reach::Inclusive => Some(chars(start, end)),
                    Reach::Exclusive => exclusive(&text, start, end, word),
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
        self.close_completion();
        self.close_signature();
        if self.insert_times > 1
            && let Some(change) = self.changing.clone()
        {
            for _ in 1..self.insert_times {
                for typed in &change.typed {
                    self.retype(typed, copy);
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
                self.moved_while_inserting();
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

    /// Types `typed` again, as `.` and a count do.
    fn retype(&mut self, typed: &Typed, copy: &mut BufferCopy) {
        match typed {
            Typed::Key(stroke) => self.insert_stroke(*stroke, copy),
            Typed::Accepted {
                text,
                offset,
                replaced,
            } => {
                let at = self.insert_byte(copy);
                let start = {
                    let rope = copy.text();
                    let char_at = rope.byte_to_char(at);
                    rope.char_to_byte(char_at.saturating_sub(*replaced))
                };
                let removed = copy.text().byte_slice(start..at).to_string();
                let edit = Edit::replace(copy.text().len_bytes(), start, &removed, text);
                self.make(edit, copy);
                self.cursor = Cursor::new(self.text(copy).point_of(start + offset));
            }
        }
    }

    /// The cursor moved in insert mode other than by typing: what's typed
    /// next starts a new undo step, and isn't part of what `.` makes
    /// again, as in Vim.
    fn moved_while_inserting(&mut self) {
        self.new_step = true;
        if let Some(change) = self.changing.take() {
            self.last_change = Some(change);
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
        self.edited(&edit);
        copy.edit(edit, self.new_step);
        self.new_step = false;
    }

    /// Moves the word being completed, and the places its items are at,
    /// through `edit`, made to the text.
    fn edited(&mut self, edit: &Edit) {
        if let Some(completing) = &mut self.completing {
            completing.start = edit.map(completing.start, Bias::Before);
        }
        if let Some(completion) = &mut self.completion {
            completion.edited(edit);
        }
    }

    // --- Completion and help with calls -------------------------------------

    /// The keys completion takes in insert mode: Ctrl+Space asks for it,
    /// as do Ctrl+N and Ctrl+P when the menu isn't shown; while it is,
    /// they and the arrows move through it, Ctrl+Y accepts, as Enter and
    /// Tab can, and Ctrl+E closes it. Esc closes it and goes on to leave
    /// insert mode. `None` for any other key.
    fn completion_stroke(&mut self, stroke: Stroke, copy: &mut BufferCopy) -> Option<Done> {
        let showing = self.completion.as_ref().is_some_and(Completion::showing);
        let moved = self
            .completion
            .as_ref()
            .is_some_and(|completion| completion.moved);
        let accepts = match stroke {
            Stroke::Char {
                ch: 'y',
                ctrl: true,
            } => true,
            Stroke::Enter => match self.as_you_type.accept {
                CompleteAccept::Enter => true,
                CompleteAccept::Picked => moved,
                CompleteAccept::CtrlY => false,
            },
            Stroke::Tab => self.as_you_type.tab,
            _ => false,
        };
        match stroke {
            _ if showing && accepts => self.accept(copy),
            Stroke::Char {
                ch: 'n' | 'p',
                ctrl: true,
            }
            | Stroke::Down
            | Stroke::Up
                if showing =>
            {
                let down = matches!(stroke, Stroke::Down | Stroke::Char { ch: 'n', .. });
                if let Some(completion) = &mut self.completion {
                    completion.pick(down);
                }
                // A list asked for again mustn't come and lose the pick.
                self.completion_due = None;
                if self.completing.is_some_and(|completing| completing.again) {
                    self.completing = None;
                }
                self.document(copy);
            }
            Stroke::Char {
                ch: ' ' | 'n' | 'p',
                ctrl: true,
            } => self.ask_completion(Trigger::Invoked, true, copy),
            Stroke::Char {
                ch: 'e',
                ctrl: true,
            } if self.completion.is_some() || self.completing.is_some() => self.close_completion(),
            Stroke::Escape => {
                self.close_completion();
                return None;
            }
            _ => return None,
        }
        Some(Done {
            redraw: true,
            yanked: None,
        })
    }

    /// What a key typed in insert mode asks of the language server, after
    /// it's done what it does, `changed` if it edited or moved the cursor.
    /// The word being completed is narrowed down, or the menu closed if the
    /// cursor has left it. A character the server says should ask, asks at
    /// once; a word's, once typing pauses. A call's signature is asked
    /// about as its brackets and commas are typed, and again as the text
    /// changes while it's shown.
    fn as_you_type(&mut self, stroke: Stroke, changed: bool, copy: &mut BufferCopy) {
        if self.mode != Mode::Insert {
            self.close_completion();
            self.close_signature();
            return;
        }
        let moved = matches!(
            stroke,
            Stroke::Left
                | Stroke::Right
                | Stroke::Up
                | Stroke::Down
                | Stroke::Home
                | Stroke::End
                | Stroke::PageUp
                | Stroke::PageDown
        );
        if moved {
            self.close_completion();
        }
        self.check_completion(copy);
        let typed = match stroke {
            Stroke::Char { ch, ctrl: false } => Some(ch),
            _ => None,
        };
        let features = copy.features();
        let asks = |list: Option<&Vec<char>>, ch| list.is_some_and(|list| list.contains(&ch));
        let completes = typed.filter(|&ch| asks(features.completion.as_ref(), ch));
        let signs = typed.filter(|&ch| asks(features.signature.as_ref(), ch));
        let signs_again = typed.filter(|ch| features.signature_again.contains(ch));
        let can_sign = features.signature.is_some() && self.as_you_type.signature;
        // Typing asks only a server that completes.
        let auto = self.as_you_type.auto && features.completion.is_some();
        if let Some(ch) = completes.filter(|_| auto) {
            self.close_completion();
            self.ask_completion(Trigger::Character(ch), false, copy);
        } else if typed.is_some_and(is_word) || (changed && !moved) {
            // Not all there is: asked again. Nothing yet: asked, for a word.
            if (self.completion.as_ref()).is_some_and(|completion| completion.incomplete) {
                self.complete_later(Trigger::Again);
            } else if self.completion.is_none()
                && self.completing.is_none()
                && auto
                && typed.is_some_and(is_word)
            {
                self.complete_later(Trigger::Invoked);
            }
        }
        if !can_sign {
            return;
        }
        let shown = self.signature.is_some();
        if let Some(ch) = signs.or(signs_again.filter(|_| shown)) {
            self.ask_signature(Trigger::Character(ch), copy);
        } else if shown && changed {
            self.signature_due = Some(Instant::now() + self.as_you_type.delay);
        }
    }

    /// Asks what could finish the word before the cursor once typing
    /// pauses, for `trigger`, or at once if the options say not to wait.
    fn complete_later(&mut self, trigger: Trigger) {
        let due = Instant::now() + self.as_you_type.delay;
        // Asking again for what the menu shows can't wait less long than
        // asking at first.
        if let Some((_, Trigger::Invoked)) = self.completion_due
            && trigger == Trigger::Again
        {
            return;
        }
        self.completion_due = Some((due, trigger));
    }

    /// Asks the language server what could finish the word before the
    /// cursor, for `trigger`, saying so if there's nothing when
    /// `explicit`, as asked for with a key.
    fn ask_completion(&mut self, trigger: Trigger, explicit: bool, copy: &mut BufferCopy) {
        self.completion_due = None;
        let text = self.text(copy);
        let at = text.byte_at(self.cursor.at);
        let start = word_start(&text, at);
        drop(text);
        // What's shown is for another word.
        if self
            .completion
            .as_ref()
            .is_some_and(|completion| completion.start != start)
        {
            self.completion = None;
        }
        let id = copy.ask(Query::Completion(trigger), at);
        self.completing = Some(Requested {
            id,
            start,
            explicit,
            again: trigger == Trigger::Again,
            len: at - start,
        });
    }

    /// Asks the language server about the call whose arguments are at the
    /// cursor, for `trigger`.
    fn ask_signature(&mut self, trigger: Trigger, copy: &mut BufferCopy) {
        self.signature_due = None;
        let at = self.cursor_byte(copy);
        let retrigger = self.signature.is_some();
        self.signing = Some(copy.ask(Query::SignatureHelp { trigger, retrigger }, at));
    }

    /// Whether the cursor is still in the word starting at `start`: after
    /// it, with only a word's characters between, on its line.
    fn in_word(&self, start: usize, copy: &BufferCopy) -> Option<String> {
        let text = self.text(copy);
        let at = text.byte_at(self.cursor.at);
        let typed = text.rope().get_byte_slice(start..at)?.to_string();
        typed.chars().all(is_word).then_some(typed)
    }

    /// Narrows the menu down to the word typed, or closes it, and forgets
    /// a completion query asked, if the cursor has left the word.
    fn check_completion(&mut self, copy: &mut BufferCopy) {
        if let Some(completing) = self.completing
            && self.in_word(completing.start, copy).is_none()
        {
            self.completing = None;
        }
        let Some(completion) = &self.completion else {
            return;
        };
        match self.in_word(completion.start, copy) {
            // Shorter than when it was asked: the server may have left out
            // what didn't start with it, so it's asked again.
            Some(typed) if typed.len() < completion.asked => {
                self.close_completion();
                if self.as_you_type.auto
                    && copy.features().completion.is_some()
                    && !typed.is_empty()
                {
                    self.complete_later(Trigger::Invoked);
                }
            }
            Some(typed) => {
                let completion = self.completion.as_mut().expect("just found");
                let picked = completion.picked();
                completion.narrow(&typed);
                if !completion.showing() && !completion.incomplete {
                    // Nothing it gave matches now: typing more asks again.
                    self.close_completion();
                } else if completion.picked() != picked {
                    self.document(copy);
                }
            }
            None => self.close_completion(),
        }
    }

    /// Closes the menu, and forgets what was asked for it.
    fn close_completion(&mut self) {
        self.completion = None;
        self.completing = None;
        self.completion_due = None;
        self.resolve_due = None;
    }

    /// Closes the signature, and forgets what was asked for it.
    fn close_signature(&mut self) {
        self.signature = None;
        self.signing = None;
        self.signature_due = None;
    }

    /// Shows what's known of the item picked's documentation, and asks the
    /// server for the rest once picking pauses, as moving through the menu
    /// quickly would otherwise ask about every item passed.
    fn document(&mut self, copy: &mut BufferCopy) {
        self.show_documentation();
        if self.as_you_type.delay.is_zero() {
            self.resolve_picked(copy);
        } else {
            self.resolve_due = Some(Instant::now() + self.as_you_type.delay);
        }
    }

    /// Shows what's known of the item picked's documentation.
    fn show_documentation(&mut self) {
        let tabstop = self.indent.tabstop;
        if let Some(completion) = &mut self.completion {
            completion.document(tabstop);
        }
    }

    /// Asks the server for the item picked's documentation, unless it has
    /// been asked, or is being.
    fn resolve_picked(&mut self, copy: &mut BufferCopy) {
        self.resolve_due = None;
        let Some(completion) = &mut self.completion else {
            return;
        };
        let Some(index) = completion.picked() else {
            return;
        };
        let asking = completion.resolving.is_some_and(|(_, item)| item == index);
        if completion.resolved(index).is_none() && !asking {
            let query = Query::Resolve {
                list: completion.list,
                item: index,
            };
            completion.resolving = Some((copy.ask(query, 0), index));
        }
    }

    /// Accepts the item picked: puts its text in place of the word typed,
    /// as part of the same undo step, with the edits it needs elsewhere,
    /// and puts the cursor where its snippet says. `.` types the same text
    /// again.
    fn accept(&mut self, copy: &mut BufferCopy) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        let at = self.insert_byte(copy);
        let accepted = match completion.accept(copy.text(), at) {
            Some(Ok(accepted)) => accepted,
            Some(Err(why)) => {
                // The menu stays, to pick another.
                self.completion = Some(completion);
                self.status = Some(why);
                return;
            }
            None => return,
        };
        self.completing = None;
        self.completion_due = None;
        self.resolve_due = None;
        if let Some(change) = &mut self.changing {
            change.typed.push(Typed::Accepted {
                text: accepted.text.clone(),
                offset: accepted.offset,
                replaced: accepted.replaced,
            });
        }
        self.make(accepted.edit, copy);
        self.cursor = Cursor::new(self.text(copy).point_of(accepted.cursor));
        // A call it put in, such as `f(`, is asked about.
        let before = accepted.text[..accepted.offset].chars().next_back();
        if let Some(ch) = before
            && self.as_you_type.signature
            && (copy.features().signature.as_ref()).is_some_and(|list| list.contains(&ch))
        {
            self.ask_signature(Trigger::Character(ch), copy);
        }
    }

    /// Takes in the answer to the completion query `requested`.
    fn completed(&mut self, requested: Requested, answer: Answer, copy: &mut BufferCopy) {
        let nothing = |editor: &mut Self, why: &str| {
            if requested.explicit {
                editor.status = Some(why.to_owned());
            }
        };
        if self.mode != Mode::Insert {
            return;
        }
        match answer {
            Answer::Completions { items, incomplete } => {
                let Some(typed) = self.in_word(requested.start, copy) else {
                    return;
                };
                let mut completion =
                    Completion::new(requested.id, requested.start, items, incomplete);
                completion.asked = requested.len;
                completion.narrow(&typed);
                // What was picked in the list it replaces stays picked.
                let moved_to = (self.completion.as_ref())
                    .and_then(Completion::moved_to)
                    .map(str::to_owned);
                if let Some(label) = moved_to {
                    completion.pick_label(&label);
                }
                if completion.showing() {
                    self.completion = Some(completion);
                    self.document(copy);
                } else if incomplete {
                    // Kept, to ask again as more is typed.
                    self.completion = Some(completion);
                } else {
                    self.completion = None;
                    nothing(self, "no completions");
                }
            }
            Answer::Failed(why) => nothing(self, &why),
            _ => nothing(self, "no completions"),
        }
    }

    /// Asks what's due to be asked of the language server, now typing has
    /// paused, as of `now`. Returns whether it asked anything.
    pub(crate) fn tick(&mut self, now: Instant, copy: &mut BufferCopy) -> bool {
        let mut asked = false;
        if self.mode != Mode::Insert {
            self.completion_due = None;
            self.signature_due = None;
            return false;
        }
        if let Some((due, trigger)) = self.completion_due
            && due <= now
        {
            self.completion_due = None;
            let at = self.cursor_byte(copy);
            let worth = match trigger {
                Trigger::Again => self.completion.is_some(),
                _ => word_start(&self.text(copy), at) < at,
            };
            if worth {
                self.ask_completion(trigger, false, copy);
                asked = true;
            }
        }
        if let Some(due) = self.signature_due
            && due <= now
        {
            self.ask_signature(Trigger::Again, copy);
            asked = true;
        }
        if let Some(due) = self.resolve_due
            && due <= now
        {
            self.resolve_picked(copy);
            asked = true;
        }
        asked
    }

    /// When something's next due to be asked of the language server, if
    /// anything is.
    pub(crate) fn wakes(&self) -> Option<Instant> {
        let completion = self.completion_due.map(|(due, _)| due);
        (completion.into_iter())
            .chain(self.signature_due)
            .chain(self.resolve_due)
            .min()
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

    /// A key typed into the bar: a search's, or a new name's. Ctrl+U
    /// clears it, as in Vim's command line.
    fn prompt_stroke(&mut self, stroke: Stroke, copy: &mut BufferCopy) -> Done {
        let Some(prompt) = &mut self.prompt else {
            return Done::default();
        };
        let typed = match prompt {
            Prompt::Search { query, .. } => query,
            Prompt::Rename { name, .. } => name,
        };
        match stroke {
            Stroke::Escape => self.prompt = None,
            Stroke::Backspace => {
                if typed.pop().is_none() {
                    self.prompt = None;
                }
            }
            Stroke::Char {
                ch: 'u',
                ctrl: true,
            } => typed.clear(),
            Stroke::Enter => match self.prompt.take() {
                Some(Prompt::Search { query, backward }) => {
                    if !query.is_empty() {
                        self.search = Some(Searched {
                            query,
                            backward,
                            whole: false,
                        });
                    }
                    let from = self.cursor.at;
                    self.search_again(false, copy);
                    if self.cursor.at != from {
                        self.left_from(from, copy);
                    }
                }
                Some(Prompt::Rename { name, was }) => self.rename(name, &was, copy),
                None => {}
            },
            Stroke::Char { ch, ctrl: false } => typed.push(ch),
            _ => {}
        }
        Done {
            redraw: true,
            yanked: None,
        }
    }

    /// What `action` asks of the language server, or shows of what it
    /// said.
    fn language_server(&mut self, action: Action, copy: &mut BufferCopy) {
        match action {
            Action::Ask(query) => self.ask(query, copy),
            Action::Hover => self.hover(copy),
            Action::ShowDiagnostics => self.show_diagnostics(copy),
            Action::Rename => self.ask_for_name(copy),
            _ => {}
        }
    }

    /// `K`: asks the language server what's under the cursor.
    fn hover(&mut self, copy: &mut BufferCopy) {
        let at = self.text(copy).byte_at(self.cursor.at);
        let id = copy.ask(Query::Hover, at);
        self.asked = Some((id, Asking::Hover(self.cursor.at)));
    }

    /// `grn`: asks in the bar for a new name for what's under the cursor,
    /// starting with the word there.
    fn ask_for_name(&mut self, copy: &BufferCopy) {
        let text = self.text(copy);
        let (start, end) = word_at(&text, text.byte_at(self.cursor.at));
        let was = text.slice(start, end);
        self.prompt = Some(Prompt::Rename {
            name: was.clone(),
            was,
        });
    }

    /// Asks the language server to rename what's under the cursor, which
    /// was `was` when the name was asked for, to `name`, unless that's no
    /// change.
    fn rename(&mut self, name: String, was: &str, copy: &mut BufferCopy) {
        if name.is_empty() || name == was {
            self.status = Some("not renamed".into());
            return;
        }
        let at = self.text(copy).byte_at(self.cursor.at);
        self.renaming = Some(copy.rename(at, name));
    }

    /// Ctrl+W `d`: what's wrong under the cursor, or else on its line, in
    /// full, in the panel by the cursor.
    fn show_diagnostics(&mut self, copy: &BufferCopy) {
        let mut shown = self.diagnostics_under(copy);
        if shown.is_empty() {
            let text = self.text(copy);
            let line = self.cursor.at.line;
            let (start, end) = (text.line_start(line), text.line_end(line));
            shown = copy.diagnostics().within(start, end).collect();
        }
        self.panel = Panel::new(&shown, None, self.cursor.at, self.indent.tabstop);
        if self.panel.is_none() {
            self.status = Some("nothing's wrong here, as far as the language server knows".into());
        }
    }

    /// The diagnostics about the text under the cursor.
    fn diagnostics_under<'c>(&self, copy: &'c BufferCopy) -> Vec<&'c Diagnostic> {
        let at = self.text(copy).byte_at(self.cursor.at);
        (copy.diagnostics().within(at, at + 1))
            .filter(|diagnostic| {
                diagnostic.start <= at && at < diagnostic.end.max(diagnostic.start + 1)
            })
            .collect()
    }

    /// `n`, or `N` if `reverse`: the next match of the last search.
    fn search_again(&mut self, reverse: bool, copy: &BufferCopy) {
        let Some(Searched {
            query,
            backward,
            whole,
        }) = self.search.clone()
        else {
            self.status = Some("no search yet".into());
            return;
        };
        let text = self.text(copy);
        let from = text.byte_at(self.cursor.at);
        let all = text.slice(0, text.rope().len_bytes());
        // A whole word, as `*` searches, ignores case or not as
        // `ignorecase` says, never as `smartcase`, as in Vim.
        let exact = if whole {
            self.case.exact_always()
        } else {
            self.case.exact(&query)
        };
        let backward = backward != reverse;
        if let Some(start) = find(&all, &query, from, (backward, exact, whole)) {
            let last = start + query.len() - 1;
            self.found = Some((text.point_of(start), text.point_of(last)));
            self.cursor = Cursor::new(text.point_of(start));
        } else {
            self.found = None;
            self.status = Some(format!("{query} isn't in the file"));
        }
    }

    /// `*`, or `#` if `backward`: searches for the word under the cursor,
    /// as a whole word, and goes to the next place it is, or the one
    /// before, going round the file's ends.
    fn search_word(&mut self, backward: bool, copy: &BufferCopy) {
        let text = self.text(copy);
        // The word under the cursor, or the next on its line, as Vim's.
        let here = text.byte_at(self.cursor.at);
        let line_end = text.line_end(self.cursor.at.line);
        let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
        let next = (text.slice(here, line_end).char_indices())
            .find(|&(_, ch)| is_word(ch))
            .map(|(at, _)| here + at);
        let Some((start, end)) = next.map(|at| word_at(&text, at)) else {
            self.status = Some("no word under the cursor".into());
            return;
        };
        let query = text.slice(start, end);
        // From the word's start, so going forward finds the next, not this.
        self.cursor = Cursor::new(text.point_of(start));
        self.search = Some(Searched {
            query,
            backward,
            whole: true,
        });
        self.search_again(false, copy);
    }

    /// `]d`, or `[d` if `backward`: to the start of the `times`th
    /// diagnostic after the cursor, or before it, going round the file's
    /// ends.
    fn next_diagnostic(&mut self, backward: bool, times: usize, copy: &BufferCopy) {
        let all = copy.diagnostics().all();
        if all.is_empty() {
            self.status = Some("nothing's wrong, as far as the language server knows".into());
            return;
        }
        let text = self.text(copy);
        // Where the cursor lands on each, which isn't always where it
        // starts: one past a line's end, as a missing semicolon is, lands
        // on the line's last character. Going by where it starts would
        // land there again and again.
        let mut landings: Vec<usize> = (all.iter())
            .map(|diagnostic| {
                let point = text.point_of(diagnostic.start);
                let cells = text.laid(point.line).map_or(0, |laid| laid.cells());
                let col = point.col.min(cells.saturating_sub(1));
                text.byte_at(Point { col, ..point })
            })
            .collect();
        landings.sort_unstable();
        landings.dedup();
        let at = text.byte_at(self.cursor.at);
        let count = landings.len();
        // Round and round as often as the count says.
        let steps = (times.max(1) - 1) % count;
        let index = if backward {
            let before = landings.partition_point(|&landing| landing < at);
            (before + count - 1 - steps) % count
        } else {
            let after = landings.partition_point(|&landing| landing <= at);
            (after + steps) % count
        };
        self.cursor = Cursor::new(text.point_of(landings[index]));
    }

    /// `]c`, or `[c` if `backward`: to the line of the `times`th hunk after
    /// the cursor's, or before it, going round the file's ends, as
    /// gitsigns' do.
    fn next_hunk(&mut self, backward: bool, times: usize, copy: &BufferCopy) {
        let rope = copy.text();
        let mut lines: Vec<usize> = (copy.hunks().all().iter())
            .map(|hunk| hunk.line(rope))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        if lines.is_empty() {
            self.status = Some(NO_CHANGES.into());
            return;
        }
        let at = usize::try_from(self.cursor.at.line).unwrap_or(usize::MAX);
        let count = lines.len();
        // Round and round as often as the count says.
        let steps = (times.max(1) - 1) % count;
        let index = if backward {
            let before = lines.partition_point(|&line| line < at);
            (before + count - 1 - steps) % count
        } else {
            let after = lines.partition_point(|&line| line <= at);
            (after + steps) % count
        };
        let text = self.text(copy);
        self.cursor = Cursor::new(Point {
            line: lines[index] as u64,
            col: 0,
        });
        self.cursor.apply(Motion::FirstNonBlank, &text);
    }

    /// Does `action` to the hunk at the cursor's line, as the window's
    /// keys and commands ask.
    pub(crate) fn hunk(&mut self, action: HunkAction, copy: &mut BufferCopy) -> Done {
        self.status = None;
        self.panel = None;
        let from = self.cursor.at;
        match action {
            HunkAction::Next { forward } => {
                self.next_hunk(!forward, 1, copy);
                if self.cursor.at != from {
                    self.left_from(from, copy);
                }
            }
            HunkAction::Preview => self.preview_hunk(copy),
            HunkAction::Reset => self.reset_hunk(copy),
            HunkAction::Stage => self.stage_hunk(copy),
        }
        if self.mode != Mode::Insert {
            self.normal_cursor(copy);
        }
        self.follow(copy);
        Done {
            redraw: true,
            yanked: None,
        }
    }

    /// The hunk at the cursor's line, and its place among the copy's.
    fn hunk_here<'c>(&self, copy: &'c BufferCopy) -> Option<(usize, &'c Hunk)> {
        let line = usize::try_from(self.cursor.at.line).ok()?;
        let (index, _) = copy.hunks().find(copy.text(), line)?;
        Some((index, &copy.hunks().all()[index]))
    }

    /// Shows the hunk at the cursor's line in the panel by the cursor: its
    /// lines as they were, and as they are.
    fn preview_hunk(&mut self, copy: &BufferCopy) {
        let Some((_, hunk)) = self.hunk_here(copy) else {
            self.status = Some(NO_CHANGE_HERE.into());
            return;
        };
        let lines = hunk.whole_lines(copy.text());
        let now = self.text(copy).slice(lines.start, lines.end);
        let tabstop = self.indent.tabstop;
        self.panel = Panel::hunk(&hunk.base, &now, self.cursor.at, tabstop);
    }

    /// Puts the lines of the hunk at the cursor's line back as they are in
    /// git's index: an edit of the user's, which `u` undoes.
    fn reset_hunk(&mut self, copy: &mut BufferCopy) {
        let Some((index, hunk)) = self.hunk_here(copy) else {
            self.status = Some(NO_CHANGE_HERE.into());
            return;
        };
        let hunk = hunk.clone();
        // As an operator in visual mode does, it ends the selection, which
        // could reach past the text it leaves.
        self.leave_visual();
        // Its lines whole, though an edit since it was found, such as one
        // joining lines, has moved its ends into a line.
        let lines = hunk.whole_lines(copy.text());
        let text = self.text(copy);
        let now = text.slice(lines.start, lines.end);
        let edit = Edit::replace(text.rope().len_bytes(), lines.start, &now, &hunk.base);
        drop(text);
        // It goes before the edit, which would leave it as lines deleted
        // until the daemon compares again.
        copy.forget_hunk(index);
        self.new_step = true;
        self.make(edit, copy);
        self.new_step = true;
        let text = self.text(copy);
        self.cursor = Cursor::new(text.point_of(lines.start));
        self.cursor.apply(Motion::FirstNonBlank, &text);
    }

    /// Asks the daemon to stage the hunk at the cursor's line.
    fn stage_hunk(&mut self, copy: &mut BufferCopy) {
        if self.hunk_here(copy).is_none() {
            self.status = Some(NO_CHANGE_HERE.into());
            return;
        }
        let at = self.text(copy).byte_at(self.cursor.at);
        self.staging.push(copy.stage(at));
    }

    /// Says in the bar how the stage asked last went.
    fn staged(&mut self, answer: &Answer) {
        self.status = Some(match answer {
            Answer::Staged => "hunk staged".into(),
            Answer::Failed(why) => why.clone(),
            _ => NO_CHANGE_HERE.into(),
        });
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
        self.edited(edit);
        if let Some((cursor, anchor)) = &mut self.held {
            *cursor = edit.map(*cursor, Bias::Before);
            if let Some(anchor) = anchor {
                *anchor = edit.map(*anchor, Bias::Before);
            }
        }
        self.place_held(copy);
    }

    /// Takes in what an update from the daemon did: `jump` if this pane
    /// asked for the undo or redo it made. An answer to this pane's
    /// completion query is taken, leaving nothing for the others.
    pub(crate) fn heard(&mut self, heard: &mut Heard, jump: bool, copy: &mut BufferCopy) {
        match heard {
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
            Heard::Answer { id, answer } => {
                let resolving = (self.completion.as_ref())
                    .and_then(|completion| completion.resolving)
                    .filter(|(resolving, _)| resolving == id);
                if let Some((asked, asking)) = self.asked
                    && asked == *id
                {
                    self.asked = None;
                    self.answered(asking, answer, copy);
                } else if self.renaming == Some(*id) {
                    self.renaming = None;
                    self.renamed(answer);
                } else if let Some(index) = self.staging.iter().position(|staging| staging == id) {
                    self.staging.remove(index);
                    self.staged(answer);
                } else if let Some(requested) = self.completing
                    && requested.id == *id
                {
                    self.completing = None;
                    let answer = std::mem::replace(answer, Answer::Nothing);
                    self.completed(requested, answer, copy);
                } else if let Some((_, index)) = resolving {
                    self.resolved(index, answer);
                } else if self.signing == Some(*id) {
                    self.signing = None;
                    self.signed(answer);
                }
            }
            Heard::Nothing => {}
        }
        // A new text, or the window's own undo, closes the panel, as an
        // edit typed does, and the menu.
        if matches!(heard, Heard::Replaced | Heard::Reversed(_)) {
            self.panel = None;
            self.close_completion();
        }
        if self.mode == Mode::Insert {
            // Others' edits may have taken the word away.
            self.check_completion(copy);
        }
        if self.mode != Mode::Insert {
            self.normal_cursor(copy);
        }
        self.follow(copy);
        // Others' edits move the cursor with the text, and the panel with
        // it.
        if let Some(panel) = &mut self.panel {
            panel.anchor = self.cursor.at;
        }
    }

    /// Takes in what the server said of item `index` of the menu,
    /// resolved, showing it if it's still the one picked.
    fn resolved(&mut self, index: u32, answer: &Answer) {
        let Some(completion) = &mut self.completion else {
            return;
        };
        completion.resolving = None;
        let resolved = match answer {
            Answer::Resolved {
                detail,
                documentation,
            } => Resolved {
                detail: detail.clone(),
                documentation: documentation.clone(),
            },
            _ => Resolved::default(),
        };
        completion.resolve(index, resolved);
        if completion.picked() == Some(index) {
            self.show_documentation();
        }
    }

    /// Takes in what the server said of the call whose arguments are being
    /// typed: its signature is shown, or none if it says there's no call.
    fn signed(&mut self, answer: &Answer) {
        self.signature = match answer {
            Answer::Signature {
                label,
                parameter,
                index,
                count,
            } if self.mode == Mode::Insert && self.as_you_type.signature => {
                Panel::signature(label, *parameter, (*index, *count))
            }
            _ => None,
        };
    }

    /// `gd` and the like: asks the language server about where the cursor
    /// is.
    fn ask(&mut self, query: Query, copy: &mut BufferCopy) {
        let at = self.text(copy).byte_at(self.cursor.at);
        self.asked = Some((copy.ask(query, at), Asking::Jump));
    }

    /// Acts on the language server's answer to what it was last asked.
    fn answered(&mut self, asking: Asking, answer: &Answer, copy: &BufferCopy) {
        match asking {
            Asking::Jump => self.jump(answer, copy),
            Asking::Hover(at) if at == self.cursor.at => {
                let hover = match answer {
                    Answer::Hover { text, markdown } => Some((text.as_str(), *markdown)),
                    _ => None,
                };
                let tabstop = self.indent.tabstop;
                self.panel = Panel::new(&self.diagnostics_under(copy), hover, at, tabstop);
                if self.panel.is_none() {
                    self.status = Some(match answer {
                        Answer::Failed(why) => why.clone(),
                        _ => "no information".into(),
                    });
                }
            }
            // The cursor has moved since.
            Asking::Hover(_) => {}
        }
    }

    /// Says in the bar how the rename asked last went.
    fn renamed(&mut self, answer: &Answer) {
        let plural = |n: usize, one: &str| {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {one}s")
            }
        };
        self.status = Some(match answer {
            Answer::Renamed { occurrences, files } => format!(
                "renamed {} in {}",
                plural(*occurrences, "occurrence"),
                plural(*files, "file")
            ),
            Answer::Failed(why) => why.clone(),
            _ => "nothing to rename there".into(),
        });
    }

    /// Goes where the language server's answer to `gd` and the like says:
    /// its place, here or, in another file, for the desk to open; or if it
    /// gives several, they're for the desk to open the finder on.
    fn jump(&mut self, answer: &Answer, copy: &BufferCopy) {
        match answer {
            Answer::Locations(found) if found.len() > 1 => self.places = Some(found.clone()),
            Answer::Locations(found) => {
                let Some(first) = found.first() else {
                    return;
                };
                self.left_from(self.cursor.at, copy);
                if first.path == copy.path() {
                    self.go_to(first.line, first.col, copy);
                } else {
                    self.opening = Some(first.clone());
                }
            }
            Answer::Nothing => {
                self.status = Some("the language server knows of nothing there".into());
            }
            Answer::Failed(why) => self.status = Some(why.clone()),
            Answer::Hover { .. }
            | Answer::Renamed { .. }
            | Answer::Formatted { .. }
            | Answer::Staged
            | Answer::Completions { .. }
            | Answer::Resolved { .. }
            | Answer::Signature { .. } => {}
        }
    }

    /// Puts the cursor on `line`, `col` bytes into it, as far as the line
    /// goes. A message about where it was, such as a search's that found
    /// nothing, goes with it.
    pub(crate) fn go_to(&mut self, line: u64, col: usize, copy: &BufferCopy) {
        self.status = None;
        self.panel = None;
        self.close_completion();
        let text = self.text(copy);
        let line = line.min(text.last_line());
        let (start, end) = (text.line_start(line), text.line_end(line));
        // Back to a character's start, from inside one.
        let rope = text.rope();
        let at = rope.char_to_byte(rope.byte_to_char((start + col).min(end)));
        self.cursor = Cursor::new(text.point_of(at));
        drop(text);
        if self.mode == Mode::Insert {
            // It may stay past the line's end, where typing goes, and it
            // moved, as the arrow keys move it.
            self.moved_while_inserting();
        } else {
            self.normal_cursor(copy);
        }
        self.follow(copy);
    }

    /// The place in another file the last answer goes to, for the desk to
    /// open, if there's one.
    pub(crate) fn take_opening(&mut self) -> Option<FileLocation> {
        self.opening.take()
    }

    /// The places the last answer gave, if it gave several, for the desk
    /// to open the finder on.
    pub(crate) fn take_places(&mut self) -> Option<Vec<FileLocation>> {
        self.places.take()
    }

    /// Notes that the cursor jumped from `from`, for the desk's jump list.
    fn left_from(&mut self, from: Point, copy: &BufferCopy) {
        self.jumped_from = Some(self.text(copy).byte_at(from));
    }

    /// The byte the cursor left on its last jump, if it's jumped since
    /// this was last asked, for the desk's jump list.
    pub(crate) fn take_left(&mut self) -> Option<usize> {
        self.jumped_from.take()
    }

    /// The byte the cursor is on.
    pub(crate) fn cursor_byte(&self, copy: &BufferCopy) -> usize {
        self.text(copy).byte_at(self.cursor.at)
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
        let at = self.cursor.at;
        self.cursor_into_view(&text);
        // Moving the cursor away from the word closes the menu.
        if self.cursor.at != at {
            self.close_completion();
        }
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
        self.panel = None;
        self.close_completion();
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

    /// The cells the signs and line numbers take for a text of `lines`
    /// lines, or none if they're off or would leave the text too little
    /// room.
    fn gutter_for(&self, lines: u64) -> usize {
        let gutter = self.sign_cells() + self.numbers_width(lines);
        if gutter > 0 && self.width >= gutter + LEAST_TEXT {
            gutter
        } else {
            0
        }
    }

    /// The cells the signs take, left of the line numbers: one, while
    /// they're on.
    fn sign_cells(&self) -> usize {
        usize::from(self.signs)
    }

    /// The cells line numbers take for a text of `lines` lines: the widest
    /// number and a space, at least `numberwidth`, or none if they're off.
    fn numbers_width(&self, lines: u64) -> usize {
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
        (widest.ilog10() as usize + 2).max(least)
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
                    let (start, end) = (text.line_start(line), text.line_end(line));
                    let diagnostics: Vec<&Diagnostic> =
                        copy.diagnostics().within(start, end).collect();
                    // The worst said of this line, where it starts, which
                    // its number and the text after it tell of.
                    let worst = (diagnostics.iter())
                        .filter(|diagnostic| diagnostic.start >= start)
                        .min_by_key(|diagnostic| diagnostic.severity);
                    if self.gutter > 0 && self.signs {
                        let at = usize::try_from(line).unwrap_or(usize::MAX);
                        match copy.hunks().sign(text.rope(), at) {
                            Some(sign) => {
                                let (sign, style) = colors::sign(sign);
                                builder.push(sign, &style, true);
                            }
                            None => builder.push(" ", &Style::DEFAULT, true),
                        }
                    }
                    let numbers = self.gutter.saturating_sub(self.sign_cells());
                    if numbers > 0 {
                        let number = self.numbers.label(line, cursor);
                        let number = format!("{number:>0$} ", numbers - 1);
                        let style = match worst {
                            Some(diagnostic) if line != cursor => Style {
                                fg: colors::diagnostic(diagnostic.severity),
                                ..Style::DEFAULT
                            },
                            _ => colors::line_number(line == cursor),
                        };
                        builder.push(&number, &style, true);
                    }
                    let laid = Laid::new(text.line_text(line), self.indent.tabstop);
                    let spans = copy.highlights().within(start, end);
                    laid.show(
                        &mut builder,
                        self.left,
                        self.text_width(),
                        spans,
                        &diagnostics,
                        start,
                    );
                    if let Some(diagnostic) = worst.filter(|_| self.virtual_text) {
                        // Two cells after the line's end, as much as fits:
                        // nothing, past the pane's edge.
                        let room = self.width.saturating_sub(builder.filled());
                        let gap = (self.gutter + laid.cells().saturating_sub(self.left) + 2)
                            .saturating_sub(builder.filled());
                        if gap < room {
                            builder.push(&" ".repeat(gap), &Style::DEFAULT, true);
                            let style = colors::virtual_text(diagnostic.severity);
                            let message = diagnostic.message.lines().next().unwrap_or_default();
                            builder.push("■ ", &style, true);
                            builder.push(message, &style, true);
                        }
                    }
                } else if copy.loaded() {
                    builder.push("~", &Style::DEFAULT, true);
                }
                builder.finish(false)
            })
            .collect();
        // By the cursor, if it's in view.
        let at = self.cursor.at;
        let row = (at.line.checked_sub(self.top))
            .and_then(|row| usize::try_from(row).ok())
            .filter(|&row| row < self.height);
        let col = (at.col.checked_sub(self.left)).filter(|&col| col < self.text_width());
        let cursor = row.zip(col).map(|(row, col)| (row, self.gutter + col));
        let pane = (self.height, self.width);
        if let Some(panel) = &mut self.panel {
            panel.settle(pane, cursor);
        }
        // The menu goes under the cursor, and the signature over it, or
        // the other way round if the menu has no room under it.
        let mut menu_under = None;
        if let Some(completion) = &mut self.completion {
            let word = text.point_of(completion.start.min(text.rope().len_bytes()));
            let word = self.gutter + word.col.saturating_sub(self.left);
            completion.settle(pane, cursor, word);
            menu_under = (completion.placed())
                .zip(cursor)
                .map(|(placed, (row, _))| placed.top > row);
        }
        if let Some(signature) = &mut self.signature {
            let sides = match menu_under {
                Some(true) => (true, false),
                Some(false) => (false, true),
                None => (true, true),
            };
            signature.settle_over(pane, cursor, sides);
        }
    }

    /// Closes the panel by the cursor, and the completion menu and
    /// signature, as the focus leaving does.
    pub(crate) fn close_panel(&mut self) {
        self.panel = None;
        self.close_completion();
        self.close_signature();
    }

    /// The panel by the cursor, if it's shown, as it's to be drawn, in
    /// cells of the pane.
    pub(crate) fn panel(&self) -> Option<PanelView<'_>> {
        self.panel.as_ref()?.view()
    }

    /// Everything shown over the text by the cursor, as it's to be drawn,
    /// in cells of the pane, in the order it's drawn: the panel, the
    /// signature, the completion menu and the documentation beside it.
    pub(crate) fn panels(&self) -> Vec<PanelView<'_>> {
        let completion = self.completion.as_ref();
        [
            self.panel(),
            self.signature.as_ref().and_then(Panel::view),
            completion.and_then(Completion::menu),
            completion.and_then(Completion::docs),
        ]
        .into_iter()
        .flatten()
        .collect()
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
        Key::Named(NamedKey::Space) => Some(Stroke::ctrl(' ')),
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

/// A diagnostic as the bar tells of it: how much it matters, and the
/// first line of what it says, with its name and who says so, as
/// `error: mismatched types [E0308 rustc]`.
pub(crate) fn describe(diagnostic: &Diagnostic) -> String {
    let severity = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
        Severity::Hint => "hint",
    };
    let message = diagnostic.message.lines().next().unwrap_or_default();
    let by: Vec<&str> = [&diagnostic.code, &diagnostic.source]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    if by.is_empty() {
        format!("{severity}: {message}")
    } else {
        format!("{severity}: {message} [{}]", by.join(" "))
    }
}

/// What an operator takes of `text` with an exclusive motion from
/// `start` to `end`, if it's a `w` or `W` if `word`: up to `end`, but
/// not it.
fn exclusive(text: &Text<'_>, start: Point, end: Point, word: bool) -> Option<Span> {
    // `dw` on a line's last word stops at the line's end.
    if word && end.line > start.line {
        return Some(Span {
            start: text.byte_at(start),
            end: text.line_end(start.line).max(text.byte_at(start)),
            lines: None,
        });
    }
    // As Vim's: one that ends at a line's start, from another line, ends
    // at the end of the line before; and from at or before its line's
    // first character that isn't blank, takes whole lines, as `d}` does.
    if end.col == 0 && end.line > start.line {
        let mut first = Cursor::new(start);
        first.apply(Motion::FirstNonBlank, text);
        if start.col <= first.at.col {
            return Some(Span {
                start: text.line_start(start.line),
                end: text.line_start(end.line),
                lines: Some((start.line, end.line - 1)),
            });
        }
        return Some(Span {
            start: text.byte_at(start),
            end: text.line_end(end.line - 1).max(text.byte_at(start)),
            lines: None,
        });
    }
    let last = before(text, start, end)?;
    Some(Span {
        start: text.byte_at(start),
        end: text.char_end(last),
        lines: None,
    })
}

/// The last search: what was searched for, which way, and if only as a
/// whole word, as `*` searches.
#[derive(Clone, Debug)]
struct Searched {
    query: String,
    backward: bool,
    whole: bool,
}

/// Where the word at byte `at` of `text` starts and ends: letters, digits
/// and `_`, as `w` reads them. Both are `at` if there's none there.
fn word_at(text: &Text<'_>, at: usize) -> (usize, usize) {
    let rope = text.rope();
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let char_at = rope.byte_to_char(at.min(rope.len_bytes()));
    let mut start = char_at;
    while start > 0 && is_word(rope.char(start - 1)) {
        start -= 1;
    }
    let mut end = char_at;
    while end < rope.len_chars() && is_word(rope.char(end)) {
        end += 1;
    }
    if start == end {
        return (at, at);
    }
    (rope.char_to_byte(start), rope.char_to_byte(end))
}

/// Where the word ending at byte `at` of `text` starts: letters, digits
/// and `_`, as `w` reads them. `at` if there's none there.
fn word_start(text: &Text<'_>, at: usize) -> usize {
    let rope = text.rope();
    let mut start = rope.byte_to_char(at.min(rope.len_bytes()));
    while start > 0 && is_word(rope.char(start - 1)) {
        start -= 1;
    }
    rope.char_to_byte(start)
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
fn find(
    text: &str,
    query: &str,
    from: usize,
    (backward, exact, whole): (bool, bool, bool),
) -> Option<usize> {
    if query.is_empty() {
        return None;
    }
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let alone = |at: usize| {
        let before = text[..at].chars().next_back();
        let after = text[at + query.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    };
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
    let all: Vec<usize> = (haystack.match_indices(&*needle))
        .map(|(at, _)| at)
        .filter(|&at| !whole || alone(at))
        .collect();
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
