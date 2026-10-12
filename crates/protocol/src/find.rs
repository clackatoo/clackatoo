//! Finding things for a client's picker: the files in a project, the lines
//! in them a pattern matches, the files open in buffers, and lines of a
//! file to preview.
//!
//! The daemon owns the files, which may be on another machine, so it walks
//! and reads them, off its event loop. A client asks with
//! [`ClientMessage::Find`](crate::ClientMessage::Find), and what's found
//! comes a batch at a time in [`Event::Found`](crate::Event::Found)s, until
//! one says it's [`Found::Done`]. The client ranks and draws what comes. It
//! stops a search it no longer wants, such as one for what was typed before
//! the last key, with
//! [`ClientMessage::StopFinding`](crate::ClientMessage::StopFinding), and
//! nothing more comes of it.

use serde::{Deserialize, Serialize};

/// What a client asks the daemon to find.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Search {
    /// The project's files: every file under its root that git doesn't
    /// ignore, leaving out hidden ones, as ripgrep does.
    Files,
    /// The lines of those files that `pattern` matches: a regular
    /// expression, or if it isn't a valid one, plain text.
    Grep {
        /// What to look for.
        pattern: String,
        /// Match case exactly; otherwise case is ignored. The client
        /// decides, by its options, as for [`Request::Search`](crate::Request::Search).
        exact: bool,
    },
    /// The files open in buffers, the newest first, including those no pane
    /// shows that have changes not written.
    Buffers,
    /// Up to `lines` lines of the file at `path`, around line `line`: a
    /// third of them before it.
    Preview {
        /// The file, on the daemon's machine.
        path: String,
        /// The line to show, from 0.
        line: u64,
        /// How many lines, at most.
        lines: u16,
    },
}

/// Some of what the daemon found for a search.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Found {
    /// Where it looks: the root of the project the pane asked from is in.
    /// The paths of files found are from it, unless they're absolute. Comes
    /// first, for every search but a preview.
    Root {
        /// The root, made absolute.
        path: String,
        /// The root as people write it, with `~` for the home directory.
        name: String,
    },
    /// Files, for [`Search::Files`].
    Files(Vec<String>),
    /// Lines, for [`Search::Grep`].
    Lines(Vec<FoundLine>),
    /// Files open in buffers, for [`Search::Buffers`].
    Buffers(Vec<FoundBuffer>),
    /// Lines of a file, for [`Search::Preview`], from line `first`. Each is
    /// cut short if it's long.
    Preview {
        /// The first line's number, from 0.
        first: u64,
        /// The lines, without their line ends.
        lines: Vec<String>,
    },
    /// The search has finished: nothing more comes of it.
    Done(Done),
}

/// How a search finished.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Done {
    /// Everything there was has been found.
    All,
    /// It stopped at the most it finds, with more left.
    CutShort,
    /// It couldn't look, as this says.
    Failed(String),
}

/// A line a pattern matches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundLine {
    /// Its file's path, from the root.
    pub path: String,
    /// Its line, from 0.
    pub line: u64,
    /// Where the first match starts, in bytes from the line's start.
    pub col: usize,
    /// The line's text, without its end, cut short if it's long, and from
    /// some way in if the first match is far along it.
    pub text: String,
    /// Where in `text` each match is, as byte ranges, in order.
    pub matches: Vec<(usize, usize)>,
}

/// A file open in a buffer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundBuffer {
    /// Its path, from the root if it's under it, or else absolute.
    pub path: String,
    /// It has changes not written.
    pub modified: bool,
}
