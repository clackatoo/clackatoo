//! Buffers: the text of files being edited, kept by the daemon, with a copy
//! in each window showing them.
//!
//! A window attaches to a buffer, and gets a [`BufferUpdate::Snapshot`] of
//! it, then each edit as it's made. It edits its copy as you type, without
//! waiting, and sends each [`Change`] to the daemon in a
//! [`ClientMessage::Edit`](crate::ClientMessage::Edit), with the version it
//! was made to. Changes made at once elsewhere, by an agent or another
//! window, are reconciled by operational transformation (see
//! [`clackatoo_text`]): the daemon brings each edit up to date as it
//! arrives, and the window brings the daemon's edits up to date past its
//! own that the daemon hasn't yet answered.
//!
//! A window has one change on its way at a time: it sends the next once
//! the daemon has answered the last, with [`BufferUpdate::Acked`] for an
//! edit, or with the edit an undo or redo made.
//!
//! A buffer whose language the daemon has a grammar for is parsed after
//! each edit, off the event loop, and its [`BufferUpdate::Highlights`] sent
//! as each parse ends: all of them first, then what each parse changed.
//! The window moves the highlights it has along with each edit until then.
//!
//! A buffer whose language has a language server is kept open in it, and
//! its [`BufferUpdate::Diagnostics`] sent whenever the server says what's
//! wrong; the window moves them along with each edit too. Once the server
//! is ready, the window is told what it can do as you type
//! ([`BufferUpdate::Features`]), so it asks only what the server answers.
//!
//! A buffer whose file is in a git work tree is compared with the file's
//! text in git's index after each pause in edits, and whenever the index
//! changes, off the event loop, and its [`BufferUpdate::Hunks`] sent: the
//! lines added, changed and deleted, for the signs beside them. The window
//! moves them along with each edit too.
//!
//! The lines agents have changed, and whether the user has reviewed them,
//! are sent as [`BufferUpdate::Agents`] whenever an agent changes the
//! buffer, or a hunk is kept or rejected (see [`crate::ReviewAction`]).
//!
//! A window asks where the functions, classes and arguments around a place
//! are, for text objects such as `af`, or where the next one starts, for
//! motions such as `]f`, as it asks a language server about a place: with
//! a [`Query`](crate::ClientMessage::Query). The daemon answers those
//! from the buffer's syntax tree, off the event loop, and so how lines
//! should be indented.

use std::fmt;

use clackatoo_text::{AgentHunk, Construct, Diagnostic, Edit, Hunk, LineIndent, Piece, Span};
use serde::{Deserialize, Serialize};

use crate::{ChatName, TerminalId};

/// Identifies a buffer within one daemon. Never reused while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BufferId(pub u32);

impl fmt::Display for BufferId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A file to edit in a new pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFile {
    /// Its path, on the daemon's machine. A file already being edited is
    /// shown again, not read again, and a file that doesn't exist yet is
    /// made when it's first saved.
    pub path: String,
    /// The directory a relative path is from. `None` uses the one the pane
    /// beside it is in.
    pub cwd: Option<String>,
}

/// Who made an edit. Each undoes their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Author {
    /// The person at the windows: every window's edits are theirs.
    User,
    /// The agent of the chat with this terminal ID.
    Agent(TerminalId),
}

/// A buffer as it is now: what a window attaching to it gets first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferSnapshot {
    /// The file's path on the daemon's machine, made absolute.
    pub path: String,
    /// Its text.
    pub text: String,
    /// The text's version: how many edits have been made to it.
    pub version: u64,
    /// Whether it has changes that aren't saved.
    pub modified: bool,
    /// The number of the last change the attaching window (see
    /// [`Request::AttachBuffer`](crate::Request::AttachBuffer)) sent that
    /// the daemon has dealt with, made or not, if any: the window's changes
    /// up to it are in the text, or were nothing to make, and any after it
    /// never arrived, as when a connection was lost with one on its way.
    pub applied: Option<u64>,
    /// The version the change numbered `applied` made, if it made one. An
    /// edit dealt with that made none couldn't be made: it isn't in the
    /// text.
    pub landed: Option<u64>,
    /// The daemon's own number, chosen at random when it first started
    /// and kept through upgrades: a window that sees another knows the
    /// daemon was restarted, and its buffers are no longer the ones it had.
    pub daemon: u64,
}

/// What a window that lost its connection knows of a buffer, to be
/// brought up to date from rather than sent the whole buffer again (see
/// [`BufferUpdate::Resumed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    /// The daemon it knew the buffer from: see [`BufferSnapshot::daemon`].
    pub daemon: u64,
    /// The last version of the buffer it heard of.
    pub version: u64,
}

/// A change a window makes to a buffer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    /// An edit, which the window has already made to its copy.
    Edit {
        /// The edit.
        edit: Edit,
        /// The version of the text it was made to.
        base: u64,
        /// Whether it starts a new undo step, rather than joining the last,
        /// as each key typed in one go of insert mode does.
        step: bool,
    },
    /// Undo the user's last step, wherever it was made.
    Undo,
    /// Redo the step the user last undid.
    Redo,
}

/// What the daemon tells a window about a buffer it attached to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BufferUpdate {
    /// The whole buffer: the first update a window gets, and whenever its
    /// copy can't be brought up to date, such as when an edit it sent
    /// couldn't be made. Changes it sent and hadn't heard back about are
    /// gone.
    Snapshot(BufferSnapshot),
    /// What changed since the version a window attaching again knew (see
    /// [`Resume`]): every edit since, in order, the first making the
    /// version after it, so the window can bring its copy, and the changes
    /// it still has waiting, up to date as if it had never been away.
    /// Sent in place of a snapshot when the daemon still has the edits.
    Resumed {
        /// The daemon's number: see [`BufferSnapshot::daemon`].
        daemon: u64,
        /// The file's path, which may have moved meanwhile.
        path: String,
        /// The edits, each made to the text the one before made.
        edits: Vec<Edit>,
        /// As [`BufferSnapshot::applied`].
        applied: Option<u64>,
        /// The version the window's change numbered `applied` made, if it
        /// made one: that edit is among `edits`, and is the window's own.
        /// An edit dealt with that made none couldn't be made.
        landed: Option<u64>,
        /// Whether the buffer has changes that aren't saved.
        modified: bool,
    },
    /// The window's own edit, which it numbered `seq`, has been made, and
    /// made `version`.
    Acked {
        /// The version it made.
        version: u64,
        /// The window's number for the change.
        seq: u64,
    },
    /// An edit was made, making `version`: someone else's, or one this
    /// window's undo or redo made.
    Edited {
        /// The version it made.
        version: u64,
        /// The edit, made to the version before.
        edit: Edit,
        /// Who made it.
        author: Author,
        /// For the window whose undo or redo made it, that change's number.
        answers: Option<u64>,
    },
    /// The window's undo or redo, which it numbered with this, found
    /// nothing to undo or redo.
    NoChange(u64),
    /// Whether the buffer has changes that aren't saved is now this.
    Modified(bool),
    /// What a parse found: bytes `start` to `end` of the text at `version`
    /// are highlighted as `spans` say, and the rest as before. Sent after
    /// the edit that made `version`, and after a snapshot, for the whole
    /// text.
    Highlights {
        /// The version of the text they're for.
        version: u64,
        /// The first byte they're for.
        start: usize,
        /// The byte after the last.
        end: usize,
        /// The spans there, in order.
        spans: Vec<Span>,
    },
    /// What the buffer's language server says about the text at `version`:
    /// every diagnostic, in place of those before. Sent after the edit
    /// that made `version`, and after a snapshot.
    Diagnostics {
        /// The version of the text they're for.
        version: u64,
        /// The diagnostics, in order of where they start.
        diagnostics: Vec<Diagnostic>,
    },
    /// The buffer's language isn't installed, or its language server
    /// isn't, and the daemon can install them: offered once to each client
    /// for each language, unless the answer was once no.
    Offer {
        /// The language, as language packs name it, such as `rust`.
        language: String,
        /// Its grammar isn't installed.
        grammar: bool,
        /// Its language server isn't, by its program's name.
        server: Option<String>,
    },
    /// The answer to the client's
    /// [`ClientMessage::Query`](crate::ClientMessage::Query) numbered `id`.
    Answer {
        /// The query's number.
        id: u64,
        /// The version of the text places in this buffer are counted in.
        version: u64,
        /// What the language server said.
        answer: Answer,
    },
    /// How the text at `version` differs from the file's text in git's
    /// index: every hunk, in place of those before. Sent after the edit
    /// that made `version`, and after a snapshot. There are none for a
    /// file that isn't in git, or is too big to compare.
    Hunks {
        /// The version of the text they're for.
        version: u64,
        /// The hunks, in order.
        hunks: Vec<Hunk>,
    },
    /// What the buffer's language server does as you type, sent once it's
    /// ready, after a snapshot, and with nothing in it once the server
    /// stops.
    Features(ServerFeatures),
    /// The buffer's file has moved to this path, made absolute, as a file
    /// explorer moving it, or a directory it's in, does. Its text and
    /// history are as they were.
    Moved(String),
    /// Something the window should say in its bar, such as that an agent
    /// changed the file where you have changes not saved.
    Notice(String),
    /// Something other than an agent changed the buffer's file on disk,
    /// such as a git checkout or another editor, and the buffer has no
    /// changes not saved. The window reads it again with
    /// [`Request::Reload`](crate::Request::Reload), or says so, as its
    /// `autoread` says. Sent again to a client attaching while it's so.
    ChangedOnDisk,
    /// The lines of the text at `version` that agents changed: every
    /// agent hunk, in place of those before, kept or not, with the names
    /// of the chats that made them. Sent after the edit that made
    /// `version`, after a snapshot, and whenever one's made, kept or
    /// rejected; the window moves them along with each edit meanwhile.
    Agents {
        /// The version of the text they're for.
        version: u64,
        /// The hunks, in order.
        hunks: Vec<AgentHunk>,
        /// The names of the chats that made them.
        chats: Vec<ChatName>,
    },
}

/// What a buffer's language server does as you type, as it said when it
/// started.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerFeatures {
    /// It completes what's typed, if `Some`: and these characters, typed,
    /// ask it to at once, as `.` does.
    pub completion: Option<Vec<char>>,
    /// It helps with a call's arguments, if `Some`: and these characters,
    /// typed, ask it to, as `(` and `,` do.
    pub signature: Option<Vec<char>>,
    /// While it's helping, these ask again too, as `)` does.
    pub signature_again: Vec<char>,
}

/// What a client asks about a place in a buffer: most questions are for
/// its language server, and [`Pieces`](Self::Pieces) and
/// [`Starts`](Self::Starts) for its grammar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Query {
    /// Where what's there is defined, as `gd` goes.
    Definition,
    /// Where it's declared, as `gD` goes: for most languages, the same.
    Declaration,
    /// Where its type is defined.
    TypeDefinition,
    /// What implements it, such as a trait's impls.
    Implementation,
    /// Everywhere it's used, its declaration included, as `grr` lists.
    References,
    /// What it is: its type, and its documentation, as `K` shows.
    Hover,
    /// The whole file formatted, as `:Format` does, with tabs `tab_size`
    /// columns wide, and indenting with spaces if `spaces`. The place is
    /// ignored. The server's edits are made to the buffer, as the user's,
    /// before the answer comes.
    Format {
        /// How many columns a tab is.
        tab_size: u32,
        /// Whether to indent with spaces rather than tabs.
        spaces: bool,
    },
    /// What could be typed there, to complete the word before it, as the
    /// menu in insert mode shows.
    Completion(Trigger),
    /// More about item `item` of the completions answered to the query
    /// numbered `list`: its documentation, which servers send only when
    /// asked. Answered with [`Answer::Resolved`]. The place isn't looked
    /// at.
    Resolve {
        /// The completion query's number.
        list: u64,
        /// The item's index in its answer.
        item: u32,
    },
    /// The call whose arguments are being typed there, and which of them
    /// it's at.
    SignatureHelp {
        /// What asked.
        trigger: Trigger,
        /// Whether its help is shown already.
        retrigger: bool,
    },
    /// What the language server could do to the text from the place to
    /// byte `end`, as `gra` asks: fix what's wrong there, or rewrite it.
    /// The server is told what's wrong there too. Answered with
    /// [`Answer::CodeActions`]. For the cursor alone, `end` is the place.
    CodeActions {
        /// The byte after the last asked about, in the same text as the
        /// place, which the daemon moves along with it.
        end: usize,
    },
    /// Does action `item` of those answered to the
    /// [`CodeActions`](Self::CodeActions) query numbered `list`: its edits
    /// are made, as a rename's are, and its command run. Answered with
    /// [`Answer::Acted`]. The place isn't looked at. The list is used up:
    /// another action of it is out of date, as it may make edits again.
    Act {
        /// The code actions query's number.
        list: u64,
        /// The action's index in its answer.
        item: u32,
    },
    /// The pieces of code of this kind around the place, innermost first,
    /// each with its insides, as `af` and `if` pick from; or if there are
    /// none, the first after it. Answered from the syntax tree with
    /// [`Answer::Pieces`].
    Pieces(Construct),
    /// Where the next `count` pieces of code of this kind start after the
    /// place, or before it if `backward`, nearest first, as `]f` and `[f`
    /// go. Answered from the syntax tree with [`Answer::Pieces`], without
    /// their insides.
    Starts {
        /// The kind.
        construct: Construct,
        /// Whether to look before the place.
        backward: bool,
        /// How many.
        count: u32,
    },
    /// How the lines from the one starting at the place to the one
    /// starting at `end` should be indented, as their language's
    /// `indents.scm` says, for `=` and to put right the indent a new line
    /// was given. Both are moved with edits made since, so lines put in
    /// between are indented too. Answered from the syntax tree with
    /// [`Answer::Indents`], or [`Answer::Failed`] if the language has no
    /// rules, or they'd take too long.
    Indent {
        /// The start of the last line.
        end: usize,
        /// The columns one step of indent is (`shiftwidth`).
        shiftwidth: u32,
        /// The columns tabs stop at multiples of (`tabstop`).
        tabstop: u32,
    },
}

impl Query {
    /// Whether the buffer's grammar answers it, not its language server.
    #[must_use]
    pub fn for_grammar(self) -> bool {
        matches!(
            self,
            Self::Pieces(_) | Self::Starts { .. } | Self::Indent { .. }
        )
    }
}

/// Why a language server is asked what could be typed, or about a call,
/// as LSP says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trigger {
    /// Asked for, as Ctrl+Space does, or as a word is typed.
    Invoked,
    /// This character was typed, one the server says should ask.
    Character(char),
    /// Asked again: for completions, because the last answer said it
    /// wasn't all of them; for a call, because the text or the cursor
    /// moved while its help was shown.
    Again,
}

/// What a language server answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    /// Places, best first.
    Locations(Vec<FileLocation>),
    /// What's there, to read, as markdown if `markdown`, or else plain
    /// text.
    Hover {
        /// What the server said.
        text: String,
        /// Whether it's markdown.
        markdown: bool,
    },
    /// What could be typed, best first as the server sorts them (see
    /// [`CompletionItem::sort`]).
    Completions {
        /// The items, as many as the daemon passes on.
        items: Vec<CompletionItem>,
        /// Whether that's not all of them, so typing more should ask again
        /// rather than only narrow these down: as when the server says so,
        /// or the daemon left some out.
        incomplete: bool,
    },
    /// More about a completion item, as [`Query::Resolve`] asks.
    Resolved {
        /// What it is, such as its type, if the server says.
        detail: Option<String>,
        /// Its documentation, as markdown if the second is true.
        documentation: Option<(String, bool)>,
    },
    /// The call whose arguments are being typed, as [`Query::SignatureHelp`]
    /// asks.
    Signature {
        /// Its signature, as the server writes it, such as
        /// `fn add(a: u32, b: u32) -> u32`.
        label: String,
        /// The bytes of `label` that are the argument being typed, if the
        /// server says which.
        parameter: Option<(usize, usize)>,
        /// Which of the function's signatures this is, from 0...
        index: usize,
        /// ...of how many it has, as an overloaded one has several.
        count: usize,
    },
    /// What could be done to the text asked about, as
    /// [`Query::CodeActions`] asks: quick fixes first, and of those, and
    /// then of the rest, those the server would have picked first.
    CodeActions(Vec<CodeAction>),
    /// A [`Query::Act`] was done: every edit it took, to buffers open and
    /// files not, has been made, the buffers' sent as edits before this,
    /// one undo step in each, and its command has been run.
    Acted {
        /// The action's title.
        title: String,
        /// How many files its edits changed, the language server's own
        /// edits while its command ran included.
        files: usize,
    },
    /// A [`ClientMessage::Rename`](crate::ClientMessage::Rename) was
    /// made: every edit it took, to buffers open and files not, has been
    /// made, the buffers' sent as edits before this.
    Renamed {
        /// How many places were changed.
        occurrences: usize,
        /// In how many files.
        files: usize,
    },
    /// Nothing: there's nothing there to ask about, or the server doesn't
    /// know. For a question for the grammar, the buffer has none. For a
    /// [`ClientMessage::Stage`](crate::ClientMessage::Stage), there's no
    /// hunk there.
    Nothing,
    /// The file is formatted: its edits, if `changed`, have been made.
    Formatted {
        /// Whether formatting changed anything.
        changed: bool,
    },
    /// The buffer has no language server, or it failed, or what it said
    /// couldn't be done, as this says. A rename that fails changes
    /// nothing, unless a file it wrote couldn't then be put back as it
    /// was, which this says. So does a stage git refused.
    Failed(String),
    /// Pieces of code, as [`Query::Pieces`] and [`Query::Starts`] ask, as
    /// bytes of the text at the answer's version, which a client moves
    /// past its own changes that haven't been made yet, as it moves
    /// diagnostics. There are none if the buffer has no grammar.
    Pieces(Vec<Piece>),
    /// How lines should be indented, as [`Query::Indent`] asks, each
    /// line by the byte it starts at in the text at the answer's version,
    /// which a client moves past its own changes as it moves pieces. A
    /// line taken out since isn't there.
    Indents(Vec<LineIndent>),
    /// A [`ClientMessage::Stage`](crate::ClientMessage::Stage) was made:
    /// the hunk's lines are in git's index, and the buffer's hunks, sent
    /// before this, no longer have it.
    Staged,
}

/// Something a language server could do to some text, such as fix what's
/// wrong with it, or rewrite it, as [`Query::CodeActions`] asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeAction {
    /// What it does, as the server says, such as `Add explicit type`.
    pub title: String,
    /// What sort of action it is, as LSP names them: `quickfix`,
    /// `refactor.extract`, `source.organizeImports` and the like. Empty if
    /// the server doesn't say.
    pub kind: String,
    /// Whether the server would have it picked first.
    pub preferred: bool,
    /// Why it can't be done now, if it can't, as the server says.
    pub disabled: Option<String>,
}

/// Something that could be typed, as a language server suggests it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionItem {
    /// What the menu shows.
    pub label: String,
    /// What it is, as LSP numbers kinds (`CompletionItemKind`): 3 is a
    /// function, 6 a variable, 14 a keyword. 0 if the server doesn't say.
    pub kind: u8,
    /// More about it, such as its type, shown dimmed beside it. Empty if
    /// the server doesn't say.
    pub detail: String,
    /// What's matched against the word typed, if not `label`.
    pub filter: Option<String>,
    /// What it's sorted by, among those matching as well, if not `label`.
    pub sort: Option<String>,
    /// Whether the server would have it picked first.
    pub preselect: bool,
    /// Where the text it puts in starts: a byte of the text at the
    /// answer's version, which a client moves past its own changes as it
    /// moves diagnostics. What's from there to the cursor is replaced.
    /// `None` if the server doesn't say, for the word before the cursor.
    pub start: Option<usize>,
    /// What it puts in.
    pub text: String,
    /// Whether `text` is a snippet, with places to type such as `$1` and
    /// `${2:name}`, as LSP writes them.
    pub snippet: bool,
    /// Edits elsewhere that go with it, such as the import it needs, as
    /// bytes of the text at the answer's version, in order, apart.
    pub more: Vec<TextChange>,
}

/// An edit to a buffer's text: bytes `start` to `end` replaced with
/// `text`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextChange {
    /// The first byte replaced.
    pub start: usize,
    /// The byte after the last.
    pub end: usize,
    /// What goes in their place.
    pub text: String,
}

/// A place in a file, on the daemon's machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLocation {
    /// The file's path, made absolute.
    pub path: String,
    /// Its line, from 0.
    pub line: u64,
    /// Its column, in bytes from the line's start.
    pub col: usize,
    /// For a place in the buffer asked about, its byte in the text at the
    /// answer's version, which a client moves past its own changes that
    /// haven't been made yet, as it moves diagnostics.
    pub offset: Option<usize>,
    /// The line's text, without its end, cut short if it's long, for a
    /// list of places to pick from.
    pub text: String,
}
