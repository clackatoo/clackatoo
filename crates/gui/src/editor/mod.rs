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
//! Every editor shares the registers (see [`registers`]) and the marks
//! (see [`marks`]). A macro recorded with `q` is the keys typed into the
//! editors; `@` types them again, through the same keys as typing does,
//! each one in turn, so whoever drives the editor sees each key's effects,
//! as the desk does the jumps and marks they make. A key that fails, as a
//! motion that can't move does, stops the macro, as in Vim.
//!
//! In insert mode, the language server is asked what could finish the
//! word being typed, shown in a menu under the cursor (see [`complete`]),
//! and about the call whose arguments are being typed, whose signature
//! shows over it. Typing never waits for either: they're asked when typing
//! pauses, and their answers are shown when they come, if they're still
//! about where the cursor is. A completion that's a snippet is filled in
//! place by place, with Tab going from one to the next (see [`snippet`]).
//!
//! `gra` asks the language server what it could do to the text at the
//! cursor, or selected, and shows it in a menu by the cursor to pick from
//! (see [`actions`]). The daemon does what's picked, as the user, so `u`
//! undoes it.
//!
//! Searches, `:s`, `:g` and the rest of Vim's commands for lines read
//! Vim's patterns (see [`pattern`], [`substitute`] and [`ex`]). Each is one
//! edit and one undo step, and while `:s` is typed, what it would do is
//! drawn in place, never made to the text (see [`preview`]).

mod actions;
mod case;
mod complete;
mod copy;
mod ex;
mod indent;
mod keys;
mod marked;
mod marks;
mod panel;
mod pattern;
mod pieces;
mod preview;
mod registers;
mod snippet;
mod substitute;
mod text;
mod wrap;

use std::time::{Duration, Instant};

use clackatoo_protocol::{Answer, BufferId, Edit, FileLocation, Query, Size, Trigger};
use clackatoo_term::{Row, RowBuilder, Style};
use clackatoo_text::{Bias, Diagnostic, Hunk};
use winit::keyboard::{Key, NamedKey};

use actions::{ActionMenu, MenuKey, MenuStroke};
use case::Recase;
pub use complete::Ranking;
use complete::{Completion, Resolved, is_word};
/// The daemon's side of a buffer, for the desk's tests.
#[cfg(test)]
pub(crate) use copy::tests::Daemon;
pub(crate) use copy::{BufferCopy, Heard, queries_asked};
pub(crate) use ex::is_editor_command;
pub(crate) use keys::Stroke;
use keys::{Action, Command, Entry, Operator, Parsed, Target, ViewAt};
pub(crate) use marks::{FilePlace, Marks};
#[cfg(test)]
pub(crate) use panel::Line as PanelLine;
use panel::Panel;
pub(crate) use panel::PanelView;
use pattern::{Matching, Pattern};
pub(crate) use pieces::GiveUp;
use preview::Preview;
use registers::Wrote;
pub(crate) use registers::{Register, Registers};
use text::{Kept, Text, tab_end};
use wrap::{Shown, Top, Wrapping};

use crate::colors;
use crate::input::KeyPress;
use crate::keymap::Chord;
use crate::motion::{
    Class, Cursor, Lines, Motion, Point, Reach, RowAt, before, class_at, last_row, row_at,
    rows_down, rows_up,
};
use crate::objects::Object;
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

/// What every editor of the window shares.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub(crate) registers: Registers,
    pub(crate) marks: Marks,
    /// The last `:s`, which `&`, `:&&` and `g&` make again, and whose
    /// replacement `~` stands for.
    pub(crate) substituted: Option<ex::Substituted>,
}

/// What a key did that the window must see to.
#[derive(Debug, Default)]
pub(crate) struct Done {
    /// The pane must be drawn again.
    pub(crate) redraw: bool,
}

impl Done {
    const REDRAW: Self = Self { redraw: true };
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
    /// replaced with `text`, and the cursor `offset` bytes into it. If
    /// `indented`, it's a snippet's, whose lines after the first take the
    /// indent of the line it's typed on.
    Accepted {
        text: String,
        offset: usize,
        replaced: usize,
        indented: bool,
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
    /// New lines are indented as the language says (`smartindent`).
    smart: bool,
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
            smart: options.smartindent,
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
    /// Whether to replace each match of `:s///c`, in turn.
    Confirm(ex::Confirming),
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
    /// What could be done to the text at this place, or selected from it,
    /// as `gra` asks: the menu's shown only if the cursor's still there.
    Actions(Point),
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
    /// Where the view starts, and when lines don't wrap, the first column
    /// shown.
    top: Top,
    left: usize,
    /// The pane has been resized since the view last followed the cursor,
    /// which the next settle does, as rows may have wrapped afresh.
    resized: bool,
    /// Lines laid out, and the rows they wrap onto, kept from frame to
    /// frame while the text stays as it is.
    kept: Kept,
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
    /// How long lines wrap.
    wrapping: Wrapping,
    /// How searches treat case.
    case: Case,
    /// The rows shown that are the file's, not past its end, as laid out
    /// last.
    shown: Vec<Shown>,
    /// Where the cursor was drawn, in cells of the pane, as laid out last.
    cursor_cell: Option<(usize, usize)>,
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
    /// The code actions answered to `gra`, in a menu by the cursor, until
    /// one's picked.
    actions: Option<ActionMenu>,
    /// The number of the code action picked last, until it's done: as a
    /// rename does, it takes a while, and `K` or `gd` meanwhile mustn't
    /// lose its answer.
    acting: Option<u64>,
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
    /// A command waiting for the daemon to say where the pieces of code
    /// it needs are, and the keys typed meanwhile.
    awaiting: Option<pieces::Awaiting>,
    /// The daemon's answer, while the command that waited for it is done.
    answered: Option<Answer>,
    /// Whether a command waiting was given up on, for the desk to drop
    /// the keys it held for it.
    gave_up: bool,
    /// Whether what the bar says stays through the next key.
    keep_status: bool,
    /// Whether the key being read comes from a macro.
    replaying: bool,
    /// Whether the command waiting for the grammar is a macro's.
    resume_replaying: bool,
    /// New lines given an indent at once, waiting for the grammar to say
    /// what it should be.
    corrections: Vec<indent::Correction>,
    /// The keys to note in a macro being recorded for the key just typed,
    /// if a menu took it: what it put in, or nothing.
    menu_keys: Option<Vec<Stroke>>,
    /// Whether a `:g` is doing its command on each line.
    in_global: bool,
    /// How many matches `:s` has replaced, or counted, on how many lines,
    /// for `:g` to tell of once it's done.
    counted_matches: (usize, usize),
    /// What the command being typed would do, shown in place
    /// (`inccommand`).
    preview: Option<Preview>,
    inccommand: bool,
    /// `~` is an operator, taking a motion (`tildeop`).
    tildeop: bool,
    /// The pattern compiled last, kept to compile it once (see
    /// [`ex`]'s `pattern`).
    compiled: std::cell::RefCell<Option<Compiled>>,
    /// The text as one string, for searching, and the buffer and revision
    /// it's of (see [`ex`]'s `haystack`).
    whole: std::cell::RefCell<Option<Whole>>,
    /// A macro to type, `@` asked for, and how many times: the desk types
    /// it, as a macro can go on in other panes.
    macro_asked: Option<(Vec<Chord>, usize)>,
    /// Whether a key failed since the desk last asked, as a motion that
    /// can't move does, which stops the macros being typed.
    failed: bool,
    /// The snippet being filled in, which a completion put in.
    snippet: Option<snippet::Session>,
    /// Whether the snippet was put in by the key being handled, whose
    /// edit isn't typing in it.
    snippet_started: bool,
    /// Whether the edit being made copies what's typed in the snippet to
    /// its mirrors.
    mirroring: bool,
    /// What the snippet had left before the key being handled.
    snippet_before: Option<snippet::Left>,
    /// The keys that type what a snippet left, for a macro being recorded
    /// to note before the next key.
    snippet_keys: Option<Vec<Stroke>>,
    /// The text of the snippet's place that's selected, lit, as last
    /// settled.
    snippet_lit: Option<(Point, Point)>,
    /// Whether the grammar put the last new line's indent right, so a
    /// snippet's keys in a macro leave indents to it.
    indents_by_grammar: bool,
}

impl Editor {
    pub(crate) fn new(buffer: BufferId, options: &Options) -> Self {
        Self {
            buffer,
            mode: Mode::Normal,
            cursor: Cursor::new(Point { line: 0, col: 0 }),
            anchor: None,
            top: Top { line: 0, cell: 0 },
            left: 0,
            resized: false,
            kept: Kept::default(),
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
            wrapping: Wrapping::of(options),
            case: Case::of(options),
            shown: Vec::new(),
            cursor_cell: None,
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
            actions: None,
            acting: None,
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
            awaiting: None,
            answered: None,
            gave_up: false,
            keep_status: false,
            replaying: false,
            resume_replaying: false,
            corrections: Vec::new(),
            menu_keys: None,
            in_global: false,
            counted_matches: (0, 0),
            preview: None,
            inccommand: options.inccommand == crate::options::InCommand::NoSplit,
            tildeop: options.tildeop,
            compiled: std::cell::RefCell::new(None),
            whole: std::cell::RefCell::new(None),
            macro_asked: None,
            failed: false,
            snippet: None,
            snippet_started: false,
            mirroring: false,
            snippet_before: None,
            snippet_keys: None,
            snippet_lit: None,
            indents_by_grammar: false,
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
        let (height, width) = (usize::from(size.rows()), usize::from(size.cols()));
        self.resized |= (height, width) != (self.height, self.width);
        self.height = height;
        self.width = width;
        if self.width < self.gutter + LEAST_TEXT {
            // Until the next settle works out the numbers afresh.
            self.gutter = 0;
        }
    }

    /// Takes in the options, as they've changed.
    pub(crate) fn set_options(&mut self, options: &Options, copy: &BufferCopy) {
        self.inccommand = options.inccommand == crate::options::InCommand::NoSplit;
        self.tildeop = options.tildeop;
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
        let wrapping = Wrapping::of(options);
        if wrapping.on != self.wrapping.on {
            self.top.cell = 0;
            self.left = 0;
        }
        self.wrapping = wrapping;
        self.case = Case::of(options);
        self.as_you_type = AsYouType::of(options);
        if !self.as_you_type.signature {
            self.close_signature();
        }
        self.follow(copy);
    }

    /// The buffer's text, laid out as the options say, wrapped at the
    /// pane's width right of the gutter.
    fn text<'c>(&self, copy: &'c BufferCopy) -> Text<'c> {
        let typing = (self.mode == Mode::Insert).then_some(self.cursor.at);
        Text::new(copy.text(), self.indent.tabstop)
            .wrapping(self.wrapping.at(self.text_width().max(1)), typing)
            .keeping(&self.kept, self.buffer, copy.revision())
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
            Some(Prompt::Confirm(confirming)) => return confirming.question(),
            None => {}
        }
        if let Some(status) = &self.status {
            return status.clone();
        }
        let modified = if copy.modified() { " [+]" } else { "" };
        let at = self.cursor.at;
        let label = format!("{}{modified}  {}:{}", copy.name(), at.line + 1, at.col + 1);
        match self.diagnostic_here(copy) {
            Some(diagnostic) => format!("{label}  {}", diagnostic.described()),
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

    /// Does what `press` does. What every editor shares is in `shared`.
    #[cfg(test)]
    pub(crate) fn key(
        &mut self,
        press: &KeyPress<'_>,
        copy: &mut BufferCopy,
        shared: &mut Shared,
    ) -> Done {
        let mut done = Done::default();
        for stroke in self.strokes(press) {
            done.redraw |= self.typed(stroke, copy, shared, false).redraw;
        }
        done
    }

    /// The keys `press` makes, for [`typed`](Self::typed).
    pub(crate) fn strokes(&self, press: &KeyPress<'_>) -> Vec<Stroke> {
        strokes(press, self.mode == Mode::Insert || self.prompt.is_some())
    }

    /// Does what `stroke`, typed, does: by a macro, if `replaying`. The
    /// desk records macros and types them (see `desk::macros`); a macro
    /// `@` asks for is [`take_macro`](Self::take_macro)'s, and a key a
    /// menu took is noted for [`take_menu_keys`](Self::take_menu_keys).
    pub(crate) fn typed(
        &mut self,
        stroke: Stroke,
        copy: &mut BufferCopy,
        shared: &mut Shared,
        replaying: bool,
    ) -> Done {
        self.menu_keys = None;
        self.failed = false;
        self.replaying = replaying;
        self.snippet_before_key(copy);
        let done = self.stroke(stroke, copy, shared);
        self.snippet_record(stroke);
        self.replaying = false;
        // Its command waits for the grammar: it's done as the macro's,
        // in its undo step, when the answer comes, and the desk types the
        // rest after.
        if replaying {
            self.resume_replaying = self.awaiting.is_some();
        }
        done
    }

    /// What the key just typed is to be noted as in a macro being
    /// recorded, if a menu took it: what it put in, or nothing, so typing
    /// the macro again doesn't need the menu, which comes only when the
    /// language server answers.
    pub(crate) fn take_menu_keys(&mut self) -> Option<Vec<Stroke>> {
        self.menu_keys.take()
    }

    /// The macro `@` asked for since last asked, and how many times to
    /// type it.
    pub(crate) fn take_macro(&mut self) -> Option<(Vec<Chord>, usize)> {
        self.macro_asked.take()
    }

    /// Whether a key failed since last asked, which stops the macros
    /// being typed, as in Vim.
    pub(crate) fn take_failed(&mut self) -> bool {
        std::mem::take(&mut self.failed)
    }

    /// Forgets the keys of a command half typed, as when the macro typing
    /// them is stopped.
    pub(crate) fn forget_typed(&mut self) {
        self.typed.clear();
    }

    /// Whether the editor waits for the language server or the daemon to
    /// answer something the keys after may depend on, asked as query
    /// `since` or later: where `gd` goes, a menu or panel to show, a
    /// rename or a stage, or a new line's indent; or an undo. A macro waits
    /// for it before its next key.
    pub(crate) fn waiting_for_answer(&self, copy: &BufferCopy, since: u64) -> bool {
        let new = |query: u64| query >= since;
        self.asked.is_some_and(|(query, _)| new(query))
            || self.acting.is_some_and(new)
            || self.renaming.is_some_and(new)
            || self.staging.iter().any(|&query| new(query))
            || self.correcting_since(since)
            || copy.reversing()
    }

    /// The connection to the daemon was lost: what was asked of it will
    /// never be answered.
    pub(crate) fn reconnected(&mut self) {
        self.asked = None;
        self.acting = None;
        self.renaming = None;
        self.staging.clear();
        self.corrections.clear();
    }

    /// A key failed, as a motion that can't move does: the macros being
    /// typed stop, as in Vim.
    fn fail(&mut self) {
        self.failed = true;
    }

    /// Puts `text` in at the cursor, as typed in insert mode.
    pub(crate) fn paste(&mut self, text: &str, copy: &mut BufferCopy) {
        self.panel = None;
        self.actions = None;
        self.close_completion();
        let edits = copy.edits_made();
        // In place of a snippet's place's text, if it's selected.
        let selected = self.snippet.as_mut().and_then(|session| {
            let selected = session.selected();
            session.selected = false;
            selected
        });
        if let Some((start, end)) = selected {
            self.delete_text(start, end, copy);
        }
        let at = self.insert_byte(copy);
        self.new_step |= self.mode != Mode::Insert;
        self.insert_text(at, text, copy);
        if self.mode != Mode::Insert {
            self.new_step = true;
            self.normal_cursor(copy);
        }
        self.snippet_after(edits, copy);
    }

    fn stroke(&mut self, stroke: Stroke, copy: &mut BufferCopy, shared: &mut Shared) -> Done {
        // Shift+Tab is Tab but in a snippet being filled in.
        let stroke = match stroke {
            Stroke::BackTab if self.snippet.is_none() || self.mode != Mode::Insert => Stroke::Tab,
            stroke => stroke,
        };
        let edits = copy.edits_made();
        let done = self.read_key(stroke, copy, shared);
        self.snippet_after(edits, copy);
        done
    }

    /// Does what `stroke` does, but for the snippet being filled in.
    fn read_key(&mut self, stroke: Stroke, copy: &mut BufferCopy, shared: &mut Shared) -> Done {
        if self.hold_stroke(stroke) {
            return Done::default();
        }
        // The selection as each key comes is the last, for `'<` and `'>`,
        // which the key's edits then move.
        if matches!(self.mode, Mode::Visual(_)) {
            self.mark_selection(copy, &mut shared.marks);
        }
        if !std::mem::take(&mut self.keep_status) {
            self.status = None;
        }
        // A macro types past menus, which it can't know will be there.
        if self.actions.is_some()
            && !self.replaying
            && self.typed.is_empty()
            && self.prompt.is_none()
            && self.mode == Mode::Normal
            && let Some(done) = self.actions_stroke(stroke, copy)
        {
            self.menu_keys.get_or_insert_with(Vec::new);
            return done;
        }
        if self.panel.is_some()
            && self.typed.is_empty()
            && self.prompt.is_none()
            && let Some(done) = self.panel_stroke(stroke)
        {
            return done;
        }
        if self.mode == Mode::Insert
            && !self.replaying
            && self.prompt.is_none()
            && let Some(done) = self.completion_stroke(stroke, copy)
        {
            self.menu_keys.get_or_insert_with(Vec::new);
            self.follow(copy);
            return done;
        }
        if self.mode == Mode::Insert && self.prompt.is_none() {
            // A snippet's choices go with any key their menu doesn't take.
            if self
                .completion
                .as_ref()
                .is_some_and(|completion| completion.choices)
            {
                self.close_completion();
            }
            if let Some(done) = self.snippet_stroke(stroke, copy) {
                self.follow(copy);
                return done;
            }
        }
        let (mode, edits) = (self.mode, copy.edits_made());
        // A macro types on without asking the language server.
        let asks = mode == Mode::Insert && !self.replaying;
        let at = asks.then(|| self.cursor_byte(copy));
        let done = self.read_stroke(stroke, copy, shared);
        self.follow(copy);
        if asks {
            let changed = copy.edits_made() != edits || Some(self.cursor_byte(copy)) != at;
            self.as_you_type(stroke, changed, copy);
        }
        // Changing mode or typing in the bar closes the menu of code
        // actions, whose keys are normal mode's.
        if self.mode != mode || self.prompt.is_some() {
            self.actions = None;
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

    /// The keys the menu of code actions takes while it's shown: j, k, the
    /// arrows, Tab, Ctrl+N and Ctrl+P move through it; Enter does the
    /// action picked, and 1 to 9 the action so numbered; Esc, `q` and
    /// Ctrl+C close it. `None` for any other key, which closes it and goes
    /// on to do what it does.
    fn actions_stroke(&mut self, stroke: Stroke, copy: &mut BufferCopy) -> Option<Done> {
        let menu_stroke = match stroke {
            Stroke::Char {
                ch: 'j',
                ctrl: false,
            }
            | Stroke::Char {
                ch: 'n',
                ctrl: true,
            }
            | Stroke::Down
            | Stroke::Tab => MenuStroke::Down,
            Stroke::Char {
                ch: 'k',
                ctrl: false,
            }
            | Stroke::Char {
                ch: 'p',
                ctrl: true,
            }
            | Stroke::Up => MenuStroke::Up,
            Stroke::Enter => MenuStroke::Pick,
            Stroke::Char {
                ch: digit @ '1'..='9',
                ctrl: false,
            } => MenuStroke::Number(u8::try_from(digit.to_digit(10)?).ok()?),
            Stroke::Escape
            | Stroke::Char {
                ch: 'q',
                ctrl: false,
            }
            | Stroke::Char {
                ch: 'c',
                ctrl: true,
            } => MenuStroke::Close,
            _ => {
                self.actions = None;
                return None;
            }
        };
        let menu = self.actions.as_mut()?;
        match menu.key(menu_stroke) {
            Some(MenuKey::Moved) | None => {}
            Some(MenuKey::Picked(item)) => self.act(item, copy),
            Some(MenuKey::Closed) => self.actions = None,
        }
        Some(Done::REDRAW)
    }

    /// Does action `item` of the menu, closing it: the daemon makes its
    /// edits and runs its command, then says how it went, in the bar. One
    /// it says can't be done now isn't asked for.
    fn act(&mut self, item: u32, copy: &mut BufferCopy) {
        let Some(menu) = self.actions.take() else {
            return;
        };
        let Some(action) = menu.action(item) else {
            return;
        };
        if let Some(why) = &action.disabled {
            self.status = Some(format!("{}: {why}", action.title));
            return;
        }
        let query = Query::Act {
            list: menu.list,
            item,
        };
        self.acting = Some(copy.ask(query, 0));
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
        Some(Done::REDRAW)
    }

    fn read_stroke(&mut self, stroke: Stroke, copy: &mut BufferCopy, shared: &mut Shared) -> Done {
        if self.prompt.is_some() {
            return self.prompt_stroke(stroke, copy, shared);
        }
        if self.mode == Mode::Insert {
            if let Some(change) = &mut self.changing
                && stroke != Stroke::Escape
            {
                change.typed.push(Typed::Key(stroke));
            }
            self.insert_stroke(stroke, copy);
            return Done::REDRAW;
        }
        if stroke == Stroke::Escape && self.typed.is_empty() {
            self.mode = Mode::Normal;
            self.anchor = None;
            self.found = None;
            return Done::REDRAW;
        }
        // `q` stops a macro being recorded, whatever comes after.
        if stroke == Stroke::char('q')
            && self.typed.is_empty()
            && shared.registers.recording().is_some()
        {
            shared.registers.stop_recording();
            return Done::REDRAW;
        }
        self.typed.push(stroke);
        let visual = matches!(self.mode, Mode::Visual(_));
        match keys::parse_with(&self.typed, visual, self.tildeop) {
            Parsed::Incomplete => Done::default(),
            Parsed::Nothing => {
                self.typed.clear();
                self.fail();
                Done::default()
            }
            Parsed::Command(command) => {
                self.typed.clear();
                self.execute(command, copy, shared, false)
            }
        }
    }

    /// Does `command`; again, for `.`, if `again`.
    fn execute(
        &mut self,
        command: Command,
        copy: &mut BufferCopy,
        shared: &mut Shared,
        again: bool,
    ) -> Done {
        if self.waits_for_grammar(command, copy, &shared.marks) {
            return Done::default();
        }
        let done = Done::REDRAW;
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
                let motion = self.by_rows(motion);
                self.cursor = self.moved(copy, motion, times);
                if self.cursor.at == from && can_fail(motion) {
                    self.fail();
                }
            }
            Action::GoToLine => {
                let text = self.text(copy);
                let line = (times as u64).saturating_sub(1).min(text.last_line());
                self.cursor = Cursor::new(Point { line, col: 0 });
                self.cursor.apply(Motion::FirstNonBlank, &text);
            }
            Action::Operate(..)
            | Action::DeleteChars { .. }
            | Action::DeleteToEnd { .. }
            | Action::Substitute
            | Action::Put { .. } => self.keep_or_put(command, copy, shared),
            Action::Join => self.join(times.max(2) - 1, copy),
            Action::Replace(ch) => self.replace_chars(ch, times, copy),
            Action::Insert(entry) => self.insert(entry, times, copy),
            Action::Undo | Action::Redo => {
                for _ in 0..times {
                    copy.reverse(command.action == Action::Undo);
                }
            }
            Action::Repeat => return self.repeat(command.count, copy, shared),
            Action::Visual { lines } => self.visual(lines),
            Action::Search { backward } => self.prompt = Some(Prompt::search(backward)),
            Action::SearchAgain { reverse } => self.search_again(reverse, copy, shared),
            Action::SearchWord { backward } => self.search_word(backward, copy, shared),
            Action::Tilde => self.tilde(times, copy),
            Action::RepeatSubstitute { all } => self.repeat_substitute(all, copy, shared),
            Action::FindAgain { reverse } => self.find_again(reverse, times, copy),
            Action::HalfPage(direction) => self.page(i64::from(direction), self.height / 2, copy),
            Action::Page(direction) => {
                self.page(i64::from(direction), self.height.saturating_sub(2), copy);
            }
            Action::View(at) => self.view_at(at, copy),
            Action::Diagnostic { backward } => self.next_diagnostic(backward, times, copy),
            Action::Hunk { backward } => self.next_hunk(backward, times, copy),
            Action::Ask(_)
            | Action::Hover
            | Action::ShowDiagnostics
            | Action::Rename
            | Action::CodeActions => {
                self.language_server(command.action, copy);
            }
            Action::OtherEnd => {
                if let Some(anchor) = self.anchor {
                    self.anchor = Some(self.cursor.at);
                    self.cursor = Cursor::new(anchor);
                }
            }
            Action::Piece {
                construct,
                backward,
            } => self.go_to_piece(construct, backward, copy),
            Action::Select { object, inner } => self.select(object, inner, times, copy),
            Action::Record(name) => shared.registers.record(name),
            Action::Replay(name) => match shared.registers.macro_keys(name) {
                Ok(keys) => self.macro_asked = Some((keys, times)),
                Err(why) => {
                    self.status = Some(why);
                    self.fail();
                }
            },
            Action::SetMark(mark) => self.set_mark(mark, copy, &mut shared.marks),
            Action::Mark { mark, exact } => self.go_to_mark(mark, exact, copy, &shared.marks),
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

    /// `&`, or `g&` if `all`, which is `:%s//~/&`, as in Vim: the last
    /// `:s` again.
    fn repeat_substitute(&mut self, all: bool, copy: &mut BufferCopy, shared: &mut Shared) {
        let line = if all { "%s//~/&" } else { "s" };
        if let Err(why) = self.run(line, copy, shared) {
            self.status = Some(why);
            self.fail();
        }
    }

    /// `i` or `a` and `object`, in visual mode: selects it, the `times`th
    /// out if it's a piece of code.
    fn select(&mut self, object: Object, inner: bool, times: usize, copy: &BufferCopy) {
        if let Object::Code(construct) = object {
            return self.select_piece(construct, inner, times, copy);
        }
        let text = self.text(copy);
        if let Some((start, last)) = text.object(object, self.cursor.at, inner) {
            self.mode = Mode::Visual(Selection::Characters);
            self.anchor = Some(start);
            self.cursor = Cursor::new(last);
        }
    }

    /// Does `command`, which keeps text in a register or puts it from one:
    /// an operator, `x`, `D`, `s` or `p` and their kin. Fails if there's
    /// nothing to act on.
    fn keep_or_put(&mut self, command: Command, copy: &mut BufferCopy, shared: &mut Shared) {
        let (times, name) = (command.times(), command.register);
        let registers = (&mut shared.registers, name);
        match command.action {
            Action::Operate(operator, target) => {
                if !self.operate(operator, target, times, copy, &shared.marks, registers) {
                    self.fail();
                }
            }
            Action::DeleteChars { before } => self.delete_chars(before, times, copy, registers),
            Action::DeleteToEnd { change } => self.delete_to_end(change, times, copy, registers),
            Action::Substitute => {
                self.delete_chars(false, times, copy, registers);
                self.enter_insert(1);
            }
            Action::Put { before } if matches!(self.mode, Mode::Visual(_)) => {
                self.put_over(before, times, copy, registers);
            }
            Action::Put { before } => self.put(before, times, copy, (&shared.registers, name)),
            _ => {}
        }
    }

    /// `;`, or `,` if `reverse`: the last `f`, `t`, `F` or `T` again, `times`
    /// over. Fails if it can't move.
    fn find_again(&mut self, reverse: bool, times: usize, copy: &BufferCopy) {
        let from = self.cursor.at;
        if let Some(Motion::Find { ch, forward, till }) = self.last_find {
            let motion = Motion::Find {
                ch,
                forward: forward != reverse,
                till,
            };
            self.cursor = self.moved(copy, motion, times);
        }
        if self.cursor.at == from {
            self.fail();
        }
    }

    /// `.`: makes the last change again, `count` times if given. A change
    /// that put text from a numbered register puts the next's, as in Vim,
    /// so `"1p` then `.` goes back through what was deleted.
    fn repeat(&mut self, count: Option<usize>, copy: &mut BufferCopy, shared: &mut Shared) -> Done {
        let Some(mut change) = self.last_change.clone() else {
            return Done::default();
        };
        if count.is_some() {
            change.command.count = count;
        }
        if let (Action::Put { .. }, Some(digit @ '1'..='8')) =
            (change.command.action, change.command.register)
        {
            change.command.register = char::from_digit(digit as u32 - '0' as u32 + 1, 10);
        }
        let done = self.execute(change.command, copy, shared, true);
        // A command waiting for the daemon finishes once it answers.
        if let Some(change) = self.awaiting_again(change) {
            self.finish_repeat(change, copy);
        }
        done
    }

    /// Finishes `.` making `change` again, once its command is done: types
    /// what was typed after it.
    fn finish_repeat(&mut self, change: Change, copy: &mut BufferCopy) {
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
    }

    /// Notes `motion` if it's a find, for `;` and `,`.
    fn remember_find(&mut self, motion: Motion) -> Motion {
        if matches!(motion, Motion::Find { .. }) {
            self.last_find = Some(motion);
        }
        motion
    }

    /// `motion` as it goes here: where lines don't wrap, `gj` and `gk` are
    /// `j` and `k`, as in Vim, taking whole lines with an operator.
    fn by_rows(&self, motion: Motion) -> Motion {
        match motion {
            Motion::RowUp(rows) if !self.wrapping.on => Motion::Up(rows),
            Motion::RowDown(rows) if !self.wrapping.on => Motion::Down(rows),
            motion => motion,
        }
    }

    /// The cursor moved by `motion`, `times` times.
    fn moved(&self, copy: &BufferCopy, motion: Motion, times: usize) -> Cursor {
        self.moved_seeing(copy, motion, times)
            .unwrap_or(self.cursor)
    }

    /// The cursor moved by `motion`, `times` times, or `None` if it reads the
    /// rows, and might not have seen where it should go, past the end of a
    /// long line's row.
    fn moved_seeing(&self, copy: &BufferCopy, motion: Motion, times: usize) -> Option<Cursor> {
        let text = self.text(copy);
        let mut cursor = self.cursor;
        match motion {
            Motion::Up(_) => cursor.apply(Motion::Up(times as u64), &text),
            Motion::Down(_) => cursor.apply(Motion::Down(times as u64), &text),
            Motion::RowUp(_) | Motion::RowDown(_) => {
                let times = times as u64;
                let up = matches!(motion, Motion::RowUp(_));
                let rows = if up {
                    Motion::RowUp(times)
                } else {
                    Motion::RowDown(times)
                };
                cursor.apply(rows, &text);
                wrap::onto_row(&mut cursor, &text, up);
            }
            // Where lines don't wrap, the first and last columns in view,
            // as in Vim.
            Motion::RowStart | Motion::RowEnd if !text.wraps() => {
                if let Some(laid) = text.laid(cursor.at.line).filter(|laid| laid.cells() > 0) {
                    let last = laid.cells() - 1;
                    let col = match motion {
                        Motion::RowStart => self.left.min(last),
                        _ => (self.left + self.text_width().max(1) - 1).min(last),
                    };
                    let byte = laid.byte_at(col);
                    let mut col = laid.col_of(byte);
                    // Begun left of the view: the character after it.
                    let next = laid.char_end(byte);
                    if col < self.left && next < laid.text().len() {
                        col = laid.col_of(next);
                    }
                    cursor = Cursor::new(Point {
                        line: cursor.at.line,
                        col,
                    });
                }
            }
            // A character at a time, however many cells it takes, as a tab
            // does, and on a line longer than a row holds, past the row.
            Motion::Left | Motion::Right => {
                let line = cursor.at.line;
                let Some(laid) = text.laid(line) else {
                    return Some(cursor);
                };
                for _ in 0..times {
                    let byte = laid.byte_at(cursor.at.col);
                    let to = if motion == Motion::Left {
                        laid.col_before(laid.col_of(byte))
                    } else {
                        let next = laid.char_end(byte);
                        (next < laid.text().len())
                            .then(|| laid.col_of(next))
                            .filter(|&next| next <= text.last_cell(line))
                    };
                    let Some(col) = to else {
                        break;
                    };
                    cursor = Cursor::new(Point { line, col });
                }
            }
            // What reads the Vim engine's rows can't see past the end of a
            // long line's: it goes nowhere, rather than somewhere wrong.
            _ if reads_row(motion) && text.past_row(cursor.at) => return None,
            _ => {
                for _ in 0..times {
                    cursor.apply(motion, &text);
                }
                let (start, end) = (cursor.at.min(self.cursor.at), cursor.at.max(self.cursor.at));
                if reads_row(motion) && text.past_rows(start, end) {
                    return None;
                }
            }
        }
        Some(cursor)
    }

    // --- Operators ----------------------------------------------------------

    /// Makes `operator` act on `target`, keeping the text in `registers`
    /// under `name`, as [`Registers::wrote`] keeps it, unless it indents.
    /// Returns whether there was anything to act on: not if the motion
    /// couldn't move, the object isn't there, or the mark isn't set.
    fn operate(
        &mut self,
        operator: Operator,
        target: Target,
        times: usize,
        copy: &mut BufferCopy,
        marks: &Marks,
        (registers, name): (&mut Registers, Option<char>),
    ) -> bool {
        if operator == Operator::Reindent {
            // As the grammar answered, for the lines it found.
            return self.reindent(copy);
        }
        let span = match target {
            Target::Mark { mark, exact } => self.mark_span(mark, exact, copy, marks),
            _ => self.span(operator, target, times, copy),
        };
        let Some(span) = span else {
            return false;
        };
        if let Operator::Recase(recase) = operator {
            self.recase(span.start, span.end, recase, copy);
            // To the start, as Vim goes, but for whole lines typed, which
            // keep the cursor's column when there are more than one.
            let text = self.text(copy);
            let col = self.cursor.at.col;
            self.cursor = match span.lines {
                Some((first, last)) if first < last && target != Target::Selection => {
                    Cursor::wanting(Point { line: first, col }, col)
                }
                Some((first, _)) => Cursor::new(Point {
                    line: first,
                    col: 0,
                }),
                None => Cursor::new(text.point_of(span.start)),
            };
            drop(text);
            self.leave_visual();
            return true;
        }
        if let Operator::Indent | Operator::Dedent = operator {
            // A count is how many lines, or for a selection, how many steps.
            let steps = if target == Target::Selection {
                times
            } else {
                1
            };
            self.shift(span, steps, operator == Operator::Dedent, copy);
            return true;
        }
        let mut text = self.text(copy).slice(span.start, span.end);
        let lines = span.lines.is_some();
        if lines && !text.ends_with('\n') {
            text.push('\n');
        }
        let wrote = if operator == Operator::Yank {
            Wrote::Yank
        } else {
            let small = !lines && !text.contains('\n');
            // As in Vim, a delete within a line goes to `"1` too if it's
            // over one of these motions, which usually go further.
            let far = matches!(
                target,
                Target::Mark { .. } | Target::Motion(Motion::Match | Motion::Paragraph { .. })
            );
            Wrote::Delete {
                small,
                history: !small || far,
            }
        };
        registers.wrote(name, Register { text, lines }, wrote);
        match operator {
            Operator::Yank => self.yanked(span, copy),
            Operator::Delete => self.delete_span(span, copy),
            Operator::Change => self.change_span(span, copy),
            Operator::Indent | Operator::Dedent | Operator::Recase(_) | Operator::Reindent => {}
        }
        true
    }

    /// What an operator acting up to `mark` acts on: its line and the
    /// cursor's, and those between, or exactly, up to the mark but not
    /// it, as an exclusive motion. `None` if it isn't set here.
    fn mark_span(
        &mut self,
        mark: char,
        exact: bool,
        copy: &BufferCopy,
        marks: &Marks,
    ) -> Option<Span> {
        let to = self.mark_here(mark, copy, marks)?;
        let text = self.text(copy);
        let at = self.cursor.at;
        let (start, end) = if to < at { (to, at) } else { (at, to) };
        if exact {
            return exclusive(&text, start, end, false);
        }
        Some(Span {
            start: text.line_start(start.line),
            end: text.line_start(end.line + 1),
            lines: Some((start.line, end.line)),
        })
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
            // Marks are the window's, which `mark_span` is given.
            Target::Mark { .. } => None,
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
            Target::Object {
                object: Object::Code(construct),
                inner,
            } => {
                drop(text);
                let (start, end) = self.piece_span(construct, inner, times, copy)?;
                Some(Span {
                    start,
                    end,
                    lines: None,
                })
            }
            Target::Object { object, inner } => {
                let Some((start, last)) = text.object(object, at, inner) else {
                    self.status = Some(match object {
                        Object::Block => "a file has no shell blocks".into(),
                        _ => "nothing there".into(),
                    });
                    return None;
                };
                Some(chars(start, last))
            }
            Target::Motion(motion) => {
                let motion = self.remember_find(motion);
                let motion = self.by_rows(motion);
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
                    // Not past the end of a long line's row, which it
                    // can't see.
                    let seen = !text.past_row(at) && !text.past_rows(at, cursor.at);
                    return seen.then(|| chars(at, cursor.at));
                }
                let moved = self.moved_seeing(copy, motion, times)?;
                // `dj` on the last line has no line to take with it.
                if matches!(motion, Motion::Up(_) | Motion::Down(_)) && moved.at.line == at.line {
                    return None;
                }
                let (start, end) = if moved.at < at {
                    (moved.at, at)
                } else {
                    (at, moved.at)
                };
                match self.reach(motion, at, moved.at, &text)? {
                    Reach::Lines => Some(lines(start.line, end.line)),
                    Reach::Inclusive => Some(chars(start, end)),
                    Reach::Exclusive => exclusive(&text, start, end, word),
                }
            }
        }
    }

    /// What an operator takes with `motion` from `from` to `to`, as
    /// [`Motion::reach_from`] says, but after `$`, `gj` and `gk` take the
    /// character they go to, as in Vim.
    fn reach(&self, motion: Motion, from: Point, to: Point, text: &Text<'_>) -> Option<Reach> {
        let rows = matches!(motion, Motion::RowUp(_) | Motion::RowDown(_));
        match motion.reach_from(from, to, text)? {
            Reach::Exclusive if rows && self.cursor.want() == usize::MAX => Some(Reach::Inclusive),
            reach => Some(reach),
        }
    }

    /// `x`, or `X` if `before`: `times` characters at the cursor, or
    /// before it, on its line, kept in `registers` under `name`.
    fn delete_chars(
        &mut self,
        before: bool,
        times: usize,
        copy: &mut BufferCopy,
        (registers, name): (&mut Registers, Option<char>),
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
        let small = Wrote::Delete {
            small: true,
            history: false,
        };
        registers.wrote(name, Register::chars(removed), small);
        self.make(edit, copy);
        self.cursor = Cursor::new(self.text(copy).point_of(line_start + start));
    }

    /// `D`, or `C` if `change`: from the cursor to its line's end, and to
    /// the end of `times - 1` lines more, kept in `registers` under `name`.
    fn delete_to_end(
        &mut self,
        change: bool,
        times: usize,
        copy: &mut BufferCopy,
        (registers, name): (&mut Registers, Option<char>),
    ) {
        let text = self.text(copy);
        let at = self.cursor.at;
        let start = text.byte_at(at);
        let last = (at.line + times as u64 - 1).min(text.last_line());
        let end = text.line_end(last).max(start);
        let removed = text.slice(start, end);
        let edit = Edit::replace(text.rope().len_bytes(), start, &removed, "");
        drop(text);
        let small = !removed.contains('\n');
        let wrote = Wrote::Delete {
            small,
            history: !small,
        };
        registers.wrote(name, Register::chars(removed), wrote);
        self.make(edit, copy);
        self.cursor = Cursor::new(self.text(copy).point_of(start));
        if change {
            self.enter_insert(1);
        }
    }

    /// `p`, or `P` if `before`: what the register `name` holds, or the
    /// unnamed, `times` over.
    fn put(
        &mut self,
        before: bool,
        times: usize,
        copy: &mut BufferCopy,
        (registers, name): (&Registers, Option<char>),
    ) {
        let Some(register) = self.register(registers, name) else {
            return;
        };
        let register = register.clone();
        self.put_register(before, times, copy, &register);
    }

    /// The register `name`, or the unnamed, if it holds anything; if not,
    /// says so, and fails.
    fn register<'r>(
        &mut self,
        registers: &'r Registers,
        name: Option<char>,
    ) -> Option<&'r Register> {
        let register = registers.get(name);
        if register.is_none() {
            let name = name.unwrap_or('"');
            self.status = Some(format!("register {name} is empty"));
            self.fail();
        }
        register
    }

    /// `p` and `P` in visual mode: puts what the register `name` holds, or
    /// the unnamed, `times` over, in place of what's selected. What was selected
    /// is kept as a delete, after `p` but not `P`, as in Vim, so `P` can
    /// put the same text over one selection after another.
    fn put_over(
        &mut self,
        before: bool,
        times: usize,
        copy: &mut BufferCopy,
        (registers, name): (&mut Registers, Option<char>),
    ) {
        let Some(register) = self.register(registers, name).cloned() else {
            self.leave_visual();
            return;
        };
        let Some(span) = self.span(Operator::Delete, Target::Selection, 1, copy) else {
            return;
        };
        let mut taken = self.text(copy).slice(span.start, span.end);
        let lines = span.lines.is_some();
        if lines && !taken.ends_with('\n') {
            taken.push('\n');
        }
        let len = self.text(copy).rope().len_bytes();
        // Whole lines taken from the end leave none after to put before.
        let at_end = lines && span.end >= len;
        self.delete_span(span, copy);
        if !before {
            let small = !lines && !taken.contains('\n');
            let wrote = Wrote::Delete {
                small,
                history: !small,
            };
            registers.wrote(None, Register { text: taken, lines }, wrote);
        }
        match (lines, register.lines) {
            // Lines in place of characters go between the two halves of the
            // line, which is broken.
            (false, true) => {
                let byte = self.cursor_byte(copy);
                let put = format!("\n{}", register.text.repeat(times));
                let edit = Edit::replace(copy.text().len_bytes(), byte, "", &put);
                self.make(edit, copy);
                let text = self.text(copy);
                self.cursor = Cursor::new(text.point_of(byte + 1));
                self.cursor.apply(Motion::FirstNonBlank, &text);
            }
            // Characters in place of lines go on a line of their own.
            (true, false) => {
                let mut register = register;
                register.text.push('\n');
                register.lines = true;
                self.put_register(!at_end, times, copy, &register);
            }
            _ => self.put_register(!at_end, times, copy, &register),
        }
    }

    /// Puts `register`, `times` over, after the cursor or its line, or
    /// before them if `before`.
    fn put_register(
        &mut self,
        before: bool,
        times: usize,
        copy: &mut BufferCopy,
        register: &Register,
    ) {
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
                drop(text);
                self.ask_indent(line, copy);
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
        self.end_snippet(copy, false);
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
                self.typed_indent_key(ch, copy);
            }
            Stroke::Enter => {
                let text = self.text(copy);
                let line = text.point_of(at).line;
                let line_text = text.line_text(line);
                let before_cursor = at - text.line_start(line);
                let indent = indent_of(&line_text[..before_cursor.min(line_text.len())]).to_owned();
                drop(text);
                self.insert_text(at, &format!("\n{indent}"), copy);
                self.ask_indent(line + 1, copy);
            }
            Stroke::Tab | Stroke::BackTab => {
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
                self.moved_while_inserting(copy);
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
                indented,
            } => {
                let at = self.insert_byte(copy);
                let start = {
                    let rope = copy.text();
                    let char_at = rope.byte_to_char(at);
                    rope.char_to_byte(char_at.saturating_sub(*replaced))
                };
                // A snippet's lines after the first, indented as this one.
                let (text, offset) = if *indented {
                    let line = self.text(copy).line_text(self.cursor.at.line);
                    snippet::indented(text, indent_of(&line), *offset)
                } else {
                    (text.clone(), *offset)
                };
                let removed = copy.text().byte_slice(start..at).to_string();
                let edit = Edit::replace(copy.text().len_bytes(), start, &removed, &text);
                self.make(edit, copy);
                self.cursor = Cursor::new(self.text(copy).point_of(start + offset));
            }
        }
    }

    /// The cursor moved in insert mode other than by typing: what's typed
    /// next starts a new undo step, and isn't part of what `.` makes
    /// again, as in Vim.
    fn moved_while_inserting(&mut self, copy: &BufferCopy) {
        self.snippet_moved(copy);
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
        let grows = self.grows(copy);
        self.edited(&edit, grows);
        // A macro's edits to a file, however many, are one undo step, as
        // in Vim, which its first starts.
        let step = if self.replaying {
            copy.first_macro_edit()
        } else {
            self.new_step
        };
        copy.edit(edit, step);
        self.new_step = false;
    }

    /// Moves the word being completed, the places its items are at, and
    /// the snippet's places, through `edit`, made to the text, which
    /// `grows` them as it says.
    fn edited(&mut self, edit: &Edit, grows: snippet::Grows) {
        self.corrections_follow(edit);
        if let Some(session) = &mut self.snippet {
            session.edited(edit, grows);
        }
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
            // A snippet's choices are picked with Ctrl+Y or Enter, as Tab
            // goes to its next place.
            Stroke::Tab => {
                self.as_you_type.tab
                    && !(self.completion.as_ref()).is_some_and(|completion| completion.choices)
            }
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
        Some(Done::REDRAW)
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
        if completion.choices {
            // Typing doesn't narrow a snippet's choices down: any key
            // their menu doesn't take closes it.
            return;
        }
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
        let Some(completion) = self
            .completion
            .as_mut()
            .filter(|completion| !completion.choices)
        else {
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
    /// and puts the cursor where its snippet says, to fill it in from
    /// there. `.` types the same text again.
    fn accept(&mut self, copy: &mut BufferCopy) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        let at = self.insert_byte(copy);
        let expand = |snippet: &clackatoo_text::Snippet| self.expand_snippet(snippet, at, copy);
        let accepted = match completion.accept(copy.text(), at, expand) {
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
        // A snippet with places to type finishes the one being filled in;
        // a choice for one of its places is typing in it.
        let starts = (accepted.stops.iter()).any(|stop| stop.number > 0);
        if starts {
            self.end_snippet(copy, false);
        } else if let Some(session) = &mut self.snippet {
            session.selected = false;
        }
        let typed_from = self.changing.as_ref().map(|change| change.typed.len());
        // A snippet's lines after the first have the line's indent, which
        // `.` gives them afresh.
        let indented = !accepted.stops.is_empty();
        let (text, offset) = if indented {
            let line = self.text(copy).line_text(self.cursor.at.line);
            snippet::unindented(&accepted.text, indent_of(&line), accepted.offset)
        } else {
            (accepted.text.clone(), accepted.offset)
        };
        if let Some(change) = &mut self.changing {
            change.typed.push(Typed::Accepted {
                text,
                offset,
                replaced: accepted.replaced,
                indented,
            });
        }
        self.make(accepted.edit, copy);
        self.cursor = Cursor::new(self.text(copy).point_of(accepted.cursor));
        // For a macro being recorded, the keys that type the same.
        let mut keys = vec![Stroke::Backspace; accepted.replaced];
        keys.extend(snippet::typing_keys(&accepted.text));
        let end = accepted.at + accepted.text.len();
        keys.extend(snippet::keys_back(&self.text(copy), end, accepted.cursor));
        self.menu_keys = Some(keys);
        // Where the first place to type starts, or else the cursor.
        let first = (accepted.stops.iter())
            .filter(|stop| stop.number > 0 && !stop.mirror)
            .min_by_key(|stop| stop.number)
            .map_or(accepted.offset, |stop| stop.start);
        if starts {
            let put = (accepted.at, accepted.text.len());
            self.start_snippet(&accepted.stops, put, accepted.replaced, typed_from, copy);
        }
        // A call it put in, such as `f(`, is asked about.
        let before = accepted.text[..first].chars().next_back();
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
        // A command waiting too long for the grammar is given up on, with
        // no key to wait for.
        let mut asked = self.tick_grammar(now);
        if self.mode != Mode::Insert {
            self.completion_due = None;
            self.signature_due = None;
            return asked;
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

    /// When something's next due to be asked of the language server, or
    /// a command waiting for the grammar given up on, if anything is.
    pub(crate) fn wakes(&self) -> Option<Instant> {
        let completion = self.completion_due.map(|(due, _)| due);
        (completion.into_iter())
            .chain(self.signature_due)
            .chain(self.resolve_due)
            .chain(self.grammar_deadline())
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
    fn prompt_stroke(&mut self, stroke: Stroke, copy: &mut BufferCopy, shared: &Shared) -> Done {
        let Some(prompt) = &mut self.prompt else {
            return Done::default();
        };
        let typed = match prompt {
            Prompt::Search { query, .. } => query,
            Prompt::Rename { name, .. } => name,
            Prompt::Confirm(_) => {
                self.confirm_stroke(stroke, copy);
                return Done::REDRAW;
            }
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
                    // Empty, it's the last search again, this way.
                    if !query.is_empty() {
                        self.search = Some(Searched {
                            pattern: query,
                            backward,
                            star: false,
                        });
                    } else if let Some(search) = &mut self.search {
                        search.backward = backward;
                    }
                    let from = self.cursor.at;
                    self.search_again(false, copy, shared);
                    if self.cursor.at != from {
                        self.left_from(from, copy);
                    }
                }
                Some(Prompt::Rename { name, was }) => self.rename(name, &was, copy),
                Some(Prompt::Confirm(_)) | None => {}
            },
            Stroke::Char { ch, ctrl: false } => typed.push(ch),
            _ => {}
        }
        Done::REDRAW
    }

    /// What `action` asks of the language server, or shows of what it
    /// said.
    fn language_server(&mut self, action: Action, copy: &mut BufferCopy) {
        match action {
            Action::Ask(query) => self.ask(query, copy),
            Action::Hover => self.hover(copy),
            Action::ShowDiagnostics => self.show_diagnostics(copy),
            Action::Rename => self.ask_for_name(copy),
            Action::CodeActions => self.ask_actions(copy),
            _ => {}
        }
    }

    /// `gra`: asks the language server what it could do to the text under
    /// the cursor, or to what's selected, leaving visual mode. The menu
    /// shows once it answers.
    fn ask_actions(&mut self, copy: &mut BufferCopy) {
        let text = self.text(copy);
        let cursor = text.byte_at(self.cursor.at);
        let (start, end) = match (self.mode, self.anchor) {
            (Mode::Visual(selection), Some(anchor)) => {
                let (first, last) = if anchor <= self.cursor.at {
                    (anchor, self.cursor.at)
                } else {
                    (self.cursor.at, anchor)
                };
                match selection {
                    // To the last line's end, not into the line after.
                    Selection::Lines => (text.line_start(first.line), text.line_end(last.line)),
                    Selection::Characters => (text.byte_at(first), text.char_end(last)),
                }
            }
            _ => (cursor, cursor),
        };
        drop(text);
        self.leave_visual();
        let id = copy.ask(Query::CodeActions { end }, start);
        self.asked = Some((id, Asking::Actions(self.cursor.at)));
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
    fn search_again(&mut self, reverse: bool, copy: &BufferCopy, shared: &Shared) {
        let Some(Searched {
            pattern,
            backward,
            star,
        }) = self.search.clone()
        else {
            self.status = Some("no search yet".into());
            self.fail();
            return;
        };
        // As `*` searches, case is ignored or not as `ignorecase` says,
        // never as `smartcase`, as in Vim.
        let matching = if star {
            Matching::Always(self.case)
        } else {
            Matching::Typed(self.case)
        };
        let compiled = match self.pattern(&pattern, matching, shared) {
            Ok(compiled) => compiled,
            Err(why) => {
                self.status = Some(why);
                self.fail();
                return;
            }
        };
        let (whole, len) = self.haystack(copy);
        let text = self.text(copy);
        let from = text.byte_at(self.cursor.at);
        let backward = backward != reverse;
        if let Some(found) = find(&whole[..len], &compiled, from, backward) {
            // The last character's first byte.
            let last = (whole[..found.end].char_indices().next_back())
                .map_or(found.start, |(at, _)| at)
                .max(found.start);
            self.found = Some((text.point_of(found.start), text.point_of(last)));
            self.cursor = Cursor::new(text.point_of(found.start));
        } else {
            self.found = None;
            self.status = Some(format!("{pattern} isn't in the file"));
            self.fail();
        }
    }

    /// `*`, or `#` if `backward`: searches for the word under the cursor,
    /// as a whole word, and goes to the next place it is, or the one
    /// before, going round the file's ends.
    fn search_word(&mut self, backward: bool, copy: &BufferCopy, shared: &Shared) {
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
            self.fail();
            return;
        };
        let word = text.slice(start, end);
        // From the word's start, so going forward finds the next, not this.
        self.cursor = Cursor::new(text.point_of(start));
        self.search = Some(Searched {
            pattern: format!("\\<{}\\>", pattern::escape(&word)),
            backward,
            star: true,
        });
        self.search_again(false, copy, shared);
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
        Done::REDRAW
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

    /// Ctrl+D and the like: `rows` screen rows down, or up if `direction`
    /// is negative, moving the view and the cursor together.
    fn page(&mut self, direction: i64, rows: usize, copy: &BufferCopy) {
        let rows = rows.max(1) as u64;
        let text = self.text(copy);
        self.clamp_top(&text);
        let top = self.top.row(&text);
        let up = direction < 0;
        let top = if up {
            self.cursor.apply(Motion::RowUp(rows), &text);
            rows_up(top, rows, 0, &text)
        } else {
            self.cursor.apply(Motion::RowDown(rows), &text);
            rows_down(top, rows, text.last_line(), &text).min(self.lowest_top(&text))
        };
        wrap::onto_row(&mut self.cursor, &text, up);
        self.top = Top::of(top, &text);
        self.cursor_into_view(&text);
    }

    /// `zt`, `zz` and `zb`: the cursor's line at the view's top, middle or
    /// bottom. At the top or bottom it's shown whole, as in Vim, unless
    /// it's too long to be with the cursor in view.
    fn view_at(&mut self, at: ViewAt, copy: &BufferCopy) {
        let text = self.text(copy);
        let height = self.height.max(1);
        let cursor = row_at(self.cursor.at, &text);
        let top = match at {
            ViewAt::Top if cursor.row < height => RowAt {
                line: cursor.line,
                row: 0,
            },
            ViewAt::Top => cursor,
            ViewAt::Middle => rows_up(cursor, (height / 2) as u64, 0, &text),
            ViewAt::Bottom => {
                let last = last_row(&text, cursor.line);
                let bottom = if last.row - cursor.row < height {
                    last
                } else {
                    cursor
                };
                rows_up(bottom, height as u64 - 1, 0, &text)
            }
        };
        self.top = Top::of(top, &text);
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
        self.edited(edit, snippet::Grows::Nothing);
        self.awaiting_follows(edit);
        if let Some(Prompt::Confirm(confirming)) = &mut self.prompt {
            confirming.edited(edit);
        }
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
                self.text_replaced();
                self.corrections.clear();
                self.held = None;
                // What `:s///c` was asking about is gone.
                if matches!(self.prompt, Some(Prompt::Confirm(_))) {
                    self.prompt = None;
                    self.found = None;
                }
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
                    self.corrections_follow(edit);
                    if let Some(session) = &mut self.snippet {
                        session.edited(edit, snippet::Grows::Nothing);
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
                if self.heard_pieces(*id, answer) || self.heard_indent(*id, answer) {
                    // Done by `resume` or `correct_indents`, which the desk
                    // calls.
                } else if let Some((asked, asking)) = self.asked
                    && asked == *id
                {
                    self.asked = None;
                    self.answered((asked, asking), answer, copy);
                } else if self.renaming == Some(*id) {
                    self.renaming = None;
                    self.renamed(answer);
                } else if self.acting == Some(*id) {
                    self.acting = None;
                    self.acted(answer);
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
            self.actions = None;
            self.close_completion();
        }
        if self.mode == Mode::Insert {
            // Others' edits may have taken the word away.
            self.check_completion(copy);
        }
        // Or the cursor out of the snippet.
        self.snippet_heard(matches!(heard, Heard::Replaced), copy);
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

    /// Acts on the language server's answer to what it was last asked, in
    /// the query numbered `asked`.
    fn answered(&mut self, (asked, asking): (u64, Asking), answer: &Answer, copy: &BufferCopy) {
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
            // Shown only in normal mode, whose keys the menu takes: not once
            // insert mode, or typing in the bar, has begun since.
            Asking::Actions(at)
                if at == self.cursor.at && self.mode == Mode::Normal && self.prompt.is_none() =>
            {
                self.show_actions(asked, answer);
            }
            // The cursor has moved since, or the keys are another mode's.
            Asking::Hover(_) | Asking::Actions(_) => {}
        }
    }

    /// Shows the code actions answered to the query numbered `asked` in a
    /// menu by the cursor, or says there are none, or why.
    fn show_actions(&mut self, asked: u64, answer: &Answer) {
        match answer {
            Answer::CodeActions(actions) => {
                self.panel = None;
                self.actions = ActionMenu::new(asked, actions.clone());
                if self.actions.is_none() {
                    self.status = Some("no code actions here".into());
                }
            }
            Answer::Failed(why) => self.status = Some(why.clone()),
            _ => self.status = Some("no code actions here".into()),
        }
    }

    /// Says in the bar how the code action picked last went.
    fn acted(&mut self, answer: &Answer) {
        self.status = Some(match answer {
            Answer::Acted { title, files: 0 } => format!("{title}: done"),
            Answer::Acted { title, files: 1 } => format!("{title}: changed 1 file"),
            Answer::Acted { title, files } => format!("{title}: changed {files} files"),
            Answer::Failed(why) => why.clone(),
            _ => "the code action did nothing".into(),
        });
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
            | Answer::Pieces(_)
            | Answer::Indents(_)
            | Answer::Completions { .. }
            | Answer::Resolved { .. }
            | Answer::Signature { .. }
            | Answer::CodeActions(_)
            | Answer::Acted { .. } => {}
        }
    }

    /// Puts the cursor on `line`, `col` bytes into it, as far as the line
    /// goes. A message about where it was, such as a search's that found
    /// nothing, goes with it.
    pub(crate) fn go_to(&mut self, line: u64, col: usize, copy: &BufferCopy) {
        self.status = None;
        self.panel = None;
        self.actions = None;
        self.close_completion();
        self.cursor = Cursor::new(self.point_at(line, col, copy));
        if self.mode == Mode::Insert {
            // It may stay past the line's end, where typing goes, and it
            // moved, as the arrow keys move it, perhaps out of a snippet.
            self.moved_while_inserting(copy);
            self.snippet_heard(false, copy);
        } else {
            self.normal_cursor(copy);
        }
        self.follow(copy);
    }

    /// The point on `line`, `col` bytes into it, as far as the text goes,
    /// on a character's start.
    fn point_at(&self, line: u64, col: usize, copy: &BufferCopy) -> Point {
        let text = self.text(copy);
        let line = line.min(text.last_line());
        let (start, end) = (text.line_start(line), text.line_end(line));
        // Back to a character's start, from inside one.
        let rope = text.rope();
        let at = rope.char_to_byte(rope.byte_to_char((start + col).min(end)));
        text.point_of(at)
    }

    // --- Marks --------------------------------------------------------------

    /// `m`: sets `mark` at the cursor; or for `'` and `` ` ``, notes the
    /// cursor's place in the jump list, which is where `''` goes back to.
    fn set_mark(&mut self, mark: char, copy: &BufferCopy, marks: &mut Marks) {
        if matches!(mark, '\'' | '`') {
            self.left_from(self.cursor.at, copy);
            return;
        }
        let byte = self.cursor_byte(copy);
        marks.set(mark, FilePlace::at(copy.path(), copy.text(), byte));
    }

    /// Sets `<` and `>` at the start and end of the selection.
    fn mark_selection(&self, copy: &BufferCopy, marks: &mut Marks) {
        let Some(anchor) = self.anchor else {
            return;
        };
        let (start, end) = if anchor <= self.cursor.at {
            (anchor, self.cursor.at)
        } else {
            (self.cursor.at, anchor)
        };
        let text = self.text(copy);
        for (mark, at) in [('<', start), ('>', end)] {
            marks.set(
                mark,
                FilePlace::at(copy.path(), copy.text(), text.byte_at(at)),
            );
        }
    }

    /// The command line is opened, with `:`: in visual mode, the
    /// selection's lines are marked and left, and the command line starts
    /// with them, `'<,'>`, as in Vim.
    pub(crate) fn command_line(&mut self, copy: &BufferCopy, marks: &mut Marks) -> &'static str {
        if !matches!(self.mode, Mode::Visual(_)) {
            return "";
        }
        self.mark_selection(copy, marks);
        self.leave_visual();
        "'<,'>"
    }

    /// `~`, with `tildeop` off: turns over the case of `times` characters
    /// from the cursor's, on its line, and moves the cursor on past them.
    fn tilde(&mut self, times: usize, copy: &mut BufferCopy) {
        let text = self.text(copy);
        let line = self.cursor.at.line;
        let Some(laid) = text.laid(line) else {
            return;
        };
        let start = laid.byte_at(self.cursor.at.col);
        let mut end = start;
        for _ in 0..times {
            if end >= laid.text().len() {
                break;
            }
            end = laid.char_end(end);
        }
        if end == start {
            self.fail();
            return;
        }
        let line_start = text.line_start(line);
        // How long the text is once changed, which can be more bytes or
        // fewer, as `ß` to `ẞ` is.
        let changed = Recase::Toggle.text(&laid.text()[start..end]).len();
        drop(text);
        self.recase(line_start + start, line_start + end, Recase::Toggle, copy);
        let text = self.text(copy);
        if let Some(laid) = text.laid(line) {
            let after = start + changed;
            let byte = if after < laid.text().len() {
                after
            } else {
                laid.text()[..after.min(laid.text().len())]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(at, _)| at)
            };
            self.cursor = Cursor::new(Point {
                line,
                col: laid.col_of(byte),
            });
        }
    }

    /// `` ` ``, or `'` if not `exact`: to `mark`, or to its line's first
    /// character that isn't blank. A mark in another file is for the desk
    /// to open, which a macro being typed can't wait for, so it stops.
    fn go_to_mark(&mut self, mark: char, exact: bool, copy: &BufferCopy, marks: &Marks) {
        if let Some(place) = marks.get(previous(mark), copy.path())
            && place.path != copy.path()
            && self.mode == Mode::Normal
        {
            self.left_from(self.cursor.at, copy);
            self.opening = Some(FileLocation {
                path: place.path.clone(),
                line: place.line,
                col: place.col,
                offset: None,
                text: String::new(),
            });
            self.fail();
            return;
        }
        let Some(to) = self.mark_here(mark, copy, marks) else {
            self.fail();
            return;
        };
        self.cursor = Cursor::new(to);
        if !exact {
            self.cursor = Cursor::new(Point {
                line: to.line,
                col: 0,
            });
            self.cursor.apply(Motion::FirstNonBlank, &self.text(copy));
        }
    }

    /// Where `mark` is in this file, if it's set, and here; if not, says
    /// why.
    fn mark_here(&mut self, mark: char, copy: &BufferCopy, marks: &Marks) -> Option<Point> {
        let place = marks.get(previous(mark), copy.path());
        match place {
            Some(place) if place.path == copy.path() => {
                Some(self.point_at(place.line, place.col, copy))
            }
            Some(place) => {
                let file = place.path.rsplit('/').next().unwrap_or(&place.path);
                self.status = Some(format!("mark {mark} is in {file}"));
                None
            }
            None => {
                self.status = Some(format!("mark {mark} isn't set"));
                None
            }
        }
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

    /// The screen rows kept in view above and below the cursor:
    /// `scrolloff`, but no more than will fit, so a big one keeps the
    /// cursor in the middle.
    fn margin(&self) -> u64 {
        let height = self.height.max(1) as u64;
        (self.scrolloff as u64).min((height - 1) / 2)
    }

    /// The last line the view can show: the line after the last line
    /// break while the cursor's on it, typing, or else the last.
    fn view_end(&self, text: &Text<'_>) -> u64 {
        if self.cursor.at.line >= text.lines() {
            text.lines()
        } else {
            text.last_line()
        }
    }

    /// Keeps the view's top on a line there is, after edits have changed
    /// the lines.
    fn clamp_top(&mut self, text: &Text<'_>) {
        let end = self.view_end(text);
        if self.top.line > end {
            self.top = Top { line: end, cell: 0 };
        }
    }

    /// Scrolls the view to keep the cursor in it, with `scrolloff` screen
    /// rows above and below it, short of the file's ends.
    fn follow(&mut self, copy: &BufferCopy) {
        self.resized = false;
        // The gutter first: the rows' width depends on it.
        self.gutter = self.gutter_for(self.text(copy).lines());
        let text = self.text(copy);
        self.clamp_top(&text);
        let top = self.top.row(&text);
        let height = self.height.max(1) as u64;
        let margin = self.margin();
        let end = self.view_end(&text);
        let cursor = row_at(self.cursor.at, &text);
        if cursor < rows_down(top, margin, end, &text) {
            self.top = Top::of(rows_up(cursor, margin, 0, &text), &text);
        } else {
            // Not so far as to show past the file's end, unless the cursor
            // is on the line after it, being typed on.
            let below = rows_down(cursor, margin, end, &text);
            if below > rows_down(top, height - 1, end, &text) {
                self.top = Top::of(rows_up(below, height - 1, 0, &text), &text);
            }
        }
        if text.wraps() {
            self.left = 0;
            return;
        }
        let col = self.cursor.at.col;
        let width = self.text_width().max(1);
        if col < self.left {
            self.left = col;
        } else if col >= self.left + width {
            self.left = col + 1 - width;
        }
    }

    /// Scrolls the view by `rows` screen rows, up if negative, as the mouse
    /// wheel does, keeping the cursor in it.
    pub(crate) fn wheel(&mut self, rows: i64, copy: &BufferCopy) {
        let text = self.text(copy);
        self.clamp_top(&text);
        let top = self.top.row(&text);
        let count = rows.unsigned_abs();
        let top = if rows > 0 {
            rows_up(top, count, 0, &text)
        } else {
            let down = rows_down(top, count, text.last_line(), &text);
            down.min(self.lowest_top(&text))
        };
        self.top = Top::of(top, &text);
        let at = self.cursor.at;
        self.cursor_into_view(&text);
        // Moving the cursor away from the word closes the menu, as it
        // does the menu of code actions about it.
        if self.cursor.at != at {
            self.close_completion();
            self.actions = None;
        }
    }

    /// The furthest down the view scrolls: as far as leaves `scrolloff`
    /// rows above the last line's last, which the cursor can then sit on
    /// without the view jumping back to show them.
    fn lowest_top(&self, text: &Text<'_>) -> RowAt {
        rows_up(last_row(text, text.last_line()), self.margin(), 0, text)
    }

    /// Moves the cursor into the view where it is, `scrolloff` rows from
    /// its top and bottom unless that's the file's start or end, as
    /// scrolling the view does. So the next key doesn't scroll it back.
    /// The column it wants to be in stays as it was.
    fn cursor_into_view(&mut self, text: &Text<'_>) {
        let last = text.last_line();
        let margin = self.margin();
        let top = self.top.row(text);
        let bottom = rows_down(top, self.height.max(1) as u64 - 1, last, text);
        let end = last_row(text, last);
        let highest = if top.line == 0 && top.row == 0 {
            top
        } else {
            rows_down(top, margin, last, text)
        };
        let lowest = if bottom >= end {
            end
        } else {
            rows_up(bottom, margin, 0, text)
        };
        // Past the end, with only a few lines in view, the last.
        let at = row_at(self.cursor.at, text);
        let to = at.max(highest).min(lowest);
        if to != at {
            let want = self.cursor.want();
            self.cursor.go_to_row(to, text);
            wrap::onto_row(&mut self.cursor, text, to < at);
            self.cursor = Cursor::wanting(self.cursor.at, want);
        }
    }

    /// Puts the cursor at `line` and `col` of the pane, as a click does. A
    /// click on a line's number goes to the line's start, and beside a row
    /// the line goes on over, to the row's.
    pub(crate) fn click(&mut self, line: usize, col: usize, copy: &BufferCopy) {
        self.panel = None;
        self.actions = None;
        self.close_completion();
        self.gutter = self.gutter_for(self.text(copy).lines());
        let text = self.text(copy);
        self.clamp_top(&text);
        let shown = wrap::view(
            &text,
            self.top.row(&text),
            self.height.max(1),
            self.view_end(&text),
            self.left,
        );
        let Some(&row) = shown.get(line).or(shown.last()) else {
            return;
        };
        let col = match col.checked_sub(self.gutter) {
            // Past the row's end, its last character.
            Some(col) => {
                let cell = row.start + col.saturating_sub(row.indent);
                cell.min(row.end().saturating_sub(1)).max(row.start)
            }
            None if row.row == 0 => 0,
            None => row.start,
        };
        let line = row.line.min(text.last_line());
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

    /// Lays out the rows shown, for [`scene`](Self::scene).
    pub(crate) fn settle(&mut self, copy: &BufferCopy) {
        // The rows may have wrapped afresh, putting the cursor out of view.
        if self.resized {
            self.follow(copy);
        }
        self.gutter = self.gutter_for(self.text(copy).lines());
        // A preview's text, if one's shown, in place of the buffer's.
        let previewed = (self.preview.as_ref()).and_then(|preview| preview.text.clone());
        let text = match &previewed {
            Some(rope) => Text::new(rope, self.indent.tabstop)
                .wrapping(self.wrapping.at(self.text_width().max(1)), None),
            None => self.text(copy),
        };
        self.clamp_top(&text);
        self.snippet_lit = self.snippet_lit(&text);
        let width = self.text_width();
        self.shown = wrap::view(
            &text,
            self.top.row(&text),
            self.height,
            self.view_end(&text),
            self.left,
        );
        let preview = self.preview.as_ref().filter(|_| previewed.is_some());
        // What's wrong in view, moved onto the preview's text.
        let moved = match (preview, self.shown.first(), self.shown.last()) {
            (Some(preview), Some(first), Some(last)) => {
                let (start, end) = (text.line_start(first.line), text.line_end(last.line));
                Some(preview.diagnostics(copy.diagnostics(), start, end))
            }
            _ => None,
        };
        let all_diagnostics = moved.as_ref().unwrap_or(copy.diagnostics());
        let mut rows = Vec::with_capacity(self.height);
        // What's wrong with the line being drawn, and the worst said of it
        // where it starts, which its number and the text after it tell of:
        // kept for its rows after the first.
        let mut wrong: (Option<u64>, Vec<&Diagnostic>, Option<&Diagnostic>) =
            (None, Vec::new(), None);
        for shown in &self.shown {
            let line = shown.line;
            let mut builder = RowBuilder::new(self.width);
            let (start, end) = (text.line_start(line), text.line_end(line));
            if wrong.0 != Some(line) {
                let diagnostics: Vec<&Diagnostic> = all_diagnostics.within(start, end).collect();
                let worst = (diagnostics.iter().copied())
                    .filter(|diagnostic| diagnostic.start >= start)
                    .min_by_key(|diagnostic| diagnostic.severity);
                wrong = (Some(line), diagnostics, worst);
            }
            let (_, diagnostics, worst) = &wrong;
            if shown.row > 0 {
                // A row the line goes on over: no sign or number, as in Vim.
                builder.push(&" ".repeat(self.gutter), &Style::DEFAULT, true);
                self.wrapping.draw_indent(&mut builder, shown.indent);
            } else {
                self.draw_gutter(&mut builder, line, *worst, &text, copy);
            }
            // The line after the last line break, while typed on, is empty.
            let Some(laid) = text.laid(line) else {
                rows.push(builder.finish(false));
                continue;
            };
            let room = width.saturating_sub(shown.indent);
            let cells = shown
                .next
                .map_or(room, |next| (next - shown.start).min(room));
            let moved_spans;
            let spans = match preview {
                Some(preview) => {
                    moved_spans = preview.spans(copy.highlights(), start, end);
                    &moved_spans[..]
                }
                None => copy.highlights().within(start, end),
            };
            laid.show(&mut builder, shown.start, cells, spans, diagnostics, start);
            if let Some(diagnostic) = worst.filter(|_| self.virtual_text && shown.next.is_none()) {
                // Two cells after the line's end, as much as fits:
                // nothing, past the pane's edge.
                let room = self.width.saturating_sub(builder.filled());
                let text_end = laid.cells().saturating_sub(shown.start);
                let gap =
                    (self.gutter + shown.indent + text_end + 2).saturating_sub(builder.filled());
                if gap < room {
                    builder.push(&" ".repeat(gap), &Style::DEFAULT, true);
                    let style = colors::virtual_text(diagnostic.severity);
                    let message = diagnostic.message.lines().next().unwrap_or_default();
                    builder.push("■ ", &style, true);
                    builder.push(message, &style, true);
                }
            }
            rows.push(builder.finish(false));
        }
        while rows.len() < self.height {
            let mut builder = RowBuilder::new(self.width);
            if copy.loaded() {
                builder.push("~", &Style::DEFAULT, true);
            }
            rows.push(builder.finish(false));
        }
        self.rows = rows;
        if let Some(preview) = &mut self.preview {
            preview.light(&text);
        }
        self.place_panels(&text);
    }

    /// Places the cursor, and the panels and menus by it, on the rows just
    /// laid out.
    fn place_panels(&mut self, text: &Text<'_>) {
        // By the cursor, if it's in view.
        let cursor = self.screen_cell(self.cursor.at);
        self.cursor_cell = cursor;
        let pane = (self.height, self.width);
        if let Some(panel) = &mut self.panel {
            panel.settle(pane, cursor);
        }
        if let Some(menu) = &mut self.actions {
            menu.settle(pane, cursor);
        }
        // The menu goes under the cursor, and the signature over it, or
        // the other way round if the menu has no room under it. It lines
        // up with the word, if the word starts on the cursor's row.
        let mut menu_under = None;
        if let Some((row, _)) = cursor
            && let Some(start) = self.completion.as_ref().map(|completion| completion.start)
        {
            let word = text.point_of(start.min(text.rope().len_bytes()));
            let word = match self.screen_cell(word) {
                Some((word_row, word)) if word_row == row => word,
                _ => self.gutter + self.shown[row].indent,
            };
            if let Some(completion) = &mut self.completion {
                completion.settle(pane, cursor, word);
                menu_under = completion.placed().map(|placed| placed.top > row);
            }
        } else if let Some(completion) = &mut self.completion {
            completion.settle(pane, cursor, self.gutter);
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

    /// Draws line `line`'s sign and number, on its first row, coloured as
    /// the `worst` said of it says.
    fn draw_gutter(
        &self,
        builder: &mut RowBuilder,
        line: u64,
        worst: Option<&Diagnostic>,
        text: &Text<'_>,
        copy: &BufferCopy,
    ) {
        let cursor = self.cursor.at.line;
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
    }

    /// Where the character at `at` was drawn, as its row and column in the
    /// pane, if it's in view, as laid out last.
    fn screen_cell(&self, at: Point) -> Option<(usize, usize)> {
        let width = self.text_width();
        (self.shown.iter().enumerate())
            .filter(|(_, shown)| shown.line == at.line)
            .find_map(|(row, shown)| Some((row, self.gutter + shown.col_of(at.col, width)?)))
    }

    /// Closes the panel by the cursor, and the completion menu and
    /// signature, as the focus leaving does.
    pub(crate) fn close_panel(&mut self) {
        self.panel = None;
        self.actions = None;
        self.close_completion();
        self.close_signature();
    }

    /// Shows `rows`, a list such as `:registers` makes, under `heading`, in
    /// the panel by the cursor, until the cursor moves.
    pub(crate) fn show_listing(&mut self, heading: &str, rows: Vec<String>) {
        self.close_completion();
        self.panel = Panel::listing(heading, rows, self.cursor.at);
    }

    /// The panel by the cursor, if it's shown, as it's to be drawn, in
    /// cells of the pane.
    pub(crate) fn panel(&self) -> Option<PanelView<'_>> {
        self.panel.as_ref()?.view()
    }

    /// Everything shown over the text by the cursor, as it's to be drawn,
    /// in cells of the pane, in the order it's drawn: the panel, the menu
    /// of code actions, the signature, the completion menu and the
    /// documentation beside it.
    pub(crate) fn panels(&self) -> Vec<PanelView<'_>> {
        let completion = self.completion.as_ref();
        [
            self.panel(),
            self.actions.as_ref().and_then(ActionMenu::view),
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
        let cursor = self.cursor_cell.map(|(line, col)| SceneCursor {
            line,
            col,
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
        let width = self.text_width();
        // Every row of the cursor's line, as Vim's `cursorlineopt` has it.
        if self.cursorline {
            for (line, _) in (self.shown.iter().enumerate())
                .filter(|(_, shown)| shown.line == self.cursor.at.line)
            {
                highlights.push(Highlight {
                    line,
                    cols: self.gutter..self.width,
                    kind: HighlightKind::CursorLine,
                });
            }
        }
        // Down the file's rows, not those past its end, as in Vim. Where
        // lines wrap, it marks the cell that column of the line is in.
        for &column in &self.columns {
            for (line, shown) in self.shown.iter().enumerate() {
                if let Some(col) = shown.col_of(column, width) {
                    let col = self.gutter + col;
                    highlights.push(Highlight {
                        line,
                        cols: col..col + 1,
                        kind: HighlightKind::ColorColumn,
                    });
                }
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
        // A snippet's place gone to, whose text typing replaces.
        if let Some((start, last)) = self.snippet_lit.filter(|_| self.mode == Mode::Insert) {
            let kind = Selection::Characters;
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
        for &(start, last) in self.preview.iter().flat_map(|preview| &preview.lit) {
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

    /// Highlights from `start` to `last`, as `why` says, on the rows shown.
    /// Where it goes on past a row's end, it's lit to the pane's edge.
    fn highlight(
        &self,
        start: Point,
        last: Point,
        kind: Selection,
        why: HighlightKind,
        into: &mut Vec<Highlight>,
    ) {
        let width = self.text_width();
        for (row, shown) in self.shown.iter().enumerate() {
            let line = shown.line;
            if line < start.line || line > last.line {
                continue;
            }
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
            let from = from.max(shown.start);
            if from >= to.min(shown.end()) {
                continue;
            }
            let first = shown.indent + (from - shown.start);
            let end = if to > shown.end() {
                width
            } else {
                (shown.indent.saturating_add(to - shown.start)).min(width)
            };
            if first < end {
                into.push(Highlight {
                    line: row,
                    cols: self.gutter + first..self.gutter + end,
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
        Key::Named(NamedKey::Tab) if modifiers.shift => Some(Stroke::BackTab),
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

/// The mark `mark` is kept as: `` ` `` is `'`, the place before the last
/// jump, by either name.
fn previous(mark: char) -> char {
    if mark == '`' { '\'' } else { mark }
}

/// Whether `motion` reads the Vim engine's row of a line, character by
/// character, so can't see past the end of a long line's (see
/// [`Lines::overflows`]): word motions, `f` and its kin, and `%`.
fn reads_row(motion: Motion) -> bool {
    matches!(
        motion,
        Motion::WordStart
            | Motion::WordBack
            | Motion::WordEnd
            | Motion::BigWordStart
            | Motion::BigWordBack
            | Motion::BigWordEnd
            | Motion::Find { .. }
            | Motion::Match
    )
}

/// Whether `motion` fails when it doesn't move the cursor, which stops a
/// macro being typed, as in Vim: all but those that go to a place on the
/// line or its screen row, or in the file, whatever's there.
fn can_fail(motion: Motion) -> bool {
    !matches!(
        motion,
        Motion::LineStart
            | Motion::LineEnd
            | Motion::FirstNonBlank
            | Motion::First
            | Motion::Last
            | Motion::RowStart
            | Motion::RowEnd
    )
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

/// A buffer's text as one string, and the buffer and revision it's of.
#[derive(Debug)]
struct Whole {
    of: (BufferId, u64),
    text: std::rc::Rc<str>,
}

/// A pattern compiled, and what it was compiled from.
#[derive(Debug)]
struct Compiled {
    source: String,
    matching: Matching,
    tilde: String,
    pattern: Pattern,
}

/// The last search: what was searched for, which way, and whether `*`
/// searched for it.
#[derive(Clone, Debug)]
struct Searched {
    /// The pattern, as Vim writes one.
    pattern: String,
    backward: bool,
    /// Whether `*` or `#` searched, which match case as `ignorecase`
    /// says, never as `smartcase`.
    star: bool,
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

/// Where `pattern` next matches in `text` after byte `from`, or before it
/// if `backward`, going round the ends.
fn find(text: &str, pattern: &Pattern, from: usize, backward: bool) -> Option<pattern::Found> {
    if !backward {
        let after = from
            + text[from.min(text.len())..]
                .chars()
                .next()
                .map_or(1, char::len_utf8);
        return (after <= text.len())
            .then(|| pattern.find_at(text, after))
            .flatten()
            .or_else(|| pattern.find_at(text, 0));
    }
    let (mut before, mut last) = (None, None);
    let mut at = 0;
    while at <= text.len()
        && let Some(found) = pattern.find_at(text, at)
    {
        if found.start < from {
            before = Some(found);
        }
        last = Some(found);
        at = if found.end > found.start {
            found.end
        } else {
            found.end + text[found.end..].chars().next().map_or(1, char::len_utf8)
        };
    }
    before.or(last)
}

#[cfg(test)]
mod tests;
