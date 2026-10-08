//! Shell integration: the blocks a shell divides its terminal into, one per
//! command, with the command's prompt, the command itself and its output.
//!
//! Shells mark them with OSC 133, which fish sends by itself, and bash and
//! zsh through scripts:
//!
//! - `A` where the prompt starts,
//! - `B` where it ends and typing starts (ignored here),
//! - `C` where the command's output starts, optionally with
//!   `cmdline_url=` and the command, percent-encoded,
//! - `D;status` when it has finished, with its exit status.
//!
//! OSC 7 (`file://host/path`) reports the working directory.
//!
//! A mark goes on the row the cursor is on, and moves with it: into
//! history, through rewrapping on resize, and away when the row is
//! erased or evicted. What the shell said about each block (its command,
//! directory and exit status) is kept here, by block number, for as long
//! as a row is marked with it. The alternate screen, where full-screen
//! programs run, takes no marks.

use std::collections::VecDeque;

use crate::terminal::Terminal;

/// A row's mark: it is where the prompt or the output of a block starts,
/// or both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mark {
    pub(crate) block: u32,
    pub(crate) prompt: bool,
    pub(crate) output: bool,
}

impl Mark {
    /// The mark a row gets when `block`'s prompt (or, with `output`, its
    /// output) starts on it, where it had `old`. Marks of other blocks are
    /// replaced: the row has been reused.
    pub(crate) fn add(old: Option<Self>, block: u32, output: bool) -> Self {
        let mut mark = match old {
            Some(old) if old.block == block => old,
            _ => Self {
                block,
                prompt: false,
                output: false,
            },
        };
        if output {
            mark.output = true;
        } else {
            mark.prompt = true;
        }
        mark
    }
}

/// A command and what the shell said about it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Entry {
    /// The command line, if the shell said.
    pub(crate) command: Option<String>,
    /// Where it ran, if the shell said.
    pub(crate) cwd: Option<String>,
    pub(crate) phase: Phase,
    /// Its exit status, once finished, if the shell said.
    pub(crate) exit: Option<i32>,
}

/// How far a block has got.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The shell is showing the prompt.
    #[default]
    Prompt,
    /// The command is running.
    Running,
    /// It has finished.
    Finished,
}

/// Every block a row is still marked with, and maybe a few older ones,
/// numbered in order.
#[derive(Clone, Debug, Default)]
pub(crate) struct Blocks {
    entries: VecDeque<Entry>,
    /// The number of the first entry.
    first: u32,
}

impl Blocks {
    /// The blocks `entries`, numbered from `first`.
    pub(crate) fn from_parts(first: u32, entries: VecDeque<Entry>) -> Self {
        Self { entries, first }
    }

    /// The number of the first block, and every block from it on.
    pub(crate) fn parts(&self) -> (u32, &VecDeque<Entry>) {
        (self.first, &self.entries)
    }

    pub(crate) fn get(&self, block: u32) -> Option<&Entry> {
        let index = block.checked_sub(self.first)?;
        self.entries.get(usize::try_from(index).ok()?)
    }

    /// A prompt has started: the number of its block. A prompt drawn again
    /// before any command ran, as shells do after a resize, is the same
    /// block.
    pub(crate) fn prompt(&mut self) -> u32 {
        match self.entries.back() {
            Some(last) if last.phase == Phase::Prompt => self.last(),
            _ => self.push(Entry::default()),
        }
    }

    /// `command` has started running in `cwd`: the number of its block.
    pub(crate) fn command(&mut self, command: Option<String>, cwd: Option<String>) -> u32 {
        let running = Entry {
            command,
            cwd,
            phase: Phase::Running,
            exit: None,
        };
        match self.entries.back_mut() {
            Some(last) if last.phase == Phase::Prompt => {
                *last = running;
                self.last()
            }
            // The shell didn't mark a prompt.
            _ => self.push(running),
        }
    }

    /// The running command has finished, with `exit` if the shell said.
    pub(crate) fn finished(&mut self, exit: Option<i32>) {
        if let Some(last) = self.entries.back_mut()
            && last.phase == Phase::Running
        {
            last.phase = Phase::Finished;
            last.exit = exit;
        }
    }

    /// Forgets the blocks numbered below `block`, which no row is marked
    /// with any more.
    pub(crate) fn forget_before(&mut self, block: u32) {
        while self.first < block && self.entries.pop_front().is_some() {
            self.first += 1;
        }
        if self.entries.is_empty() {
            self.first = self.first.max(block);
        }
    }

    fn push(&mut self, entry: Entry) -> u32 {
        self.entries.push_back(entry);
        self.last()
    }

    fn last(&self) -> u32 {
        let len = u32::try_from(self.entries.len()).expect("far fewer blocks than 4 billion");
        self.first + len - 1
    }
}

/// One command's block in the document (see [`crate::document`]).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Block {
    /// The block's first line: its prompt's, or its output's if the shell
    /// marked no prompt.
    pub first: u64,
    /// Its last line, with blank lines at the end left out.
    pub last: u64,
    /// The line the command's output starts on, if it has printed any.
    /// The output runs to [`last`](Self::last).
    pub output: Option<u64>,
    /// The command line, if the shell said.
    pub command: Option<String>,
    /// The directory it ran in, if the shell said.
    pub cwd: Option<String>,
    /// Whether the command is still running.
    pub running: bool,
    /// Its exit status, once it has finished, if the shell said.
    pub exit: Option<i32>,
}

/// Which block [`Terminal::block`] finds, from a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Which {
    /// The block the line is in.
    At,
    /// The last block starting above the line.
    Before,
    /// The first block starting below it.
    After,
}

/// Where a block's marks are in the document.
#[derive(Clone, Copy, Debug)]
struct Span {
    block: u32,
    first: u64,
    output: Option<u64>,
}

impl Terminal {
    /// The block at `line` of the document, or the one before or after it.
    /// None on the alternate screen, which has no blocks.
    #[must_use]
    pub fn block(&self, line: u64, which: Which) -> Option<Block> {
        if self.is_alternate_screen() {
            return None;
        }
        let spans = self.spans();
        let index = match which {
            Which::At => spans.iter().rposition(|span| span.first <= line)?,
            Which::Before => spans.iter().rposition(|span| span.first < line)?,
            Which::After => spans.iter().position(|span| span.first > line)?,
        };
        let span = spans[index];
        let (_, last_line) = self.document_bounds();
        let end = spans
            .get(index + 1)
            .map_or(last_line, |next| next.first - 1);
        let last = self.last_written(span.first, end);
        let entry = self.block_entry(span.block);
        Some(Block {
            first: span.first,
            last,
            output: span.output.filter(|&output| output <= last),
            command: entry.and_then(|entry| entry.command.clone()),
            cwd: entry.and_then(|entry| entry.cwd.clone()),
            running: entry.is_some_and(|entry| entry.phase == Phase::Running),
            exit: entry.and_then(|entry| entry.exit),
        })
    }

    /// The start of every block, in order, from the marks: consecutive
    /// marks of one block make one span.
    fn spans(&self) -> Vec<Span> {
        let mut spans: Vec<Span> = Vec::new();
        for (line, mark) in self.primary().marks() {
            match spans.last_mut() {
                Some(span) if span.block == mark.block => {
                    if mark.output && span.output.is_none() {
                        span.output = Some(line);
                    }
                }
                _ => spans.push(Span {
                    block: mark.block,
                    first: line,
                    output: mark.output.then_some(line),
                }),
            }
        }
        spans
    }

    /// The last line from `first` to `last` with anything written on it,
    /// or `first`.
    fn last_written(&self, first: u64, last: u64) -> u64 {
        (first + 1..=last)
            .rev()
            .find(|&line| {
                self.document_line(line)
                    .is_some_and(|row| !row.text().is_empty())
            })
            .unwrap_or(first)
    }
}

/// Decodes `%XX` escapes, as in a `file:` URL or `cmdline_url`. Invalid
/// escapes are kept as they are, and invalid UTF-8 replaced.
pub(crate) fn percent_decode(text: &[u8]) -> String {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text;
    while let Some((&byte, after)) = rest.split_first() {
        let hex = |byte: u8| char::from(byte).to_digit(16);
        if byte == b'%'
            && let [high, low, ..] = after
            && let (Some(high), Some(low)) = (hex(*high), hex(*low))
        {
            bytes.push(u8::try_from(high * 16 + low).expect("two hex digits fit in a byte"));
            rest = &after[2..];
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The path in a `file://host/path` URL, decoded.
pub(crate) fn file_url_path(url: &[u8]) -> Option<String> {
    let rest = url.strip_prefix(b"file://")?;
    let path = &rest[rest.iter().position(|&byte| byte == b'/')?..];
    Some(percent_decode(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Compaction, Config, Size};

    const PROMPT: &str = "\x1b]133;A;click_events=1\x07$ ";

    /// A command typed at a prompt, run, printing `output`, and finishing
    /// with `exit`, as fish marks it.
    fn command(command: &str, output: &str, exit: i32) -> String {
        let url = command.replace(' ', "%20");
        format!("{command}\r\n\x1b]133;C;cmdline_url={url}\x07{output}\x1b]133;D;{exit}\x07")
    }

    fn terminal(cols: u16, rows: u16, scrollback: usize) -> Terminal {
        let config = Config {
            scrollback_lines: scrollback,
            scrollback_compaction: Compaction::Lazy,
        };
        Terminal::new(Size::new(cols, rows).unwrap(), &config)
    }

    /// A block's first line, output line, last line, command and exit
    /// status.
    type Found = (u64, Option<u64>, u64, Option<String>, Option<i32>);

    /// The block at `line`.
    fn at(t: &Terminal, line: u64) -> Option<Found> {
        let block = t.block(line, Which::At)?;
        Some((
            block.first,
            block.output,
            block.last,
            block.command,
            block.exit,
        ))
    }

    fn text(t: &Terminal, first: u64, last: u64) -> String {
        use crate::Position;
        t.text(
            Position {
                line: first,
                col: 0,
            },
            Position { line: last, col: 0 },
            true,
        )
    }

    #[test]
    fn a_block_runs_from_its_prompt_to_the_next() {
        let mut t = terminal(20, 10, 100);
        t.advance(b"motd\r\n");
        t.advance(PROMPT.as_bytes());
        t.advance(command("ls -a", "a\r\nb\r\n\r\n", 0).as_bytes());
        t.advance(PROMPT.as_bytes());
        t.advance(command("false", "", 1).as_bytes());
        t.advance(PROMPT.as_bytes());

        assert_eq!(
            at(&t, 0),
            None,
            "nothing was marked before the first prompt"
        );
        let ls = Some((1, Some(2), 3, Some("ls -a".to_owned()), Some(0)));
        assert_eq!(at(&t, 1), ls);
        // Blank lines at its end belong to it, but aren't counted.
        assert_eq!(at(&t, 4), ls);
        assert_eq!(text(&t, 2, 3), "a\nb");
        // `false` printed nothing: the next prompt went on the line its
        // output would have started on.
        assert_eq!(
            at(&t, 5),
            Some((5, None, 5, Some("false".to_owned()), Some(1)))
        );
        // The prompt now showing is a block of its own.
        assert_eq!(at(&t, 6), Some((6, None, 6, None, None)));
    }

    #[test]
    fn blocks_before_and_after_a_line() {
        let mut t = terminal(20, 10, 100);
        for _ in 0..3 {
            t.advance(PROMPT.as_bytes());
            t.advance(command("echo", "x\r\n", 0).as_bytes());
        }
        let first = |line, which| t.block(line, which).map(|block| block.first);
        assert_eq!(first(3, Which::Before), Some(2));
        assert_eq!(first(2, Which::Before), Some(0));
        assert_eq!(first(0, Which::Before), None);
        assert_eq!(first(3, Which::After), Some(4));
        assert_eq!(first(4, Which::After), None);
    }

    #[test]
    fn a_running_command_is_its_block_so_far() {
        let mut t = terminal(20, 10, 100);
        t.advance(PROMPT.as_bytes());
        t.advance(b"sleep 9\r\n\x1b]133;C\x07one\r\ntwo\r\n");
        let block = t.block(0, Which::At).unwrap();
        assert!(block.running);
        assert_eq!(
            (block.output, block.last, block.command),
            (Some(1), 2, None)
        );
    }

    #[test]
    fn marks_follow_their_lines_when_the_screen_is_rewrapped() {
        let mut t = terminal(10, 6, 100);
        t.advance(b"0123456789abcdef\r\n");
        t.advance(PROMPT.as_bytes());
        t.advance(command("ls", "x\r\n", 0).as_bytes());
        t.advance(PROMPT.as_bytes());
        // The long line takes two rows, so the prompt is on line 2.
        assert_eq!(at(&t, 2).map(|block| block.0), Some(2));

        t.resize(Size::new(20, 6).unwrap());
        assert_eq!(
            at(&t, 1),
            Some((1, Some(2), 2, Some("ls".to_owned()), Some(0)))
        );
        t.resize(Size::new(5, 6).unwrap());
        // Four rows for the long line, then the block.
        assert_eq!(
            at(&t, 4),
            Some((4, Some(5), 5, Some("ls".to_owned()), Some(0)))
        );
        t.assert_invariants();
    }

    #[test]
    fn marks_go_into_history_and_out_with_it() {
        let mut t = terminal(20, 3, 3);
        t.advance(PROMPT.as_bytes());
        t.advance(command("seq", "1\r\n2\r\n3\r\n", 0).as_bytes());
        t.advance(PROMPT.as_bytes());
        // The prompt and the first line of output are in history now, and
        // compacting them keeps their marks.
        assert_eq!(t.primary().scrollback_len(), 2);
        t.compact_scrollback();
        let seq = |first| Some((first, Some(1), 3, Some("seq".to_owned()), Some(0)));
        assert_eq!(at(&t, 2), seq(0));
        t.assert_invariants();

        // Pushing its prompt out of history leaves its output.
        t.advance(command("x", "", 0).as_bytes());
        t.advance(b"\r\n");
        assert_eq!(t.document_bounds().0, 1);
        assert_eq!(at(&t, 2), seq(1));
        // Then the output goes too, and with it what was known of the block.
        t.advance(b"\r\n\r\n");
        t.advance(PROMPT.as_bytes());
        assert_eq!(at(&t, 3), None);
        assert!(t.block_entry(0).is_none());
        assert!(t.block_entry(1).is_some());
        t.assert_invariants();
    }

    #[test]
    fn clearing_forgets_marks() {
        let mut t = terminal(20, 3, 100);
        t.advance(PROMPT.as_bytes());
        t.advance(command("seq", "1\r\n2\r\n3\r\n", 0).as_bytes());
        t.advance(PROMPT.as_bytes());
        // `clear`: the screen, then history.
        t.advance(b"\x1b[H\x1b[2J\x1b[3J");
        assert_eq!(t.block(4, Which::At), None);
        assert_eq!(t.primary().marks().count(), 0);
        t.assert_invariants();
    }

    #[test]
    fn erasing_the_line_keeps_its_mark() {
        let mut t = terminal(20, 3, 100);
        t.advance(PROMPT.as_bytes());
        t.advance(b"\r\x1b[2K$ ");
        assert_eq!(at(&t, 0).map(|block| block.0), Some(0));
    }

    #[test]
    fn the_alternate_screen_takes_no_marks() {
        let mut t = terminal(20, 3, 100);
        t.advance(b"\x1b[?1049h");
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.block(0, Which::At), None);
        t.advance(b"\x1b[?1049l");
        assert_eq!(t.block(0, Which::At), None);
    }

    #[test]
    fn the_shell_reports_its_directory() {
        let mut t = terminal(20, 3, 100);
        t.advance(b"\x1b]7;file://host/home/j%20p\x07");
        assert_eq!(t.cwd(), Some("/home/j p"));
        t.advance(PROMPT.as_bytes());
        t.advance(command("ls", "", 0).as_bytes());
        assert_eq!(
            t.block(0, Which::At).unwrap().cwd.as_deref(),
            Some("/home/j p")
        );
    }

    #[test]
    fn a_prompt_drawn_again_is_the_same_block() {
        let mut blocks = Blocks::default();
        assert_eq!(blocks.prompt(), 0);
        assert_eq!(blocks.prompt(), 0);
        assert_eq!(blocks.command(Some("ls".to_owned()), None), 0);
        blocks.finished(Some(2));
        assert_eq!(blocks.get(0).unwrap().exit, Some(2));
        assert_eq!(blocks.prompt(), 1);
        // A command without a prompt mark starts a block of its own.
        blocks.finished(Some(1));
        assert_eq!(blocks.command(None, None), 1);
        blocks.finished(None);
        assert_eq!(blocks.command(None, None), 2);
    }

    #[test]
    fn forgetting_keeps_the_numbering() {
        let mut blocks = Blocks::default();
        for _ in 0..3 {
            blocks.command(None, None);
            blocks.finished(Some(0));
        }
        blocks.forget_before(2);
        assert!(blocks.get(1).is_none());
        assert!(blocks.get(2).is_some());
        blocks.forget_before(5);
        assert_eq!(blocks.prompt(), 5);
    }

    #[test]
    fn urls_are_decoded() {
        assert_eq!(percent_decode(b"echo%20hi%3B%zz%"), "echo hi;%zz%");
        assert_eq!(
            file_url_path(b"file://host/home/j%20p").as_deref(),
            Some("/home/j p")
        );
        assert_eq!(file_url_path(b"http://host/x"), None);
    }
}
