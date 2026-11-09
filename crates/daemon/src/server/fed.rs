//! Panes showing what an agent's command printed (CLACKA-24): terminals
//! no program runs in, which the daemon writes what the agent says a
//! command printed into (`HostedTerminal::fed`), as it says it.
//!
//! A window asks for one from a command's card ([`PaneTerminal::Output`]),
//! never the agent, and only then is it made: from what the chat kept of
//! the command's output, as it came (`chat/outputs.rs`), then each piece
//! the agent sends after, and a last line saying how the command ended.
//! It's a terminal like any other to everything else: viewers, history,
//! search, yanks, closing and compaction are a terminal's. Input to it is
//! refused, as no program reads it, and a window's typing says so.
//!
//! Through an upgrade it's handed over as a terminal whose program has
//! gone, keeping what it shows: which tool of which chat it shows isn't
//! handed over, nor is what the chat kept, so one still showing a command
//! that runs says nothing more comes, and a pane opened from the card
//! after the upgrade shows the card's text, plain, and what comes after.
//! A restart doesn't keep them.
//!
//! [`PaneTerminal::Output`]: clackatoo_protocol::PaneTerminal::Output

use clackatoo_protocol::{Error, OpenOutput, TerminalId};

use super::Daemon;
use crate::chat::{Feed, HostedChat, NOT_KEPT};
use crate::hosted::HostedTerminal;

/// What a terminal showing a command's output says last once the tool
/// call has left the chat, as after a rewind.
const GONE: &str = "[no longer in the chat]";

/// What one says last once its chat has closed while the command ran.
const CHAT_CLOSED: &str = "[the chat closed]";

/// What one says last as the daemon upgrades while the command runs.
pub(super) const UPGRADED: &str =
    "[clackatoo was upgraded: what the command prints next shows in its card]";

impl Daemon {
    /// Makes a terminal showing what tool `open.tool` of chat `open.chat`
    /// printed, so far and as it goes on, `open.size`, for a new pane.
    pub(super) fn new_output(&mut self, open: &OpenOutput) -> Result<TerminalId, Error> {
        let id = TerminalId(self.next_id);
        let chat = self.chat(open.chat).ok_or(Error::NotAChat(open.chat))?;
        let shown = chat.show_output(&open.tool, id).map_err(Error::Other)?;
        let mut terminal = HostedTerminal::fed(id, open.size, &shown.title, &self.config);
        if shown.cut {
            terminal.feed(NOT_KEPT);
        }
        terminal.feed(&shown.text);
        if let Some(Feed::End { exit, failed }) = shown.end {
            terminal.end(exit, failed);
        }
        self.next_id += 1;
        // It has nothing to watch, so this can't fail.
        if let Err(err) = self.add_terminal(terminal) {
            if let Some(chat) = self.chat(open.chat) {
                chat.forget_output(id);
            }
            return Err(Error::Other(format!("the output couldn't be shown: {err}")));
        }
        Ok(id)
    }

    /// Tells the terminals showing what chat `chat`'s commands printed
    /// what's new: more of it, or how a command ended. One that has
    /// closed is forgotten.
    pub(super) fn feed_outputs(&mut self, chat: TerminalId) {
        let Some(told) = self.chat(chat).map(HostedChat::take_fed) else {
            return;
        };
        for (terminal, feed) in told {
            let fed = (self.terminals.get_mut(&terminal))
                .and_then(|slot| slot.terminal.pty())
                .filter(|fed| fed.is_fed());
            let Some(fed) = fed else {
                if let Some(chat) = self.chat(chat) {
                    chat.forget_output(terminal);
                }
                continue;
            };
            match feed {
                Feed::Text(text) => fed.feed(&text),
                Feed::End { exit, failed } => fed.end(exit, failed),
                Feed::Gone => fed.stop_feeding(GONE),
            }
            self.schedule_compaction(terminal);
            self.collect_damage(terminal);
        }
    }

    /// Says in each of `terminals`, which showed what a command of a chat
    /// that has closed printed, that nothing more comes.
    pub(super) fn outputs_closed(&mut self, terminals: Vec<TerminalId>) {
        for terminal in terminals {
            let Some(fed) =
                (self.terminals.get_mut(&terminal)).and_then(|slot| slot.terminal.pty())
            else {
                continue;
            };
            fed.stop_feeding(CHAT_CLOSED);
            self.collect_damage(terminal);
        }
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{OpenOutput, Size, TerminalId, TerminalStatus};
    use serde_json::json;

    use super::super::agent_files::tests::daemon_with_chat;
    use super::super::{Daemon, Hosted};

    /// Has chat `chat`'s agent say `update`, as it would over ACP, and the
    /// daemon take it in.
    fn said(daemon: &mut Daemon, chat: TerminalId, update: serde_json::Value) {
        let update = serde_json::from_value(update).unwrap();
        daemon.chat(chat).unwrap().update(update);
        daemon.collect_damage(chat);
    }

    /// Has the chat's agent print `data` as tool `e1`'s output.
    fn printed(daemon: &mut Daemon, chat: TerminalId, data: &str) {
        said(
            daemon,
            chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "e1",
                    "_meta": { "terminal_output_delta": { "data": data, "terminal_id": "e1" } } }),
        );
    }

    fn screen(daemon: &Daemon, terminal: TerminalId) -> String {
        let slot = &daemon.terminals[&terminal];
        slot.terminal.grid().unwrap().screen_text()
    }

    fn open(chat: TerminalId) -> OpenOutput {
        OpenOutput {
            chat,
            tool: "e1".into(),
            size: Size::new(40, 10).unwrap(),
        }
    }

    #[test]
    fn a_commands_output_shows_as_it_came_then_how_it_ended() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        daemon.next_id = chat.0 + 1;
        // A command, as Codex's adapter runs one.
        said(
            &mut daemon,
            chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "e1", "kind": "execute",
                    "title": "ls --color", "status": "in_progress",
                    "content": [{ "type": "terminal", "terminalId": "e1" }],
                    "_meta": { "terminal_info": { "cwd": "/", "terminal_id": "e1" } } }),
        );
        printed(&mut daemon, chat, "\x1b[31mred\x1b[0m\nnot");
        let fed = daemon.new_output(&open(chat)).unwrap();
        let slot = &daemon.terminals[&fed];
        let Hosted::Pty(terminal) = &slot.terminal else {
            panic!("a terminal");
        };
        assert!(terminal.is_fed());
        assert_eq!(terminal.info().pid, None);
        assert_eq!(terminal.emulator().title(), "ls --color");
        // A line break goes to the next line's start, and the colour's
        // a colour, not text.
        assert!(
            screen(&daemon, fed).starts_with("red\nnot"),
            "{}",
            screen(&daemon, fed)
        );
        let row = terminal.emulator().grid().row(0);
        assert_eq!(
            row.style(row.cells()[0]).fg,
            clackatoo_protocol::Color::Indexed(1)
        );

        // What comes after is shown as it comes.
        printed(&mut daemon, chat, "es.txt\n");
        assert!(screen(&daemon, fed).starts_with("red\nnotes.txt"));
        said(
            &mut daemon,
            chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "e1",
                    "status": "failed",
                    "_meta": { "terminal_exit": { "exit_code": 2, "terminal_id": "e1" } } }),
        );
        assert!(screen(&daemon, fed).contains("notes.txt\n[exited 2]"));
        let slot = daemon.terminals.get_mut(&fed).unwrap();
        assert_eq!(slot.terminal.status(), TerminalStatus::Exited(2));
        // No program reads it.
        let terminal = slot.terminal.pty().unwrap();
        assert!(terminal.send_input(b"x").is_err());
        // Nothing more is told it.
        assert!(daemon.chat(chat).unwrap().output_terminals().is_empty());

        // One opened after it ended shows it all at once.
        let again = daemon.new_output(&open(chat)).unwrap();
        assert!(screen(&daemon, again).contains("notes.txt\n[exited 2]"));
    }

    #[test]
    fn only_a_command_has_output_to_show() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        super::super::agent_files::tests::start_tool(&mut daemon, chat, "e1");
        let refused = daemon.new_output(&open(chat)).unwrap_err();
        assert!(refused.to_string().contains("runs no command"), "{refused}");
        let open = OpenOutput {
            tool: "gone".into(),
            ..open(chat)
        };
        assert!(daemon.new_output(&open).is_err());
        assert_eq!(daemon.terminals.len(), 1, "only the chat");
    }

    #[test]
    fn a_running_commands_pane_says_when_its_chat_closes() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        daemon.next_id = chat.0 + 1;
        said(
            &mut daemon,
            chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "e1", "kind": "execute",
                    "title": "sleep 60", "status": "in_progress" }),
        );
        let fed = daemon.new_output(&open(chat)).unwrap();
        daemon.close_terminal(chat).unwrap();
        assert!(screen(&daemon, fed).contains("[the chat closed]"));
        // It runs no more: it's listed as ended, and can't be put in
        // another pane as a terminal still running.
        let status = daemon.terminals[&fed].terminal.status();
        assert_eq!(status, crate::hosted::FED_CUT_OFF);
        assert_ne!(status, TerminalStatus::Running);
        assert!(matches!(
            daemon.pane_content(clackatoo_protocol::PaneTerminal::Existing(fed), None),
            Err(clackatoo_protocol::Error::TerminalExited(_))
        ));
    }

    /// Starts command `e1`, `ls`, in the chat, and opens a pane on it.
    fn running(daemon: &mut Daemon, chat: TerminalId) -> TerminalId {
        daemon.next_id = chat.0 + 1;
        said(
            daemon,
            chat,
            json!({ "sessionUpdate": "tool_call", "toolCallId": "e1", "kind": "execute",
                    "title": "ls", "status": "in_progress" }),
        );
        daemon.new_output(&open(chat)).unwrap()
    }

    #[test]
    fn what_a_command_left_set_is_put_back_before_its_last_line() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let fed = running(&mut daemon, chat);
        // A pager on the alternate screen, asking for the mouse and
        // pastes, cut off in the middle of a title.
        printed(&mut daemon, chat, "before\n");
        printed(
            &mut daemon,
            chat,
            "\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[1;2rpaging\x1b]2;half",
        );
        said(
            &mut daemon,
            chat,
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "e1",
                    "status": "completed",
                    "_meta": { "terminal_exit": { "exit_code": 0, "terminal_id": "e1" } } }),
        );
        let emulator = daemon.terminals[&fed].terminal.grid().unwrap();
        assert!(!emulator.is_alternate_screen());
        let modes = emulator.input_modes();
        assert_eq!(
            modes.mouse_tracking,
            clackatoo_term::mouse::MouseTracking::Off
        );
        assert!(!modes.bracketed_paste);
        // The half title didn't swallow the line, which shows on the
        // screen the command started on.
        assert_eq!(screen(&daemon, fed), "before\n[exited 0]");
        assert_eq!(emulator.title(), "ls");
    }

    #[test]
    fn a_commands_output_never_names_its_pane_nor_says_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let fed = running(&mut daemon, chat);
        printed(
            &mut daemon,
            chat,
            "\x1b]0;rm -rf\x07\x1b]2;sudo\x1b\\\x1b]7;file://host/etc\x07\
             \x1b]8;;https://clackatoo.dev\x1b\\docs\x1b]8;;\x1b\\\n",
        );
        let emulator = daemon.terminals[&fed].terminal.grid().unwrap();
        assert_eq!((emulator.title(), emulator.cwd()), ("ls", None));
        // Links are a terminal's as ever.
        let row = emulator.grid().row(0);
        let link = row.style(row.cells()[0]).link;
        assert!(link.is_some_and(|link| emulator.link(link).is_some()));
    }
}
