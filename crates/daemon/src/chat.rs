//! Chats with agents: terminals whose program is the daemon itself.
//!
//! A chat's terminal holds the conversation. The daemon writes it there as
//! a program would, with escape sequences, so clients show, scroll, search
//! and copy it as they do any terminal's, knowing nothing about agents.
//! What's typed into it arrives as a terminal's input does, and is edited
//! here into the next message.
//!
//! The conversation is written once, top to bottom, and scrolls into the
//! terminal's history like a shell's output. Only its live end changes:
//! the line of the agent's message being written, tool calls still
//! running, a question the agent is asking, and the message being typed.
//! That end is redrawn below everything else whenever it changes.
//!
//! Each exchange is a block, as a shell's commands are (see
//! [`clackatoo_term`]'s blocks): the message is its command, and the
//! answer its output.

use std::fmt::Write as _;

use agent_client_protocol_schema::MaybeUndefined;
use agent_client_protocol_schema::v1::{
    ContentBlock, PermissionOption, PermissionOptionKind, PlanEntryStatus, SessionUpdate,
    StopReason, ToolCallStatus, ToolCallUpdate,
};
use clackatoo_protocol::{Size, TerminalId, TerminalInfo, TerminalStatus, View};
use clackatoo_term::{Damage, Terminal};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use crate::DaemonConfig;
use crate::handover::{self, HandoverError};

/// Styles, as SGR parameters.
const PLAIN: &str = "0";
const DIM: &str = "2";
const THOUGHT: &str = "2;3";
const BOLD: &str = "1";
const CODE: &str = "36";
const USER: &str = "1;34";
const DONE: &str = "32";
const FAILED: &str = "31";
const ASKING: &str = "1;33";

/// What a chat asks the server to do.
#[derive(Debug, PartialEq)]
pub(crate) enum Action {
    /// Send the agent this message.
    Prompt(String),
    /// Get the chat a session: its own again, or a new one.
    Connect,
    /// Stop the agent answering.
    Cancel,
    /// Answer the agent's question: with the option picked, or none.
    Answer {
        request: Value,
        option: Option<String>,
    },
}

/// A chat with an agent.
#[derive(Debug)]
pub(crate) struct HostedChat {
    id: TerminalId,
    emulator: Terminal,
    cwd: String,
    /// The agent's session, once it has made one.
    session: Option<String>,
    /// Whether the session is open in the agent now. It isn't before it's
    /// made, after an upgrade, or once the agent has gone.
    link: Link,
    /// Whether a message has been sent and not yet answered.
    answering: bool,
    /// A message sent while there's no session, to go once there is.
    queued: Option<String>,
    /// Why the agent couldn't be started, until it is.
    failure: Option<String>,
    editor: Editor,
    /// A paste began and hasn't ended yet.
    pasting: bool,
    /// A note under the message being typed, until the next key.
    notice: Option<&'static str>,
    words: Words,
    tools: Vec<Tool>,
    plan: Vec<(String, PlanEntryStatus)>,
    question: Option<Question>,
    /// Where the live end starts while it's drawn: its row's number,
    /// counting history's rows first.
    live: Option<u64>,
    /// The command of the exchange whose answer hasn't started yet: its
    /// row is marked once it does.
    unanswered: Option<String>,
    /// What's to be written above the live end, at the next redraw.
    pending: String,
}

/// The agent's words as they're written.
#[derive(Debug, Default)]
struct Words {
    /// The line being written, not yet ended.
    partial: String,
    /// What `partial` is: a message, or the agent's thinking.
    thinking: bool,
    /// Whether the message is inside a fenced code block.
    code: bool,
}

/// Whether a chat's session is open in the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Link {
    Closed,
    /// The server has been asked to open it.
    Opening,
    Open,
}

/// A tool call still running.
#[derive(Debug)]
struct Tool {
    id: String,
    title: String,
    status: ToolCallStatus,
}

/// The agent asking whether it may use a tool.
#[derive(Debug)]
struct Question {
    request: Value,
    title: String,
    options: Vec<PermissionOption>,
}

impl HostedChat {
    /// A chat in `cwd`, not yet with a session: the server is to get it
    /// one.
    pub(crate) fn new(id: TerminalId, size: Size, cwd: String, config: &DaemonConfig) -> Self {
        let mut chat = Self::blank(id, Terminal::new(size, &config.terminal), cwd);
        chat.link = Link::Opening;
        // Pastes come marked, so their lines aren't each sent.
        chat.pending.push_str("\x1b[?2004h\x1b]2;agent\x07");
        chat.redraw();
        chat
    }

    fn blank(id: TerminalId, emulator: Terminal, cwd: String) -> Self {
        Self {
            id,
            emulator,
            cwd,
            session: None,
            link: Link::Closed,
            answering: false,
            queued: None,
            failure: None,
            editor: Editor::default(),
            pasting: false,
            notice: None,
            words: Words::default(),
            unanswered: None,
            tools: Vec::new(),
            plan: Vec::new(),
            question: None,
            live: None,
            pending: String::new(),
        }
    }

    /// Hands the chat over to the next build. The agent doesn't go with
    /// it: the next build opens the session again when it's next needed.
    pub(crate) fn hand_over(mut self) -> handover::Chat {
        self.erase_live();
        handover::Chat {
            id: self.id.0,
            cwd: self.cwd,
            session: self.session,
            answering: self.answering,
            failure: self.failure,
            input: self.editor.text,
            screen: self.emulator.snapshot(),
            blocks: self.emulator.blocks_snapshot(),
        }
    }

    /// A chat the last build handed over.
    pub(crate) fn resume(
        chat: &handover::Chat,
        config: &DaemonConfig,
    ) -> Result<Self, HandoverError> {
        let mut emulator = Terminal::restore(&chat.screen, &config.terminal)?;
        emulator.restore_blocks(&chat.blocks);
        let mut resumed = Self::blank(TerminalId(chat.id), emulator, chat.cwd.clone());
        resumed.session.clone_from(&chat.session);
        resumed.failure.clone_from(&chat.failure);
        resumed.editor.insert(&chat.input);
        if chat.answering {
            resumed.answering = true;
            resumed.end_turn(Some(("the answer was cut off by an upgrade", FAILED)), 1);
        }
        resumed.redraw();
        Ok(resumed)
    }

    pub(crate) fn info(&self) -> TerminalInfo {
        TerminalInfo {
            id: self.id,
            pid: None,
            size: self.emulator.size(),
            title: self.emulator.title().to_owned(),
            status: TerminalStatus::Running,
        }
    }

    pub(crate) fn working_directory(&self) -> &str {
        &self.cwd
    }

    /// The agent's session, if it has made one.
    pub(crate) fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub(crate) fn view(&self) -> View {
        View::capture(&self.emulator)
    }

    pub(crate) fn emulator(&self) -> &Terminal {
        &self.emulator
    }

    pub(crate) fn take_damage(&mut self, into: &mut Damage) {
        self.emulator.take_damage(into);
    }

    /// Whether scrollback has rows to compact, or memory freed by history
    /// being cleared to hand back.
    pub(crate) fn scrollback_needs_compaction(&mut self) -> bool {
        self.emulator.take_history_cleared() | self.emulator.scrollback_needs_compaction()
    }

    pub(crate) fn compact_scrollback(&mut self) {
        self.emulator.compact_scrollback();
    }

    pub(crate) fn resize(&mut self, size: Size) {
        if size == self.emulator.size() {
            return;
        }
        // The live end is drawn for one width: it goes, and comes back
        // drawn for the new one.
        self.erase_live();
        self.emulator.resize(size);
        self.redraw();
    }

    // --- What's typed -----------------------------------------------------

    /// Takes what was typed into the chat, as a terminal's program would
    /// get it, and returns what the server must do about it.
    pub(crate) fn input(&mut self, bytes: &[u8]) -> Vec<Action> {
        let mut actions = Vec::new();
        for key in keys(bytes, &mut self.pasting) {
            self.notice = None;
            if self.question.is_some() {
                self.answer_key(&key, &mut actions);
                continue;
            }
            match key {
                Key::Text(text) => self.editor.insert(&text),
                Key::Newline => self.editor.insert("\n"),
                Key::Enter => self.send(&mut actions),
                Key::Interrupt if self.answering => self.interrupt(&mut actions),
                Key::Interrupt | Key::ClearLine => self.editor.clear_before(),
                Key::Backspace => self.editor.backspace(),
                Key::DeleteWord => self.editor.delete_word(),
                Key::Delete => self.editor.delete(),
                Key::Left => self.editor.left(),
                Key::Right => self.editor.right(),
                Key::Home => self.editor.cursor = 0,
                Key::End => self.editor.cursor = self.editor.text.len(),
                Key::Escape => {}
            }
        }
        self.redraw();
        actions
    }

    /// Enter: sends the message typed, if there is one and the agent isn't
    /// still answering the last.
    fn send(&mut self, actions: &mut Vec<Action>) {
        if self.editor.text.trim().is_empty() {
            return;
        }
        if self.answering {
            self.notice = Some("the agent is still answering; Ctrl+C stops it");
            return;
        }
        let text = std::mem::take(&mut self.editor.text);
        self.editor.cursor = 0;
        self.begin_turn(&text);
        match self.link {
            Link::Open => actions.push(Action::Prompt(text)),
            Link::Opening => self.queued = Some(text),
            Link::Closed => {
                self.queued = Some(text);
                self.link = Link::Opening;
                self.failure = None;
                actions.push(Action::Connect);
            }
        }
    }

    /// Ctrl+C while the agent answers: asks it to stop, or takes back the
    /// message if it hasn't gone yet.
    fn interrupt(&mut self, actions: &mut Vec<Action>) {
        if self.link == Link::Open {
            actions.push(Action::Cancel);
            self.notice = Some("stopping…");
        } else if self.queued.take().is_some() {
            self.end_turn(Some(("stopped", DIM)), 1);
        }
    }

    /// A key while the agent waits for an answer to its question: an
    /// option's number picks it, and Esc or Ctrl+C turns it down.
    fn answer_key(&mut self, key: &Key, actions: &mut Vec<Action>) {
        let Some(question) = &self.question else {
            return;
        };
        let option = match key {
            Key::Text(text) => {
                let picked = text
                    .chars()
                    .next()
                    .and_then(|ch| ch.to_digit(10))
                    .and_then(|digit| usize::try_from(digit).ok())
                    .and_then(|digit| digit.checked_sub(1))
                    .and_then(|index| question.options.get(index));
                let Some(option) = picked else {
                    self.notice = Some("pick an option by its number, or Esc to say no");
                    return;
                };
                Some(option.option_id.0.to_string())
            }
            Key::Escape | Key::Interrupt => question
                .options
                .iter()
                .find(|option| option.kind == PermissionOptionKind::RejectOnce)
                .map(|option| option.option_id.0.to_string()),
            _ => return,
        };
        let question = self.question.take().expect("checked above");
        let name = option.as_deref().and_then(|picked| {
            question
                .options
                .iter()
                .find(|option| *option.option_id.0 == *picked)
                .map(|option| option.name.clone())
        });
        let answer = format!(
            "  ? {}: {}",
            question.title,
            name.as_deref().unwrap_or("no")
        );
        self.commit_line(&answer, DIM);
        actions.push(Action::Answer {
            request: question.request,
            option,
        });
    }

    // --- What the agent says ----------------------------------------------

    /// The session asked for was made, or opened again. Returns the
    /// message waiting for it, to send.
    pub(crate) fn connected(&mut self, session: String) -> Option<String> {
        self.link = Link::Open;
        self.failure = None;
        // Opening one again answers with no ID: it's the one asked for.
        if !session.is_empty() {
            self.session = Some(session);
        }
        let queued = self.queued.take();
        self.redraw();
        queued
    }

    /// The session couldn't be made or opened.
    pub(crate) fn failed_to_connect(&mut self, reason: &str) {
        self.link = Link::Closed;
        let note = format!("the agent couldn't start: {reason}");
        if self.queued.take().is_some() {
            self.end_turn(Some((&note, FAILED)), 1);
        } else {
            self.commit_line(&note, FAILED);
        }
        self.failure = Some(reason.to_owned());
        self.redraw();
    }

    /// The agent has gone: it exited, or stopped answering.
    pub(crate) fn disconnected(&mut self) {
        let was_opening = self.link == Link::Opening;
        self.link = Link::Closed;
        self.queued = None;
        if self.answering {
            self.end_turn(Some(("the agent stopped", FAILED)), 1);
        } else if was_opening {
            self.commit_line("the agent stopped before it started", FAILED);
            self.failure = Some("it stopped".to_owned());
        } else if self.session.is_some() {
            self.commit_line("the agent stopped; the next message starts it again", DIM);
        }
        self.redraw();
    }

    /// The agent finished answering, or failed to.
    pub(crate) fn answered(&mut self, result: Result<StopReason, String>) {
        let note = match result {
            Ok(StopReason::Cancelled) => Some(("stopped".to_owned(), DIM)),
            Ok(StopReason::MaxTokens) => {
                Some(("the answer hit its length limit".to_owned(), FAILED))
            }
            Ok(StopReason::MaxTurnRequests) => {
                Some(("the answer took too many steps".to_owned(), FAILED))
            }
            Ok(StopReason::Refusal) => Some(("the agent refused".to_owned(), FAILED)),
            // Done, or a reason newer than this build knows.
            Ok(_) => None,
            Err(err) => Some((format!("the agent failed: {err}"), FAILED)),
        };
        // Only a finished answer is a success, as a command's exit 0 is.
        let status = i32::from(note.is_some());
        self.end_turn(
            note.as_ref().map(|(text, style)| (text.as_str(), *style)),
            status,
        );
        self.redraw();
    }

    /// The agent asks whether it may use a tool.
    pub(crate) fn ask(
        &mut self,
        request: Value,
        tool: &ToolCallUpdate,
        options: Vec<PermissionOption>,
    ) {
        let title = tool
            .fields
            .title
            .clone()
            .or_else(|| {
                self.tool(&tool.tool_call_id.0)
                    .map(|tool| tool.title.clone())
            })
            .unwrap_or_else(|| "use a tool".to_owned());
        self.question = Some(Question {
            request,
            title: clean(&title),
            options,
        });
        self.redraw();
    }

    /// Takes a change to the session.
    pub(crate) fn update(&mut self, update: SessionUpdate) {
        match update {
            SessionUpdate::AgentMessageChunk(chunk) => self.write_words(&chunk.content, false),
            SessionUpdate::AgentThoughtChunk(chunk) => self.write_words(&chunk.content, true),
            SessionUpdate::ToolCall(call) => {
                self.end_partial();
                let id = call.tool_call_id.0.to_string();
                let tool = Tool {
                    id,
                    title: clean(&call.title),
                    status: call.status,
                };
                self.tools.retain(|old| old.id != tool.id);
                self.settle_or_keep(tool);
            }
            SessionUpdate::ToolCallUpdate(update) => {
                let id = update.tool_call_id.0.to_string();
                let index = self.tools.iter().position(|tool| tool.id == id);
                let mut tool = match index {
                    Some(index) => self.tools.remove(index),
                    None => Tool {
                        id,
                        title: "a tool".to_owned(),
                        status: ToolCallStatus::Pending,
                    },
                };
                if let Some(title) = &update.fields.title {
                    tool.title = clean(title);
                }
                if let Some(status) = update.fields.status {
                    tool.status = status;
                }
                if matches!(
                    tool.status,
                    ToolCallStatus::Completed | ToolCallStatus::Failed
                ) {
                    self.end_partial();
                }
                // Back where it was, unless it's finished.
                match index {
                    Some(index)
                        if !matches!(
                            tool.status,
                            ToolCallStatus::Completed | ToolCallStatus::Failed
                        ) =>
                    {
                        self.tools.insert(index, tool);
                    }
                    _ => self.settle_or_keep(tool),
                }
            }
            SessionUpdate::Plan(plan) => {
                self.plan = plan
                    .entries
                    .into_iter()
                    .map(|entry| (clean(&entry.content), entry.status))
                    .collect();
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                if let MaybeUndefined::Value(title) = info.title {
                    let _ = write!(self.pending, "\x1b]2;{}\x07", clean(&title));
                }
            }
            // Its usage, modes, settings and commands, which the chat
            // doesn't show yet (CLACKA-25), and the user's own words,
            // already written.
            _ => return,
        }
        self.redraw();
    }

    fn tool(&self, id: &str) -> Option<&Tool> {
        self.tools.iter().find(|tool| tool.id == id)
    }

    /// Writes down a finished tool call above the live end, or keeps one
    /// still running in it.
    fn settle_or_keep(&mut self, tool: Tool) {
        let (mark, style) = match tool.status {
            ToolCallStatus::Completed => ("✓", DONE),
            ToolCallStatus::Failed => ("✗", FAILED),
            _ => {
                self.tools.push(tool);
                return;
            }
        };
        self.answer_starts();
        let _ = write!(
            self.pending,
            "  \x1b[{style}m{mark}\x1b[0m \x1b[{DIM}m{}\x1b[0m\r\n",
            tool.title
        );
    }

    /// A piece of the agent's message, or of its thinking.
    fn write_words(&mut self, content: &ContentBlock, thinking: bool) {
        let ContentBlock::Text(text) = content else {
            return;
        };
        if thinking != self.words.thinking {
            self.end_partial();
            self.words.thinking = thinking;
        }
        self.words.partial.push_str(&text.text);
        while let Some(end) = self.words.partial.find('\n') {
            let line: String = self.words.partial.drain(..=end).collect();
            self.commit_words(line.trim_end_matches(['\n', '\r']));
        }
    }

    /// Writes down the line of words being written, though it hasn't
    /// ended, as something else comes after it.
    fn end_partial(&mut self) {
        if !self.words.partial.is_empty() {
            let line = std::mem::take(&mut self.words.partial);
            self.commit_words(&line);
        }
    }

    /// Writes down one line of the agent's words, styled as Markdown
    /// would show it.
    fn commit_words(&mut self, line: &str) {
        let style = self.words_style(line);
        if line.trim_start().starts_with("```") && !self.words.thinking {
            self.words.code = !self.words.code;
        }
        self.commit_line(line, style);
    }

    /// How a line of the agent's words is shown.
    fn words_style(&self, line: &str) -> &'static str {
        if self.words.thinking {
            THOUGHT
        } else if line.trim_start().starts_with("```") {
            DIM
        } else if self.words.code {
            CODE
        } else if line.starts_with('#') {
            BOLD
        } else {
            PLAIN
        }
    }

    // --- Exchanges ----------------------------------------------------------

    /// Writes down the user's message, starting an exchange.
    fn begin_turn(&mut self, text: &str) {
        self.answering = true;
        self.words.code = false;
        self.words.thinking = false;
        // Where what's written down ends: the live end starts there.
        let end = self
            .live
            .unwrap_or_else(|| self.emulator.primary().scrollback_end() + self.cursor_line());
        if end > 0 {
            self.pending.push_str("\r\n");
        }
        self.pending.push_str("\x1b]133;A\x07");
        for (index, line) in text.lines().enumerate() {
            let lead = if index == 0 { "›" } else { " " };
            let _ = write!(
                self.pending,
                "\x1b[{USER}m{lead}\x1b[0m {}\r\n",
                clean(line)
            );
        }
        self.pending.push_str("\r\n");
        self.unanswered = Some(text.to_owned());
    }

    /// Ends the exchange: what's left of the answer is written down, with
    /// `note`, and the block gets `status`, as a command's exit status.
    fn end_turn(&mut self, note: Option<(&str, &str)>, status: i32) {
        self.end_partial();
        for tool in std::mem::take(&mut self.tools) {
            self.answer_starts();
            let _ = write!(self.pending, "  \x1b[{DIM}m○ {}\x1b[0m\r\n", tool.title);
        }
        if let Some((note, style)) = note {
            self.commit_line(note, style);
        }
        self.plan.clear();
        self.question = None;
        self.answering = false;
        self.unanswered = None;
        let _ = write!(self.pending, "\x1b]133;D;{status}\x07");
    }

    /// Marks the row this exchange's answer starts on, if it hasn't
    /// started yet. The row being written down, not the live end, which is
    /// erased with its marks.
    fn answer_starts(&mut self) {
        if let Some(command) = self.unanswered.take() {
            let _ = write!(
                self.pending,
                "\x1b]133;C;cmdline_url={}\x07",
                percent_encode(&command)
            );
        }
    }

    fn commit_line(&mut self, line: &str, style: &str) {
        self.answer_starts();
        let _ = write!(self.pending, "\x1b[{style}m{}\x1b[0m\r\n", clean(line));
    }

    // --- Drawing ------------------------------------------------------------

    /// Takes the live end away, leaving the cursor where it started.
    fn erase_live(&mut self) {
        if let Some(start) = self.live.take() {
            let top = self.emulator.primary().scrollback_end();
            let line = start.saturating_sub(top);
            self.emulator
                .advance(format!("\x1b[{};1H\x1b[J", line + 1).as_bytes());
        }
    }

    /// Writes down what's pending, and draws the live end below it.
    fn redraw(&mut self) {
        self.erase_live();
        self.emulator.advance(self.pending.as_bytes());
        self.pending.clear();

        let size = self.emulator.size();
        let (width, height) = (usize::from(size.cols()), usize::from(size.rows()));
        let start = self.emulator.primary().scrollback_end() + self.cursor_line();
        self.live = Some(start);

        let mut lines = self.live_lines();
        // The live end fits on the screen, or it couldn't be erased: what
        // doesn't fit goes, from the top, but the message being typed.
        let rows = |line: &(String, &str)| {
            UnicodeWidthStr::width(line.0.as_str())
                .div_ceil(width)
                .max(1)
        };
        let input_rows: usize = self
            .editor
            .lines()
            .map(|line| (line.width() + 2).div_ceil(width).max(1))
            .sum();
        let mut total: usize = lines.iter().map(rows).sum::<usize>() + input_rows;
        while total >= height && !lines.is_empty() {
            total -= rows(&lines.remove(0));
        }

        let mut out = String::new();
        for (text, style) in &lines {
            let _ = write!(out, "\x1b[{style}m{text}\x1b[0m\r\n");
        }
        // The message being typed, with the cursor at its caret.
        let caret = self.editor.cursor;
        let (before, after) = self.editor.text.split_at(caret);
        let prompt_style = if self.answering || self.link == Link::Opening {
            DIM
        } else {
            USER
        };
        let _ = write!(out, "\x1b[{prompt_style}m›\x1b[0m ");
        push_input(&mut out, before);
        self.emulator.advance(out.as_bytes());
        let caret_row = self.emulator.primary().scrollback_end() + self.cursor_line();
        let caret_col = self.emulator.cursor().col;
        out.clear();
        push_input(&mut out, after);
        self.emulator.advance(out.as_bytes());
        let top = self.emulator.primary().scrollback_end();
        let row = caret_row.saturating_sub(top);
        self.emulator
            .advance(format!("\x1b[{};{}H", row + 1, caret_col + 1).as_bytes());
    }

    fn cursor_line(&self) -> u64 {
        u64::try_from(self.emulator.cursor().line).expect("screen lines fit in a u64")
    }

    /// The lines of the live end above the message being typed, each with
    /// its style.
    fn live_lines(&self) -> Vec<(String, &'static str)> {
        let mut lines = Vec::new();
        if !self.words.partial.is_empty() {
            lines.push((
                clean(&self.words.partial),
                self.words_style(&self.words.partial),
            ));
        }
        for tool in &self.tools {
            lines.push((format!("  ○ {}", tool.title), DIM));
        }
        if self.answering {
            for (entry, status) in &self.plan {
                let mark = match status {
                    PlanEntryStatus::Completed => "☑",
                    PlanEntryStatus::InProgress => "◐",
                    _ => "☐",
                };
                lines.push((format!("  {mark} {entry}"), DIM));
            }
        }
        if let Some(question) = &self.question {
            lines.push((format!("  ? May the agent {}?", question.title), ASKING));
            let options: Vec<String> = (question.options.iter().enumerate())
                .map(|(index, option)| format!("{} {}", index + 1, clean(&option.name)))
                .collect();
            lines.push((format!("    {}  ·  Esc no", options.join("  ·  ")), BOLD));
        }
        let status = if let Some(notice) = self.notice {
            Some(notice.to_owned())
        } else if self.question.is_some() {
            None
        } else if self.link == Link::Opening {
            Some("starting the agent…".to_owned())
        } else if self.answering {
            Some("answering · Ctrl+C stops it".to_owned())
        } else if self.failure.is_some() {
            Some("send a message to try again".to_owned())
        } else {
            None
        };
        if let Some(status) = status {
            lines.push((format!("  {status}"), DIM));
        }
        lines
    }
}

/// Writes the message being typed: its first line after the prompt, the
/// rest each under it.
fn push_input(out: &mut String, text: &str) {
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push_str("\r\n  ");
        }
        out.push_str(&clean(line));
    }
}

/// `text` without anything that would control the terminal rather than
/// show, so an agent can't move the cursor, retitle the window or ring the
/// bell by what it writes. Tabs become spaces.
fn clean(text: &str) -> String {
    text.chars()
        .filter_map(|ch| match ch {
            '\t' => Some(' '),
            ch if ch.is_control() => None,
            ch => Some(ch),
        })
        .collect()
}

/// `text` percent-encoded, as OSC 133's `cmdline_url` takes it.
fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// The message being typed.
#[derive(Debug, Default)]
struct Editor {
    text: String,
    /// Where the caret is, in bytes.
    cursor: usize,
}

impl Editor {
    fn lines(&self) -> impl Iterator<Item = &str> {
        self.text.split('\n')
    }

    fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    fn previous(&self) -> Option<usize> {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
    }

    fn next(&self) -> Option<usize> {
        self.text[self.cursor..]
            .chars()
            .next()
            .map(|ch| self.cursor + ch.len_utf8())
    }

    fn backspace(&mut self) {
        if let Some(previous) = self.previous() {
            self.text.replace_range(previous..self.cursor, "");
            self.cursor = previous;
        }
    }

    fn delete(&mut self) {
        if let Some(next) = self.next() {
            self.text.replace_range(self.cursor..next, "");
        }
    }

    fn left(&mut self) {
        if let Some(previous) = self.previous() {
            self.cursor = previous;
        }
    }

    fn right(&mut self) {
        if let Some(next) = self.next() {
            self.cursor = next;
        }
    }

    /// Deletes the word before the caret, and the spaces after it.
    fn delete_word(&mut self) {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end();
        let start = trimmed
            .char_indices()
            .rev()
            .find(|(_, ch)| ch.is_whitespace())
            .map_or(0, |(index, ch)| index + ch.len_utf8());
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Deletes everything before the caret.
    fn clear_before(&mut self) {
        self.text.replace_range(..self.cursor, "");
        self.cursor = 0;
    }
}

/// A key typed into a chat.
#[derive(Debug, PartialEq, Eq)]
enum Key {
    /// Text to type: keys, or a paste.
    Text(String),
    /// Enter, which sends the message.
    Enter,
    /// A new line in the message: Alt+Enter or Ctrl+J.
    Newline,
    Backspace,
    /// Ctrl+W, or Ctrl+Backspace.
    DeleteWord,
    Delete,
    Left,
    Right,
    Home,
    End,
    /// Ctrl+C.
    Interrupt,
    /// Ctrl+U.
    ClearLine,
    Escape,
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// The keys in `bytes`, encoded as a terminal sends them to its program.
/// `pasting` says whether a paste is under way, across calls.
fn keys(mut bytes: &[u8], pasting: &mut bool) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut text = Vec::new();
    let flush = |text: &mut Vec<u8>, keys: &mut Vec<Key>| {
        if !text.is_empty() {
            keys.push(Key::Text(String::from_utf8_lossy(text).into_owned()));
            text.clear();
        }
    };
    while !bytes.is_empty() {
        if *pasting {
            let end = find(bytes, PASTE_END);
            text.extend_from_slice(&bytes[..end.unwrap_or(bytes.len())]);
            match end {
                Some(end) => {
                    *pasting = false;
                    bytes = &bytes[end + PASTE_END.len()..];
                }
                None => bytes = &[],
            }
            continue;
        }
        if let Some(rest) = bytes.strip_prefix(PASTE_START) {
            *pasting = true;
            bytes = rest;
            continue;
        }
        let (key, len) = match bytes {
            [b'\r', ..] => (Some(Key::Enter), 1),
            [b'\n', ..] => (Some(Key::Newline), 1),
            [0x7f, ..] => (Some(Key::Backspace), 1),
            [0x08 | 0x17, ..] => (Some(Key::DeleteWord), 1),
            [0x03, ..] => (Some(Key::Interrupt), 1),
            [0x15, ..] => (Some(Key::ClearLine), 1),
            [0x01, ..] => (Some(Key::Home), 1),
            [0x05, ..] => (Some(Key::End), 1),
            [0x1b, b'\r', ..] => (Some(Key::Newline), 2),
            [0x1b, b'[' | b'O', rest @ ..] => {
                // A control sequence: parameters, then its final byte.
                let end = rest.iter().position(|byte| (0x40..=0x7e).contains(byte));
                match end {
                    Some(end) => {
                        let key = match (&rest[..end], rest[end]) {
                            (b"" | b"1", b'D') => Some(Key::Left),
                            (b"" | b"1", b'C') => Some(Key::Right),
                            (b"" | b"1", b'H') | (b"1" | b"7", b'~') => Some(Key::Home),
                            (b"" | b"1", b'F') | (b"4" | b"8", b'~') => Some(Key::End),
                            (b"3", b'~') => Some(Key::Delete),
                            _ => None,
                        };
                        (key, end + 3)
                    }
                    None => (None, bytes.len()),
                }
            }
            [0x1b, ..] => (Some(Key::Escape), 1),
            [byte, ..] if *byte < 0x20 => (None, 1),
            [byte, ..] => {
                text.push(*byte);
                bytes = &bytes[1..];
                let _ = byte;
                continue;
            }
            [] => unreachable!("bytes isn't empty"),
        };
        flush(&mut text, &mut keys);
        keys.extend(key);
        bytes = &bytes[len..];
    }
    flush(&mut text, &mut keys);
    keys
}

/// Where `needle` first is in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use agent_client_protocol_schema::v1::SessionNotification;
    use clackatoo_term::Which;

    use super::*;

    fn chat() -> HostedChat {
        let config = DaemonConfig::from_env();
        HostedChat::new(
            TerminalId(1),
            Size::new(40, 12).unwrap(),
            "/tmp".to_owned(),
            &config,
        )
    }

    fn update(chat: &mut HostedChat, json: &str) {
        let json = format!(r#"{{"sessionId":"s","update":{json}}}"#);
        let notification: SessionNotification = serde_json::from_str(&json).unwrap();
        chat.update(notification.update);
    }

    fn screen(chat: &HostedChat) -> String {
        chat.emulator.screen_text()
    }

    #[test]
    fn keys_are_decoded_as_a_terminal_sends_them() {
        let mut pasting = false;
        assert_eq!(
            keys(b"hi\x1b[D\x7f\r\x1b\r\x03\x1b[3~\x1b", &mut pasting),
            [
                Key::Text("hi".into()),
                Key::Left,
                Key::Backspace,
                Key::Enter,
                Key::Newline,
                Key::Interrupt,
                Key::Delete,
                Key::Escape,
            ]
        );
        assert_eq!(
            keys("a\x1b[200~x\ry\x1b[201~é".as_bytes(), &mut pasting),
            [Key::Text("ax\ryé".into())]
        );
        // A paste split between two inputs.
        assert_eq!(
            keys(b"\x1b[200~one\r", &mut pasting),
            [Key::Text("one\r".into())]
        );
        assert!(pasting);
        assert_eq!(
            keys(b"two\x1b[201~\r", &mut pasting),
            [Key::Text("two".into()), Key::Enter]
        );
    }

    #[test]
    fn the_editor_moves_and_deletes_by_character_and_word() {
        let mut editor = Editor::default();
        editor.insert("héllo wörld");
        editor.left();
        editor.backspace();
        assert_eq!((editor.text.as_str(), editor.cursor), ("héllo wörd", 11));
        editor.delete_word();
        assert_eq!(editor.text, "héllo d");
        editor.insert("a\r\nb");
        assert_eq!(editor.text, "héllo a\nbd");
    }

    #[test]
    fn a_message_waits_for_the_session_then_goes() {
        let mut chat = chat();
        assert!(screen(&chat).contains("starting the agent"));
        // Typed before the session is made: it waits.
        assert_eq!(chat.input(b"hello\r"), []);
        assert!(screen(&chat).contains("› hello"));
        assert_eq!(chat.connected("s1".into()).as_deref(), Some("hello"));
        assert_eq!(chat.session(), Some("s1"));
        // Typed once there's a session: it goes at once.
        chat.answered(Ok(StopReason::EndTurn));
        assert_eq!(chat.input(b"again\r"), [Action::Prompt("again".into())]);
    }

    #[test]
    fn ctrl_c_takes_back_a_message_still_waiting_for_its_session() {
        let mut chat = chat();
        chat.input(b"hello\r");
        assert_eq!(chat.input(b"\x03"), []);
        assert!(screen(&chat).contains("stopped"));
        assert_eq!(chat.connected("s".into()), None);
    }

    #[test]
    fn the_answer_streams_in_and_lines_are_written_down_as_they_end() {
        let mut chat = chat();
        chat.connected("s".into());
        chat.input(b"hi\r");
        update(
            &mut chat,
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Hel"}}"#,
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"lo\nwor"}}"#,
        );
        let text = screen(&chat);
        assert!(text.contains("Hello\nwor"), "{text}");
        assert!(text.contains("answering"));
        chat.answered(Ok(StopReason::EndTurn));
        let text = screen(&chat);
        assert!(text.contains("Hello\nwor\n"), "{text}");
        assert!(!text.contains("answering"));
        // The exchange is a block: the message is its command.
        let block = chat.emulator.block(0, Which::At).expect("a block");
        assert_eq!(block.command.as_deref(), Some("hi"));
    }

    #[test]
    fn tool_calls_run_in_the_live_end_and_are_written_down_when_done() {
        let mut chat = chat();
        chat.connected("s".into());
        chat.input(b"go\r");
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call","toolCallId":"t1","title":"Read a.rs","status":"in_progress"}"#,
        );
        assert!(screen(&chat).contains("○ Read a.rs"));
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"}"#,
        );
        let text = screen(&chat);
        assert!(
            text.contains("✓ Read a.rs") && !text.contains("○"),
            "{text}"
        );
    }

    #[test]
    fn the_agent_asks_and_a_number_answers() {
        let mut chat = chat();
        chat.connected("s".into());
        chat.input(b"go\r");
        let options: Vec<PermissionOption> = serde_json::from_str(
            r#"[{"optionId":"yes","name":"Allow","kind":"allow_once"},
                {"optionId":"no","name":"Reject","kind":"reject_once"}]"#,
        )
        .unwrap();
        let tool: ToolCallUpdate =
            serde_json::from_str(r#"{"toolCallId":"t","title":"run ls"}"#).unwrap();
        chat.ask(Value::from(7), &tool, options);
        assert!(screen(&chat).contains("May the agent run ls?"));
        assert_eq!(chat.input(b"x"), []);
        assert_eq!(
            chat.input(b"1"),
            [Action::Answer {
                request: Value::from(7),
                option: Some("yes".into())
            }]
        );
        assert!(screen(&chat).contains("? run ls: Allow"));
    }

    #[test]
    fn what_the_agent_writes_cant_control_the_terminal() {
        let mut chat = chat();
        chat.connected("s".into());
        chat.input(b"hi\r");
        update(
            &mut chat,
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"a\u001b]2;owned\u0007b\n"}}"#,
        );
        assert_eq!(chat.emulator.title(), "agent");
        assert!(screen(&chat).contains("a]2;ownedb"));
    }

    #[test]
    fn a_resize_redraws_the_live_end_at_the_new_width() {
        let mut chat = chat();
        chat.connected("s".into());
        chat.input(b"some words typed");
        chat.resize(Size::new(10, 12).unwrap());
        assert_eq!(screen(&chat).trim_end(), "› some wor\nds typed");
        chat.resize(Size::new(40, 12).unwrap());
        assert_eq!(screen(&chat).trim_end(), "› some words typed");
    }

    #[test]
    fn the_agent_stopping_while_idle_is_said() {
        let mut chat = chat();
        chat.connected("s".into());
        chat.disconnected();
        assert!(screen(&chat).contains("the agent stopped"));
        assert_eq!(chat.input(b"hi\r"), [Action::Connect]);
    }

    #[test]
    fn a_chat_handed_over_keeps_its_conversation_and_what_was_typed() {
        let mut chat = chat();
        chat.connected("s".into());
        chat.input(b"hi\r");
        update(
            &mut chat,
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello\n"}}"#,
        );
        chat.answered(Ok(StopReason::EndTurn));
        chat.input(b"half typ");
        let before = screen(&chat);

        let config = DaemonConfig::from_env();
        let mut chat = HostedChat::resume(&chat.hand_over(), &config).unwrap();
        assert_eq!(screen(&chat), before);
        assert_eq!(chat.session(), Some("s"));
        // Not connected: the next message opens the session again.
        assert_eq!(chat.input(b"ed\r"), [Action::Connect]);
        assert_eq!(chat.connected(String::new()).as_deref(), Some("half typed"));
        assert_eq!(chat.session(), Some("s"));
    }

    #[test]
    fn an_answer_an_upgrade_cut_off_says_so() {
        let mut chat = chat();
        chat.connected("s".into());
        chat.input(b"hi\r");
        let config = DaemonConfig::from_env();
        let chat = HostedChat::resume(&chat.hand_over(), &config).unwrap();
        assert!(screen(&chat).contains("cut off by an upgrade"));
        assert!(!screen(&chat).contains("answering"));
    }
}
