//! The connections agents' tools are served on, going on through an
//! upgrade with their agents (CLACKA-188, see `chats/live.rs`). Each
//! chat's agent reaches its tools through a relay it started, which
//! connected to the daemon's socket: the relay goes on with the agent, so
//! its connection must too, or the agent goes on with its tools broken.
//! So each socket goes to the next build, with what's half read and half
//! written of it, as the agents' pipes do, and the relay never knows.
//!
//! The calls on them still waiting go too, each by its connection: a
//! wait for a chat or a command goes on, with the time it had left, and
//! a call waiting for the user, or the boards, is read again as it came
//! (see [`mcp::Raw`]). The browser stops as the daemon upgrades, and the
//! windows its native calls wait on go, so those are answered first: the
//! agent calls them again.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use clackatoo_protocol::TerminalId;
use mio::{Interest, Token};
use serde_json::Value;

use super::{Commanded, Daemon, Flood, Source, Typing, Wait, Waiting};
use crate::handover::{Fds, Live, LiveCommanded, LiveConnection, LiveWait, LiveWaiting};
use crate::mcp::{self, Connection, Incoming};

impl Daemon {
    /// Takes in all the parser's thread has parsed of the connections
    /// serving the tools of `chats`, reading nothing more, as reading them
    /// would. Returns the chats one of whose connections still has
    /// messages with the thread, and so can't go on through an upgrade as
    /// it is. One that has finished isn't handed over, and holds nothing
    /// up.
    pub(in crate::server) fn take_in_tools(&mut self, chats: &[TerminalId]) -> HashSet<TerminalId> {
        let mut stuck = HashSet::new();
        for token in self.tool_connections(chats) {
            let Some(Source::Tools(connection)) = self.sources.get_mut(token.0) else {
                continue;
            };
            let chat = connection.chat;
            let mut incoming = Vec::new();
            let ready = connection.take_parsed(&mut incoming);
            let finished = connection.finished;
            for message in incoming {
                match message {
                    Incoming::Call { id, tool, raw } => self.call_tool(token, chat, id, tool, raw),
                    Incoming::Cancelled(id) => self.cancel_call(token, &id),
                }
            }
            self.flush_tools(token);
            if !ready && !finished {
                stuck.insert(chat);
            }
        }
        stuck
    }

    /// The connections serving the tools of `chats`.
    fn tool_connections(&self, chats: &[TerminalId]) -> Vec<Token> {
        (self.sources.iter())
            .filter_map(|(key, source)| match source {
                Source::Tools(connection) if chats.contains(&connection.chat) => Some(Token(key)),
                _ => None,
            })
            .collect()
    }

    /// Answers the calls from the agents of `chats` that can't wait through
    /// an upgrade: those waiting on the browser, which stops, and on a
    /// window, which goes. The agent can call them again.
    pub(in crate::server) fn refuse_waits_that_cant_go_on(&mut self, chats: &[TerminalId]) {
        let (refused, kept) = std::mem::take(&mut self.tools.waiting)
            .into_iter()
            .partition(|waiting| {
                chats.contains(&waiting.chat)
                    && matches!(waiting.what, Wait::Browser { .. } | Wait::Native { .. })
            });
        self.tools.waiting = kept;
        for waiting in refused {
            let why = match waiting.what {
                Wait::Browser { .. } => {
                    "clackatoo's browser stopped as the daemon upgraded: call it again"
                }
                _ => {
                    "the window this waited on went as the daemon upgraded: what it did isn't \
                     known, so look before doing it again"
                }
            };
            self.answer_tool(waiting.token, &waiting.id, Err(why.to_owned()));
        }
    }

    /// Hands the connections serving the tools of `chats` over to the next
    /// build, into `live` and `fds`, unwatched first, with the calls on
    /// them still waiting, and what each terminal an agent ran a command in
    /// waits for.
    pub(in crate::server) fn hand_over_tools(
        &mut self,
        live: &mut Live,
        fds: &mut Fds,
        chats: &[TerminalId],
    ) {
        let mut indices = HashMap::new();
        for token in self.tool_connections(chats) {
            let can = matches!(self.sources.get(token.0),
                Some(Source::Tools(connection)) if connection.can_hand_over());
            if !can {
                continue;
            }
            let Some(Source::Tools(mut connection)) = self.sources.try_remove(token.0) else {
                continue;
            };
            if let Err(err) = self.poll.registry().deregister(&mut connection.stream) {
                log::warn!(
                    "chat {}: unwatching its tools failed: {err}",
                    connection.chat
                );
            }
            self.unread.retain(|&unread| unread != Flood::Tools(token));
            self.tools.parsing.retain(|&parsing| parsing != token);
            log::info!(
                "chat {}: its tools go on through the upgrade",
                connection.chat
            );
            indices.insert(token, u32::try_from(live.tools.len()).unwrap_or(u32::MAX));
            live.tools.push(connection.hand_over(fds));
        }
        let now = Instant::now();
        let left = |due: Instant| millis(due.saturating_duration_since(now));
        let (handed, kept): (Vec<Waiting>, Vec<Waiting>) = std::mem::take(&mut self.tools.waiting)
            .into_iter()
            .partition(|waiting| indices.contains_key(&waiting.token));
        self.tools.waiting = kept;
        for waiting in handed {
            let what = match waiting.what {
                Wait::Permission { call, raw, .. } => LiveWaiting::Permission {
                    call,
                    tool: raw.name,
                    arguments: raw.arguments.to_string(),
                },
                Wait::Finish {
                    terminal,
                    lines,
                    due,
                } => LiveWaiting::Finish {
                    terminal: terminal.0,
                    lines: lines as u64,
                    left: left(due),
                },
                Wait::Turn { chat, due } => LiveWaiting::Turn {
                    chat: chat.0,
                    left: left(due),
                },
                Wait::Chats { chats, first, due } => LiveWaiting::Chats {
                    chats: chats.iter().map(|chat| chat.0).collect(),
                    first,
                    left: left(due),
                },
                Wait::Boards { raw, due, .. } => LiveWaiting::Boards {
                    tool: raw.name,
                    arguments: raw.arguments.to_string(),
                    left: left(due),
                },
                // Answered as the agents to keep were chosen.
                Wait::Browser { .. } | Wait::Native { .. } => continue,
            };
            live.waiting.push(LiveWait {
                connection: indices[&waiting.token],
                id: waiting.id.to_string(),
                chat: waiting.chat.0,
                what,
            });
        }
        live.next_call = self.tools.next_call;
        live.commanded = (self.tools.commanded.iter())
            .map(|(terminal, commanded)| LiveCommanded {
                terminal: terminal.0,
                owner: commanded.owner.map(|owner| owner.0),
                name: commanded.name.clone(),
                typing: (commanded.typing.as_ref())
                    .map(|typing| (typing.command.clone(), left(typing.due), typing.marked)),
                failed: commanded.failed.clone(),
                expect: commanded.expect,
                expect_first: commanded.expect_first,
                renumbered: commanded.renumbered,
            })
            .collect();
    }

    /// Takes back the connections the last build handed over in `live`,
    /// for the chats in `chats`, whose agents went on: their sockets from
    /// `fds`, watched anew, the calls on them still waiting, and what each
    /// terminal an agent ran a command in waits for.
    pub(in crate::server) fn resume_tools(
        &mut self,
        live: &mut Live,
        fds: &mut Fds,
        chats: &HashSet<TerminalId>,
    ) {
        self.tools.next_call = self.tools.next_call.max(live.next_call);
        let now = Instant::now();
        let at = |left: u64| now + Duration::from_millis(left);
        for kept in std::mem::take(&mut live.commanded) {
            let commanded = Commanded {
                owner: kept.owner.map(TerminalId),
                name: kept.name,
                typing: kept.typing.map(|(command, left, marked)| Typing {
                    command,
                    due: at(left),
                    marked,
                    again: None,
                    refused: None,
                }),
                failed: kept.failed,
                expect: kept.expect,
                expect_first: kept.expect_first,
                renumbered: kept.renumbered,
            };
            if self.terminals.contains_key(&TerminalId(kept.terminal)) {
                self.tools
                    .commanded
                    .insert(TerminalId(kept.terminal), commanded);
            }
        }
        let mut tokens = Vec::new();
        for handed in std::mem::take(&mut live.tools) {
            let chat = TerminalId(handed.chat);
            tokens.push(self.resume_connection(handed, fds, chats.contains(&chat)));
        }
        for waiting in std::mem::take(&mut live.waiting) {
            let token = usize::try_from(waiting.connection)
                .ok()
                .and_then(|at| tokens.get(at).copied().flatten());
            let chat = TerminalId(waiting.chat);
            self.resume_wait(token, chat, waiting, at);
        }
        // A question a tool asked whose call doesn't wait any more, as its
        // connection didn't go on, goes.
        for &chat in chats {
            let calls: Vec<u64> = (self.tools.waiting.iter())
                .filter_map(|waiting| match waiting.what {
                    Wait::Permission { call, .. } if waiting.chat == chat => Some(call),
                    _ => None,
                })
                .collect();
            if let Some(hosted) = self.chat(chat) {
                hosted.keep_tool_questions(&calls);
                self.collect_damage(chat);
            }
        }
    }

    /// Takes back a connection the last build handed over, `handed`, its
    /// socket from `fds`, watched anew, if its chat's agent went on,
    /// `kept`. Returns its token, if it's served.
    fn resume_connection(
        &mut self,
        handed: LiveConnection,
        fds: &mut Fds,
        kept: bool,
    ) -> Option<Token> {
        let chat = TerminalId(handed.chat);
        let instructions = self.tools_instructions(chat);
        let connection = Connection::resume(handed, fds, instructions, &self.parser);
        // Dropped, it closes, and the relay ends, as its agent did.
        let mut connection = connection
            .map_err(|err| log::error!("chat {chat}: its tools couldn't be taken over: {err}"))
            .ok()
            .filter(|_| kept)?;
        let entry = self.sources.vacant_entry();
        let token = Token(entry.key());
        if let Err(err) =
            (self.poll.registry()).register(&mut connection.stream, token, Interest::READABLE)
        {
            log::error!("chat {chat}: its tools couldn't be watched: {err}");
            return None;
        }
        entry.insert(Source::Tools(connection));
        log::info!("chat {chat}: its agent's tools went on through the upgrade");
        self.flush_tools(token);
        // What the agent sent meanwhile waits in the socket.
        self.queue_flood(Flood::Tools(token));
        Some(token)
    }

    /// Takes back `waiting`, a call from the chat `chat`'s agent on the
    /// connection now at `token`, if it's served, with its deadlines
    /// measured by `at`. One whose connection isn't served can't be
    /// answered: its question, if it asked one, is taken back. One whose
    /// call can't be read again is refused.
    fn resume_wait(
        &mut self,
        token: Option<Token>,
        chat: TerminalId,
        waiting: LiveWait,
        at: impl Fn(u64) -> Instant,
    ) {
        let asked = match &waiting.what {
            LiveWaiting::Permission { call, .. } => Some(*call),
            _ => None,
        };
        let withdraw = |daemon: &mut Self| {
            if let Some(call) = asked
                && let Some(hosted) = daemon.chat(chat)
            {
                hosted.withdraw(call);
                daemon.collect_damage(chat);
            }
        };
        let Some(token) = token else {
            return withdraw(self);
        };
        let Ok(id) = serde_json::from_str::<Value>(&waiting.id) else {
            return withdraw(self);
        };
        let raw = |tool: String, arguments: &str| mcp::Raw {
            name: tool,
            arguments: serde_json::from_str(arguments).unwrap_or_default(),
        };
        let what = match waiting.what {
            LiveWaiting::Permission {
                call,
                tool,
                arguments,
            } => {
                let raw = raw(tool, &arguments);
                let Some(tool) = raw.read_again() else {
                    withdraw(self);
                    let refused = "this build reads the call otherwise: call it again";
                    self.answer_tool(token, &id, Err(refused.to_owned()));
                    return;
                };
                // This build's rules are the ones that hold now: what they
                // refuse isn't asked about.
                let refused =
                    (self.limits_refused(chat, &tool)).or_else(|| self.talk_refused(chat, &tool));
                if let Some(why) = refused {
                    withdraw(self);
                    self.answer_tool(token, &id, Err(why));
                    return;
                }
                Some(Wait::Permission { call, tool, raw })
            }
            LiveWaiting::Boards {
                tool,
                arguments,
                left,
            } => {
                let raw = raw(tool, &arguments);
                (raw.read_again()).map(|tool| Wait::Boards {
                    tool,
                    due: at(left),
                    raw,
                })
            }
            LiveWaiting::Finish {
                terminal,
                lines,
                left,
            } => Some(Wait::Finish {
                terminal: TerminalId(terminal),
                lines: usize::try_from(lines).unwrap_or(usize::MAX),
                due: at(left),
            }),
            LiveWaiting::Turn { chat, left } => Some(Wait::Turn {
                chat: TerminalId(chat),
                due: at(left),
            }),
            LiveWaiting::Chats { chats, first, left } => Some(Wait::Chats {
                chats: chats.into_iter().map(TerminalId).collect(),
                first,
                due: at(left),
            }),
        };
        let Some(what) = what else {
            withdraw(self);
            let refused = "this build reads the call otherwise: call it again";
            self.answer_tool(token, &id, Err(refused.to_owned()));
            return;
        };
        self.tools.waiting.push(Waiting {
            token,
            id,
            chat,
            what,
        });
    }
}

/// `left` as a whole number of milliseconds, as the handover keeps it.
fn millis(left: Duration) -> u64 {
    u64::try_from(left.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::fd::OwnedFd;

    use super::*;
    use crate::mcp::Tool;
    use crate::server::agent_files::tests::daemon_with_chat;

    /// A call carried waiting for the user is checked against this
    /// build's rules as it's taken back: one they refuse is refused, and
    /// its question taken back, rather than left to be allowed
    /// (CLACKA-188).
    #[test]
    fn a_carried_call_this_build_refuses_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        ours.set_nonblocking(true).unwrap();
        let stream = mio::net::UnixStream::from_std(ours);
        let info = (String::new(), false);
        let connection = Connection::new(stream, chat, Vec::new(), info, &daemon.parser);
        let token = Token(daemon.sources.insert(Source::Tools(connection)));
        if let Some(Source::Tools(connection)) = daemon.sources.get_mut(token.0) {
            (daemon.poll.registry())
                .register(&mut connection.stream, token, Interest::READABLE)
                .unwrap();
        }
        let raw = mcp::Raw {
            name: "run_in_terminal".to_owned(),
            arguments: serde_json::json!({ "command": "make" }),
        };
        let tool = raw.read_again().unwrap();
        let hosted = daemon.chat(chat).unwrap();
        assert!(hosted.ask_for_tool(3, "run make", None, crate::chat::Choices::Tool));
        daemon.tools.waiting.push(Waiting {
            token,
            id: serde_json::json!(5),
            chat,
            what: Wait::Permission { call: 3, tool, raw },
        });
        let mut live = Live::default();
        let mut fds = Fds::default();
        daemon.hand_over_tools(&mut live, &mut fds, &[chat]);
        let received: Vec<OwnedFd> = fds.iter().map(|fd| fd.try_clone().unwrap()).collect();
        drop(fds);
        // The chat may run no commands now, as this build reads it.
        let wanted = crate::chat::Wanted::default();
        let limits = crate::profiles::Limits::NoCommands;
        (daemon.chat(chat).unwrap()).start_as("reader", limits, None, wanted);
        let mut fds = Fds::from_received(received);
        daemon.resume_tools(&mut live, &mut fds, &HashSet::from([chat]));

        assert!(
            daemon.tools.waiting.is_empty(),
            "{:?}",
            daemon.tools.waiting
        );
        assert!(
            daemon.chat(chat).unwrap().node().question.is_none(),
            "still asked"
        );
        let mut said = String::new();
        BufReader::new(&theirs).read_line(&mut said).unwrap();
        let answer: Value = serde_json::from_str(&said).unwrap();
        assert_eq!(answer["id"], 5);
        assert_eq!(answer["result"]["isError"], true, "{answer}");
    }

    /// The calls waiting on a connection handed over wait on, by the
    /// connection as it's watched now, on the same socket, each as it was:
    /// a wait for the user read again as it came, a wait for a chat with
    /// the time it had left (CLACKA-188).
    #[test]
    fn calls_waiting_go_on_by_their_connection_and_as_they_came() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        ours.set_nonblocking(true).unwrap();
        let stream = mio::net::UnixStream::from_std(ours);
        let info = (String::new(), false);
        let connection = Connection::new(stream, chat, Vec::new(), info, &daemon.parser);
        let token = Token(daemon.sources.insert(Source::Tools(connection)));
        if let Some(Source::Tools(connection)) = daemon.sources.get_mut(token.0) {
            (daemon.poll.registry())
                .register(&mut connection.stream, token, Interest::READABLE)
                .unwrap();
        }
        let raw = mcp::Raw {
            name: "list_panes".to_owned(),
            arguments: serde_json::json!({}),
        };
        daemon.tools.waiting.push(Waiting {
            token,
            id: serde_json::json!(5),
            chat,
            what: Wait::Permission {
                call: 3,
                tool: Tool::ListPanes,
                raw,
            },
        });
        let due = Instant::now() + Duration::from_mins(1);
        daemon.tools.waiting.push(Waiting {
            token,
            id: serde_json::json!("six"),
            chat,
            what: Wait::Chats {
                chats: vec![chat],
                first: true,
                due,
            },
        });

        let mut live = Live::default();
        let mut fds = Fds::default();
        daemon.hand_over_tools(&mut live, &mut fds, &[chat]);
        assert!(daemon.tools.waiting.is_empty());
        assert!(daemon.sources.get(token.0).is_none(), "it's left watched");
        assert_eq!(live.tools.len(), 1);
        assert!(live.waiting.iter().all(|waiting| waiting.connection == 0));
        // As the next build receives them: the same files, other fds.
        let received: Vec<OwnedFd> = fds.iter().map(|fd| fd.try_clone().unwrap()).collect();
        drop(fds);
        let mut fds = Fds::from_received(received);
        daemon.resume_tools(&mut live, &mut fds, &HashSet::from([chat]));

        let [asked, waiting] = &daemon.tools.waiting[..] else {
            panic!("{:?}", daemon.tools.waiting);
        };
        assert_eq!(asked.token, waiting.token);
        assert!(matches!(
            daemon.sources.get(asked.token.0),
            Some(Source::Tools(connection)) if connection.chat == chat
        ));
        assert!(matches!(
            &asked.what,
            Wait::Permission {
                call: 3,
                tool: Tool::ListPanes,
                ..
            }
        ));
        assert_eq!(asked.id, serde_json::json!(5));
        let Wait::Chats { due: kept, .. } = waiting.what else {
            panic!("{waiting:?}");
        };
        // The time it had left, measured again from when it was taken back:
        // at most the time the handover took later.
        let off = kept.max(due) - kept.min(due);
        assert!(off < Duration::from_secs(5), "{off:?}");
        // Answered on the same socket.
        daemon.answer_tool(asked.token, &serde_json::json!(5), Ok("done".to_owned()));
        let mut said = String::new();
        BufReader::new(&theirs).read_line(&mut said).unwrap();
        let answer: Value = serde_json::from_str(&said).unwrap();
        assert_eq!(answer["id"], 5);
    }
}
