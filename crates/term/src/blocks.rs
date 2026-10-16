//! Shell integration: the blocks a shell divides its terminal into, one per
//! command, with the command's prompt, the command itself and its output.
//!
//! Shells mark them with OSC 133, which fish sends by itself, and bash and
//! zsh through scripts:
//!
//! - `A` where the prompt starts,
//! - `B` where it ends and typing starts, which says the shell is
//!   reading a command line (see [`ShellState`]),
//! - `C` where the command's output starts, optionally with
//!   `cmdline_url=` and the command, percent-encoded,
//! - `D;status` when it has finished, with its exit status.
//!
//! OSC 7 (`file://host/path`) reports the working directory.
//!
//! Any program can print these marks, so what they say is only taken as
//! the shell's when the shell is the one that could have printed them.
//! clackatoo's own integration knows a secret the daemon gives each
//! terminal (see [`Terminal::expect_secret`]), and adds it to its `C` as
//! `clackatoo=SECRET`. A command marked so is the shell's for certain,
//! and only such a command is offered to run again after a restart. The
//! secret is checked and dropped as the mark is read: it's never kept,
//! and never reaches the screen.
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
    /// Whether clackatoo's own shell integration said what the command is,
    /// with the terminal's secret: then the command is the shell's, not
    /// something a program printed.
    pub(crate) trusted: bool,
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

/// What the shell is doing, as its marks say: whether it is reading a
/// command line, which a window can count on to echo what's typed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ShellState {
    /// The shell hasn't marked anything: it may have no shell
    /// integration, or the terminal runs some other program.
    #[default]
    Unmarked,
    /// The shell is reading a command line: it has shown its prompt
    /// (`A`), and no command has started since. If it marks where typing
    /// starts (`B`), as clackatoo's shell integration does, that mark has
    /// come too, so the prompt has been drawn in full. What's typed goes
    /// to its line editor.
    Prompt,
    /// The shell isn't reading a command line: a command has started
    /// (`C`), and the shell hasn't shown its prompt again since, or it's
    /// still drawing the prompt. What's typed goes to the command, if it
    /// reads it.
    Busy,
}

/// Every block a row is still marked with, and maybe a few older ones,
/// numbered in order.
#[derive(Clone, Debug, Default)]
pub(crate) struct Blocks {
    entries: VecDeque<Entry>,
    /// The number of the first entry.
    first: u32,
    /// Whether the shell has marked where typing starts (`B`) since its
    /// prompt started.
    typing: bool,
    /// Whether the shell marks where typing starts at all. Not every shell
    /// does, and for those, a prompt is ready for typing from the start.
    marks_typing: bool,
    /// Whether the shell marks where commands start (`C`). One that
    /// doesn't, as bash before 4.4, where `PS0` does nothing, never
    /// says it has left its prompt, so it's never taken to be at one.
    marks_commands: bool,
    /// How many times blocks have been numbered from 0 again, as numbers
    /// ran out (see [`start_again`](Self::start_again)). Nothing else
    /// changes it: not clearing history, which changes its generation,
    /// nor a reset, which keeps the numbering.
    renumbered: u64,
}

impl Blocks {
    /// The blocks `entries`, numbered from `first`.
    pub(crate) fn from_parts(first: u32, entries: VecDeque<Entry>) -> Self {
        let marks_commands = entries.iter().any(|entry| entry.phase != Phase::Prompt);
        Self {
            entries,
            first,
            typing: false,
            marks_typing: false,
            marks_commands,
            renumbered: 0,
        }
    }

    /// These blocks, counted as numbered from 0 again `renumbered` times,
    /// as blocks they carry on from were.
    pub(crate) fn renumbered_as(mut self, renumbered: u64) -> Self {
        self.renumbered = renumbered;
        self
    }

    /// How many times blocks have been numbered from 0 again.
    pub(crate) fn renumbered(&self) -> u64 {
        self.renumbered
    }

    /// The number of the first block, and every block from it on.
    pub(crate) fn parts(&self) -> (u32, &VecDeque<Entry>) {
        (self.first, &self.entries)
    }

    /// Whether a command is running: it has started (`C`) and not
    /// finished (`D`).
    pub(crate) fn running(&self) -> bool {
        self.entries
            .back()
            .is_some_and(|last| last.phase == Phase::Running)
    }

    /// The prompt has been drawn, and typing starts here.
    pub(crate) fn typing(&mut self) {
        if self
            .entries
            .back()
            .is_some_and(|last| last.phase == Phase::Prompt)
        {
            self.typing = true;
            self.marks_typing = true;
        }
    }

    /// What the shell is doing, as the last block says.
    pub(crate) fn state(&self) -> ShellState {
        match self.entries.back() {
            None => ShellState::Unmarked,
            Some(last)
                if last.phase == Phase::Prompt
                    && self.marks_commands
                    && (self.typing || !self.marks_typing) =>
            {
                ShellState::Prompt
            }
            Some(_) => ShellState::Busy,
        }
    }

    pub(crate) fn get(&self, block: u32) -> Option<&Entry> {
        let index = block.checked_sub(self.first)?;
        self.entries.get(usize::try_from(index).ok()?)
    }

    /// A prompt has started: the number of its block. A prompt drawn again
    /// before any command ran, as shells do after a resize, is the same
    /// block.
    pub(crate) fn prompt(&mut self) -> u32 {
        self.typing = false;
        match self.entries.back() {
            Some(last) if last.phase == Phase::Prompt => self.last(),
            _ => self.push(Entry::default()),
        }
    }

    /// `command` has started running in `cwd`: the number of its block.
    /// It's `trusted` if clackatoo's own shell integration said so.
    pub(crate) fn command(
        &mut self,
        command: Option<String>,
        cwd: Option<String>,
        trusted: bool,
    ) -> u32 {
        self.marks_commands = true;
        let running = Entry {
            command,
            cwd,
            phase: Phase::Running,
            exit: None,
            trusted,
        };
        match self.entries.back_mut() {
            Some(last) if last.phase == Phase::Prompt => {
                *last = running;
                self.last()
            }
            _ if trusted => self.push(running),
            // The shell didn't mark a prompt: whatever printed this, it
            // isn't known to be the shell, so what it says ran isn't kept.
            _ => self.push(Entry {
                command: None,
                ..running
            }),
        }
    }

    /// clackatoo's own shell integration says the running command is
    /// `command`: fish marks the command's start itself, then runs the
    /// integration's hook, which marks it again with the secret.
    pub(crate) fn trust_running(&mut self, command: Option<String>) {
        if let Some(last) = self.entries.back_mut()
            && last.phase == Phase::Running
        {
            last.command = command;
            last.trusted = true;
        }
    }

    /// The number of the latest block, if clackatoo's own shell
    /// integration said what its command is.
    pub(crate) fn trusted(&self) -> Option<u32> {
        let last = self.entries.back()?;
        last.trusted.then(|| self.last())
    }

    /// Takes block `block`'s command to be what clackatoo's own shell
    /// integration said, as the build that handed this terminal over
    /// knew it was.
    pub(crate) fn trust(&mut self, block: u32) {
        let entry = block
            .checked_sub(self.first)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| self.entries.get_mut(index));
        if let Some(entry) = entry {
            entry.trusted = true;
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

    /// The number the next new block would get, if numbers haven't run
    /// out: a terminal that has run four billion commands starts again
    /// (see [`Terminal::shell_mark`]).
    pub(crate) fn next(&self) -> Option<u32> {
        self.first
            .checked_add(u32::try_from(self.entries.len()).ok()?)
    }

    /// Forgets every block and numbers them from 0 again, once numbers
    /// have run out, still knowing which marks the shell makes.
    pub(crate) fn start_again(&mut self) {
        self.entries.clear();
        self.first = 0;
        self.typing = false;
        self.renumbered += 1;
    }

    /// Whether a prompt or a command starting now would need a number
    /// there's none left for: the last block isn't at its prompt, which
    /// either would be.
    pub(crate) fn run_out(&self) -> bool {
        self.next().is_none()
            && !(self.entries.back()).is_some_and(|last| last.phase == Phase::Prompt)
    }

    fn push(&mut self, entry: Entry) -> u32 {
        self.entries.push_back(entry);
        self.last()
    }

    fn last(&self) -> u32 {
        let len = u32::try_from(self.entries.len()).expect("far fewer blocks than 4 billion");
        self.first + (len - 1)
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

/// How far the shell has got with a block, as [`Terminal::latest_block`]
/// says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// It's showing the prompt: no command has started.
    Prompt,
    /// Its command is running.
    Running,
    /// Its command has finished, with its exit status if the shell said.
    Finished(Option<i32>),
}

impl Entry {
    fn progress(&self) -> Progress {
        match self.phase {
            Phase::Prompt => Progress::Prompt,
            Phase::Running => Progress::Running,
            Phase::Finished => Progress::Finished(self.exit),
        }
    }
}

/// Where a block's marks are in the document.
#[derive(Clone, Copy, Debug)]
struct Span {
    block: u32,
    first: u64,
    output: Option<u64>,
}

impl Terminal {
    /// The number of the shell's latest block, and how far it has got, or
    /// `None` if the shell has marked none. Blocks are numbered in order,
    /// so a later one has a higher number: a command typed at the prompt
    /// of block `n` runs as block `n`, and the next prompt is `n + 1`.
    #[must_use]
    pub fn latest_block(&self) -> Option<(u32, Progress)> {
        let blocks = self.emulator_blocks();
        let last = blocks.parts().1.back()?;
        Some((blocks.last(), last.progress()))
    }

    /// How many times the terminal has numbered its blocks from 0 again,
    /// once numbers ran out. A block number from before it changed means
    /// nothing now. Clearing history doesn't change it, though it changes
    /// [`Grid::history_generation`](crate::Grid::history_generation), nor
    /// does a reset, as block numbers carry on through one.
    #[must_use]
    pub fn block_renumberings(&self) -> u64 {
        self.emulator_blocks().renumbered()
    }

    /// The command the shell's latest block is running, if one is and
    /// the shell said what it is: what a list of terminals says each is
    /// doing. Any program could have printed it: see
    /// [`trusted_running_command`](Self::trusted_running_command).
    #[must_use]
    pub fn running_command(&self) -> Option<&str> {
        let last = self.emulator_blocks().parts().1.back()?;
        if last.phase != Phase::Running {
            return None;
        }
        last.command.as_deref()
    }

    /// The command the shell's latest block is running, if one is and
    /// clackatoo's own shell integration said what it is, with the
    /// terminal's secret (see [`expect_secret`](Self::expect_secret)):
    /// what the shell ran, never what a program printed.
    #[must_use]
    pub fn trusted_running_command(&self) -> Option<&str> {
        let last = self.emulator_blocks().parts().1.back()?;
        if last.phase != Phase::Running || !last.trusted {
            return None;
        }
        last.command.as_deref()
    }

    /// The number of the shell's latest block, if clackatoo's own shell
    /// integration said what its command is: for the next build to know,
    /// with [`trust_block`](Self::trust_block), as snapshots don't say.
    #[must_use]
    pub fn trusted_block(&self) -> Option<u32> {
        self.emulator_blocks().trusted()
    }

    /// How far block `block` has got, while it's still known: until no
    /// row is marked with it.
    #[must_use]
    pub fn block_progress(&self, block: u32) -> Option<Progress> {
        self.block_entry(block).map(Entry::progress)
    }

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
            ..Config::default()
        };
        Terminal::new(Size::new(cols, rows).unwrap(), &config)
    }

    #[test]
    fn the_shell_state_follows_the_marks() {
        let mut t = terminal(20, 5, 100);
        assert_eq!(t.shell_state(), ShellState::Unmarked);
        t.advance(PROMPT.as_bytes());
        assert_eq!(
            t.shell_state(),
            ShellState::Busy,
            "not known to mark commands yet"
        );
        t.advance(b"true\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.shell_state(), ShellState::Prompt);
        t.advance(b"sleep 1\r\n\x1b]133;C\x07");
        assert_eq!(t.shell_state(), ShellState::Busy);
        t.advance(b"\x1b]133;D;0\x07");
        assert_eq!(t.shell_state(), ShellState::Busy, "no prompt yet");
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.shell_state(), ShellState::Prompt);
    }

    #[test]
    fn a_shell_marking_where_typing_starts_reads_a_command_line_from_there() {
        let mut t = terminal(20, 5, 100);
        t.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        t.advance(b"ls\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        // Now it's known to mark typing, a prompt half drawn isn't ready.
        t.advance(b"\x1b]133;A\x07");
        assert_eq!(t.shell_state(), ShellState::Busy);
        t.advance(b"$ \x1b]133;B\x07");
        assert_eq!(t.shell_state(), ShellState::Prompt);
    }

    #[test]
    fn a_shell_that_never_marks_commands_is_never_at_its_prompt() {
        // bash before 4.4, where PS0 does nothing: prompts, but no C.
        let mut t = terminal(20, 5, 100);
        for _ in 0..3 {
            t.advance(PROMPT.as_bytes());
            assert_eq!(t.shell_state(), ShellState::Busy);
            t.advance(b"sudo true\r\n[sudo] password: \r\n\x1b]133;D;0\x07");
        }
    }

    #[test]
    fn a_prompt_printed_while_a_command_runs_is_its_output() {
        let mut t = terminal(20, 5, 100);
        t.advance(PROMPT.as_bytes());
        t.advance(b"ssh box\r\n\x1b]133;C\x07");
        // The shell on the other end marks its prompt, but it's a program
        // here, which reads its own keys.
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.shell_state(), ShellState::Busy);
        t.advance(b"exit\r\n\x1b]133;D;0\x07");
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.shell_state(), ShellState::Prompt);
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
    fn the_latest_block_says_how_far_its_command_has_got() {
        let mut t = terminal(20, 10, 100);
        assert_eq!(t.latest_block(), None, "nothing marked yet");
        t.advance(PROMPT.as_bytes());
        let (first, progress) = t.latest_block().unwrap();
        assert_eq!(progress, Progress::Prompt);
        // A prompt drawn again is the same block.
        t.advance(b"\r\n");
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.latest_block(), Some((first, Progress::Prompt)));
        t.advance(b"sleep 1\r\n\x1b]133;C\x07");
        assert_eq!(t.latest_block(), Some((first, Progress::Running)));
        t.advance(b"\x1b]133;D;3\x07");
        assert_eq!(t.latest_block(), Some((first, Progress::Finished(Some(3)))));
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.latest_block(), Some((first + 1, Progress::Prompt)));
        assert_eq!(
            t.block_progress(first),
            Some(Progress::Finished(Some(3))),
            "the command's block is still known"
        );
        assert_eq!(t.block_progress(first + 2), None);
    }

    #[test]
    fn the_running_command_is_said_only_while_it_runs() {
        let mut t = terminal(40, 10, 100);
        assert_eq!(t.running_command(), None);
        t.advance(PROMPT.as_bytes());
        t.advance(b"cargo test\r\n\x1b]133;C;cmdline_url=cargo%20test\x07");
        assert_eq!(t.running_command(), Some("cargo test"));
        t.advance(b"\x1b]133;D;0\x07");
        assert_eq!(t.running_command(), None, "finished");
        t.advance(PROMPT.as_bytes());
        t.advance(b"sleep 1\r\n\x1b]133;C\x07");
        assert_eq!(t.running_command(), None, "running, but not said what");
    }

    #[test]
    fn a_program_cant_say_what_runs_while_a_command_does_or_with_no_prompt() {
        let mut t = terminal(40, 10, 100);
        t.advance(PROMPT.as_bytes());
        t.advance(
            b"make
]133;C;cmdline_url=make",
        );
        let (block, _) = t.latest_block().unwrap();
        // What make prints, marks and all, isn't the shell's.
        t.advance(b"]133;C;cmdline_url=rm%20-rf%20~");
        assert_eq!(t.running_command(), Some("make"));
        assert_eq!(t.latest_block(), Some((block, Progress::Running)));
        // A command with no prompt before it says nothing of what it is.
        t.advance(b"]133;D;0]133;C;cmdline_url=rm%20-rf%20~");
        assert_eq!(t.latest_block().map(|(_, at)| at), Some(Progress::Running));
        assert_eq!(t.running_command(), None);
    }

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn only_a_command_marked_with_the_secret_is_trusted() {
        let mut t = terminal(60, 10, 100);
        t.expect_secret(Some(SECRET));
        t.advance(PROMPT.as_bytes());
        // As bash and zsh mark it, through clackatoo's integration.
        t.advance(
            format!("sleep 9\r\n\x1b]133;C;clackatoo={SECRET};cmdline_url=sleep%209\x07")
                .as_bytes(),
        );
        assert_eq!(t.trusted_running_command(), Some("sleep 9"));
        let (block, _) = t.latest_block().unwrap();
        assert_eq!(t.trusted_block(), Some(block));
        // What it prints, with a wrong secret or none, changes nothing.
        t.advance(b"\x1b]133;C;clackatoo=0123;cmdline_url=rm%20-rf%20~\x07");
        t.advance(b"\x1b]133;C;cmdline_url=rm%20-rf%20~\x07");
        assert_eq!(t.trusted_running_command(), Some("sleep 9"));
        // Nor does a forged end, prompt and start: the command it starts
        // is said, but isn't trusted, whatever's in front.
        t.advance(b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b]133;B\x07");
        t.advance(
            format!(
                "\x1b]133;C;clackatoo={}x;cmdline_url=sleep%20-rf\x07",
                &SECRET[1..]
            )
            .as_bytes(),
        );
        assert_eq!(t.running_command(), Some("sleep -rf"));
        assert_eq!(t.trusted_running_command(), None);
        assert_eq!(t.trusted_block(), None);
        // Nor does one with the secret cut short, or carried too far.
        t.advance(b"\x1b]133;D;0\x07\x1b]133;A\x07");
        t.advance(format!("\x1b]133;C;clackatoo={}\x07", &SECRET[..31]).as_bytes());
        t.advance(b"\x1b]133;D;0\x07\x1b]133;A\x07");
        t.advance(format!("\x1b]133;C;clackatoo={SECRET}0;cmdline_url=x\x07").as_bytes());
        assert_eq!(t.trusted_running_command(), None);
        // The secret never reaches the screen or the title.
        t.advance(b"\x1b]133;D;0\x07\r\n");
        assert!(
            !text(&t, 0, 20).contains("0123456789"),
            "{}",
            text(&t, 0, 20)
        );
        assert!(!t.title().contains("0123456789"));
    }

    #[test]
    fn a_terminal_expecting_no_secret_trusts_nothing() {
        let mut t = terminal(60, 10, 100);
        t.advance(PROMPT.as_bytes());
        t.advance(format!("ls\r\n\x1b]133;C;clackatoo={SECRET};cmdline_url=ls\x07").as_bytes());
        assert_eq!(t.running_command(), Some("ls"));
        assert_eq!(t.trusted_running_command(), None);
        t.advance(b"\x1b]133;D;0\x07");
        t.expect_secret(Some(SECRET));
        t.advance(PROMPT.as_bytes());
        t.advance(b"ls\r\n\x1b]133;C;clackatoo=;cmdline_url=ls\x07");
        assert_eq!(t.trusted_running_command(), None, "an empty secret");
    }

    #[test]
    fn fish_marks_the_command_then_the_integration_trusts_it_in_the_same_block() {
        let mut t = terminal(60, 10, 100);
        t.expect_secret(Some(SECRET));
        t.advance(PROMPT.as_bytes());
        t.advance(b"sleep 9\r\n\x1b]133;C;cmdline_url=sleep%209\x07");
        let (block, _) = t.latest_block().unwrap();
        assert_eq!(t.trusted_running_command(), None);
        t.advance(format!("\x1b]133;C;cmdline_url=sleep%209;clackatoo={SECRET}\x07").as_bytes());
        assert_eq!(t.trusted_running_command(), Some("sleep 9"));
        assert_eq!(t.latest_block(), Some((block, Progress::Running)));
        t.advance(b"done\r\n\x1b]133;D;0\x07");
        t.advance(PROMPT.as_bytes());
        // One block, with its output marked once, on the line it starts.
        let marks: Vec<(u64, u32, bool, bool)> = (t.primary().marks())
            .map(|(line, mark)| (line, mark.block, mark.prompt, mark.output))
            .collect();
        assert_eq!(
            marks,
            [
                (0, block, true, false),
                (1, block, false, true),
                (2, block + 1, true, false)
            ]
        );
        assert_eq!(at(&t, 0).unwrap().1, Some(1));
        assert_eq!(t.latest_block(), Some((block + 1, Progress::Prompt)));
    }

    #[test]
    fn a_trusted_command_needs_no_prompt_and_survives_a_reset() {
        let mut t = terminal(60, 10, 100);
        t.expect_secret(Some(SECRET));
        t.advance(format!("\x1b]133;C;clackatoo={SECRET};cmdline_url=top\x07").as_bytes());
        assert_eq!(t.trusted_running_command(), Some("top"));
        t.advance(b"\x1b]133;D;0\x07\x1bc");
        t.advance(PROMPT.as_bytes());
        t.advance(format!("\x1b]133;C;clackatoo={SECRET};cmdline_url=vi\x07").as_bytes());
        assert_eq!(
            t.trusted_running_command(),
            Some("vi"),
            "RIS kept the secret"
        );
    }

    #[test]
    fn trust_is_put_back_by_block_number() {
        let mut t = terminal(60, 10, 100);
        t.expect_secret(Some(SECRET));
        t.advance(PROMPT.as_bytes());
        t.advance(format!("make\r\n\x1b]133;C;clackatoo={SECRET};cmdline_url=make\x07").as_bytes());
        let block = t.trusted_block().unwrap();
        let mut restored = Terminal::restore(&t.snapshot(), &Config::default()).unwrap();
        restored.restore_blocks(&t.blocks_snapshot());
        assert_eq!(restored.running_command(), Some("make"));
        assert_eq!(restored.trusted_running_command(), None);
        restored.trust_block(block + 1);
        assert_eq!(restored.trusted_running_command(), None);
        restored.trust_block(block);
        assert_eq!(restored.trusted_running_command(), Some("make"));
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
    fn prompts_are_found_on_the_screen_and_in_history() {
        let mut t = terminal(20, 3, 100);
        t.advance(PROMPT.as_bytes());
        t.advance(command("false", "", 1).as_bytes());
        t.advance(PROMPT.as_bytes());
        t.advance(command("seq", "1\r\n2\r\n", 0).as_bytes());
        t.advance(PROMPT.as_bytes());
        // Block 0's and block 1's prompts are in history. The screen
        // shows block 1's output, whose mark isn't a prompt's, and block
        // 2's prompt.
        let grid = t.primary();
        assert_eq!(grid.scrollback_end(), 2);
        let all: Vec<_> = grid.history_prompts(0..2).collect();
        assert_eq!(all, [(0, 0), (1, 1)]);
        assert_eq!(grid.history_prompts(1..9).collect::<Vec<_>>(), [(1, 1)]);
        assert_eq!(grid.history_prompts(0..1).collect::<Vec<_>>(), [(0, 0)]);
        assert_eq!(grid.history_prompts(2..9).count(), 0);
        let screen: Vec<_> = grid.rows().map(crate::Row::prompt_block).collect();
        assert_eq!(screen, [None, None, Some(2)]);
        // Compacted, history keeps them.
        t.compact_scrollback();
        assert_eq!(t.primary().history_prompts(0..2).count(), 2);
        assert_eq!(t.block_progress(0), Some(Progress::Finished(Some(1))));
    }

    #[test]
    fn prompts_and_exits_are_handed_over_to_the_next_build() {
        let mut t = terminal(20, 3, 100);
        t.advance(PROMPT.as_bytes());
        t.advance(command("false", "", 1).as_bytes());
        t.advance(PROMPT.as_bytes());
        t.advance(command("seq", "1\r\n2\r\n", 0).as_bytes());
        t.advance(PROMPT.as_bytes());
        let (snapshot, links) = t.snapshot_with_links();
        let config = Config {
            scrollback_lines: 100,
            ..Config::default()
        };
        let mut next = Terminal::restore_with_links(&snapshot, &links, &config).unwrap();
        next.restore_blocks(&t.blocks_snapshot());
        let prompts = |t: &Terminal| -> Vec<_> {
            let grid = t.primary();
            let screen = grid.rows().filter_map(crate::Row::prompt_block);
            (grid.history_prompts(0..grid.scrollback_end()))
                .map(|(_, block)| block)
                .chain(screen)
                .collect()
        };
        assert_eq!(prompts(&next), [0, 1, 2]);
        assert_eq!(prompts(&next), prompts(&t));
        assert_eq!(next.block_progress(0), Some(Progress::Finished(Some(1))));
        assert_eq!(next.block_progress(1), Some(Progress::Finished(Some(0))));
    }

    #[test]
    fn a_full_reset_numbers_blocks_on_from_where_it_was() {
        let mut t = terminal(20, 3, 100);
        t.advance(PROMPT.as_bytes());
        t.advance(command("false", "", 1).as_bytes());
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.latest_block(), Some((1, Progress::Prompt)));
        t.advance(b"\x1bc");
        assert_eq!(t.latest_block(), None);
        assert_eq!(t.block_progress(0), None);
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.latest_block(), Some((2, Progress::Prompt)));
        t.assert_invariants();
    }

    #[test]
    fn numbers_that_run_out_start_again_with_history_renumbered() {
        let mut t = terminal(20, 3, 100);
        t.restore_blocks(&crate::snapshot::BlocksSnapshot {
            first: u32::MAX,
            ..crate::snapshot::BlocksSnapshot::default()
        });
        t.advance(PROMPT.as_bytes());
        t.advance(command("false", "", 1).as_bytes());
        assert_eq!(
            t.latest_block(),
            Some((u32::MAX, Progress::Finished(Some(1))))
        );
        let generation = t.primary().history_generation();
        assert_eq!(t.block_renumberings(), 0);
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.latest_block(), Some((0, Progress::Prompt)));
        assert_eq!(
            t.shell_state(),
            ShellState::Prompt,
            "it still knows the shell marks its commands"
        );
        assert_eq!(t.block_progress(u32::MAX), None);
        assert_eq!(t.primary().history_generation(), generation + 1);
        assert_eq!(t.block_renumberings(), 1);
        let marked: Vec<_> = t.primary().marks().map(|(_, mark)| mark.block).collect();
        assert_eq!(marked, [0], "the old marks are gone");
        // A reset there starts from 0 too, which counts as numbering them
        // again.
        t.restore_blocks(&crate::snapshot::BlocksSnapshot {
            first: u32::MAX,
            ..crate::snapshot::BlocksSnapshot::default()
        });
        assert_eq!(t.block_renumberings(), 1, "a snapshot keeps the count");
        t.advance(PROMPT.as_bytes());
        t.advance(b"\x1bc");
        assert_eq!(t.block_renumberings(), 2);
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.latest_block(), Some((0, Progress::Prompt)));
        t.assert_invariants();
    }

    #[test]
    fn clearing_history_or_resetting_isnt_numbering_blocks_again() {
        let mut t = terminal(20, 3, 100);
        t.advance(PROMPT.as_bytes());
        t.advance(command("seq", "1\r\n2\r\n3\r\n", 0).as_bytes());
        let generation = t.primary().history_generation();
        // `clear`, as a command's output: history's generation changes,
        // and block numbers mean what they did.
        t.advance(b"\x1b[H\x1b[2J\x1b[3J");
        assert_ne!(t.primary().history_generation(), generation);
        t.advance(b"\x1bc");
        assert_eq!(t.block_renumberings(), 0);
        t.advance(PROMPT.as_bytes());
        assert_eq!(t.latest_block(), Some((1, Progress::Prompt)));
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
        assert_eq!(blocks.command(Some("ls".to_owned()), None, false), 0);
        blocks.finished(Some(2));
        assert_eq!(blocks.get(0).unwrap().exit, Some(2));
        assert_eq!(blocks.prompt(), 1);
        // A command without a prompt mark starts a block of its own.
        blocks.finished(Some(1));
        assert_eq!(blocks.command(None, None, false), 1);
        blocks.finished(None);
        assert_eq!(blocks.command(None, None, false), 2);
    }

    #[test]
    fn forgetting_keeps_the_numbering() {
        let mut blocks = Blocks::default();
        for _ in 0..3 {
            blocks.command(None, None, false);
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
