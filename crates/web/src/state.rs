//! What the app knows, kept current with what the daemon says: the
//! workspaces, the chats as a list, and the chat shown, with what's
//! changed in it since it was last drawn. No browser in here: messages
//! in, messages out, so it's tested on its own.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use clackatoo_protocol::{
    ChatAction, ChatPatch, ChatSummary, ClientMessage, Event, PROTOCOL_VERSION, Patch, Request,
    Response, ServerMessage, TerminalId, View, ViewUpdate, Workspaces,
};

/// How the app's connection to the daemon is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Online {
    /// Connecting, or about to.
    Connecting,
    /// Connected.
    Open,
    /// Lost, trying again in this many seconds.
    Lost(u32),
}

/// What changed since the screen was last drawn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "each part changed or didn't")]
pub struct Dirty {
    /// The list of chats, or the connection.
    pub home: bool,
    /// The shown chat's header: its title, state and usage.
    pub header: bool,
    /// What's under its turns: its question, notice and menus.
    pub footer: bool,
    /// Its turns, by index.
    pub turns: BTreeSet<usize>,
    /// Every turn: the chat is new, or came whole.
    pub all_turns: bool,
}

/// The chat shown.
#[derive(Debug)]
pub struct OpenChat {
    pub id: TerminalId,
    /// What it shows, once the daemon has sent it.
    pub view: Option<View>,
    /// The parts opened or folded by hand, by their `data-fold`.
    pub folds: BTreeMap<String, bool>,
    /// A view update came that hasn't been acknowledged: once it's drawn,
    /// the daemon is told, and sends the next.
    unacknowledged: bool,
}

/// What a request was for, to say what went wrong if it fails.
#[derive(Debug)]
enum Asked {
    /// Nothing to say: its failure is only logged.
    Quiet,
    /// Something the user did, as "sending the message".
    Doing(&'static str),
}

/// What the app knows.
#[derive(Debug)]
pub struct State {
    pub online: Online,
    pub workspaces: Workspaces,
    pub chats: Vec<ChatSummary>,
    pub open: Option<OpenChat>,
    /// Something to tell the user for a while, such as what failed.
    pub note: Option<String>,
    pub dirty: Dirty,
    asked: VecDeque<Asked>,
    outbox: Vec<ClientMessage>,
    /// Whether the user is looking at the app, as the daemon was last told.
    here: bool,
}

/// What came of a message from the daemon.
#[derive(Debug, PartialEq, Eq)]
pub enum Received {
    /// Taken in.
    Fine,
    /// The daemon speaks another protocol, as after an upgrade: the app
    /// is to load again, as the daemon serves it now.
    Reload,
}

impl Default for State {
    fn default() -> Self {
        Self {
            online: Online::Connecting,
            workspaces: Workspaces::default(),
            chats: Vec::new(),
            open: None,
            note: None,
            dirty: Dirty::default(),
            asked: VecDeque::new(),
            outbox: Vec::new(),
            here: true,
        }
    }
}

impl State {
    /// The connection opened: says hello, and asks for what the app shows.
    pub fn connected(&mut self) {
        self.online = Online::Open;
        self.asked.clear();
        self.outbox.clear();
        self.outbox.push(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        });
        self.request(Request::WatchWorkspaces, Asked::Quiet);
        self.request(Request::WatchChats, Asked::Quiet);
        self.outbox.push(ClientMessage::Present { here: self.here });
        if let Some(open) = &mut self.open {
            open.view = None;
            open.unacknowledged = false;
            let terminal = open.id;
            self.request(
                Request::Attach { terminal },
                Asked::Doing("opening the chat"),
            );
        }
        self.dirty.home = true;
    }

    /// The connection was lost, and is tried again in `retry` seconds.
    pub fn lost(&mut self, retry: u32) {
        self.online = Online::Lost(retry);
        self.asked.clear();
        self.outbox.clear();
        self.dirty.home = true;
        self.dirty.header = true;
    }

    /// Takes in a message from the daemon.
    pub fn receive(&mut self, message: ServerMessage) -> Received {
        match message {
            ServerMessage::Hello { protocol_version } if protocol_version != PROTOCOL_VERSION => {
                return Received::Reload;
            }
            ServerMessage::Hello { .. } | ServerMessage::NotUpgrading { .. } => {}
            ServerMessage::Response(response) => self.answered(response),
            ServerMessage::Event(event) => self.event(event),
        }
        Received::Fine
    }

    /// Takes in the answer to the oldest request.
    fn answered(&mut self, response: Response) {
        let asked = self.asked.pop_front();
        match (response, asked) {
            (Response::Workspaces(workspaces), _) => {
                self.workspaces = workspaces;
                self.dirty.home = true;
            }
            (Response::Chats(chats), _) => {
                self.chats = chats;
                self.dirty.home = true;
                self.dirty.header = true;
            }
            (Response::Error(err), Some(Asked::Doing(what))) => {
                self.note = Some(format!("Failed {what}: {err}"));
                self.dirty.home = true;
                self.dirty.footer = true;
            }
            _ => {}
        }
    }

    /// Takes in an event.
    fn event(&mut self, event: Event) {
        match event {
            Event::Workspaces(workspaces) => {
                self.workspaces = workspaces;
                self.dirty.home = true;
            }
            Event::Chats(chats) => {
                self.chats = chats;
                self.dirty.home = true;
                self.dirty.header = true;
            }
            Event::View { terminal, update } => self.view_changed(terminal, update),
            _ => {}
        }
    }

    /// Takes in a change to a chat's view.
    fn view_changed(&mut self, terminal: TerminalId, update: ViewUpdate) {
        let Some(open) = self.open.as_mut().filter(|open| open.id == terminal) else {
            // One left behind, before the detach landed: acknowledged, so
            // the daemon isn't left waiting.
            self.outbox.push(ClientMessage::FrameDone { terminal });
            return;
        };
        open.unacknowledged = true;
        match (&mut open.view, update) {
            (view, ViewUpdate::Snapshot(snapshot)) => {
                *view = Some(snapshot);
                self.dirty.all_turns = true;
                self.dirty.header = true;
                self.dirty.footer = true;
            }
            (Some(view), ViewUpdate::Patch(patches)) => {
                for patch in &patches {
                    mark(&mut self.dirty, patch);
                }
                view.apply(ViewUpdate::Patch(patches));
            }
            (None, ViewUpdate::Patch(_)) => {}
        }
    }

    /// Shows chat `id`.
    pub fn open_chat(&mut self, id: TerminalId) {
        if self.open.as_ref().is_some_and(|open| open.id == id) {
            return;
        }
        self.close_chat();
        self.open = Some(OpenChat {
            id,
            view: None,
            folds: BTreeMap::new(),
            unacknowledged: false,
        });
        if self.online == Online::Open {
            self.request(
                Request::Attach { terminal: id },
                Asked::Doing("opening the chat"),
            );
        }
        self.dirty.all_turns = true;
        self.dirty.header = true;
        self.dirty.footer = true;
    }

    /// Shows the list of chats.
    pub fn close_chat(&mut self) {
        if let Some(open) = self.open.take()
            && self.online == Online::Open
        {
            self.request(Request::Detach { terminal: open.id }, Asked::Quiet);
        }
        self.dirty.home = true;
    }

    /// Asks the chat shown to do `action`, saying what failed as `doing`.
    pub fn chat(&mut self, action: ChatAction, doing: &'static str) {
        let Some(open) = &self.open else {
            return;
        };
        if self.online != Online::Open {
            self.note = Some(format!("Not connected: try {doing} again in a moment"));
            self.dirty.footer = true;
            return;
        }
        let terminal = open.id;
        self.request(Request::Chat { terminal, action }, Asked::Doing(doing));
    }

    /// Tells the daemon whether the user is looking at the app.
    pub fn present(&mut self, here: bool) {
        self.here = here;
        if self.online == Online::Open {
            self.outbox.push(ClientMessage::Present { here });
        }
    }

    /// The screen has been drawn: the daemon may send the next update of
    /// the chat shown.
    pub fn drawn(&mut self) {
        self.dirty = Dirty::default();
        if let Some(open) = &mut self.open
            && std::mem::take(&mut open.unacknowledged)
            && self.online == Online::Open
        {
            self.outbox
                .push(ClientMessage::FrameDone { terminal: open.id });
        }
    }

    /// What's to be sent.
    pub fn take_outbox(&mut self) -> Vec<ClientMessage> {
        std::mem::take(&mut self.outbox)
    }

    /// Notes a hand opening or folding the part `fold` of the chat shown,
    /// which is in turn `turn`.
    pub fn folded(&mut self, fold: &str, open: bool) {
        if let Some(chat) = &mut self.open {
            chat.folds.insert(fold.to_owned(), open);
        }
    }

    /// The chat shown's summary, from the list.
    #[must_use]
    pub fn open_summary(&self) -> Option<&ChatSummary> {
        let open = self.open.as_ref()?;
        self.chats.iter().find(|chat| chat.chat == open.id)
    }

    fn request(&mut self, request: Request, asked: Asked) {
        self.asked.push_back(asked);
        self.outbox.push(ClientMessage::Request(request));
    }
}

/// Notes in `dirty` what `patch` changes.
fn mark(dirty: &mut Dirty, patch: &Patch) {
    let Patch::Chat(patch) = patch else {
        dirty.header = true;
        return;
    };
    match patch {
        ChatPatch::Turn { index, .. } => {
            dirty.turns.insert(*index as usize);
        }
        ChatPatch::Item { turn, .. }
        | ChatPatch::Append { turn, .. }
        | ChatPatch::Output { turn, .. }
        | ChatPatch::Files { turn, .. } => {
            dirty.turns.insert(*turn as usize);
        }
        ChatPatch::Subagents(subagents) => {
            dirty
                .turns
                .extend(subagents.iter().map(|subagent| subagent.turn as usize));
        }
        ChatPatch::State(_) | ChatPatch::Usage(_) | ChatPatch::Agent(_) => {
            dirty.header = true;
            dirty.footer = true;
        }
        ChatPatch::Question(_)
        | ChatPatch::Notice(_)
        | ChatPatch::Settings(_)
        | ChatPatch::Providers(_) => dirty.footer = true,
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{ChatNode, ChatState, Item, Node, Turn};

    use super::*;

    fn turn(prompt: &str) -> Turn {
        Turn {
            prompt: prompt.into(),
            sent: 0,
            items: vec![Item::Words(String::new())],
            end: None,
            files: None,
            from: None,
            switch: None,
        }
    }

    fn snapshot(turns: Vec<Turn>) -> ServerMessage {
        ServerMessage::Event(Event::View {
            terminal: TerminalId(4),
            update: ViewUpdate::Snapshot(View {
                title: "chat".into(),
                cwd: None,
                root: Node::Chat(Box::new(ChatNode {
                    turns,
                    ..ChatNode::default()
                })),
            }),
        })
    }

    fn patch(patches: Vec<ChatPatch>) -> ServerMessage {
        ServerMessage::Event(Event::View {
            terminal: TerminalId(4),
            update: ViewUpdate::Patch(patches.into_iter().map(Patch::Chat).collect()),
        })
    }

    #[test]
    fn connecting_says_hello_and_watches() {
        let mut state = State::default();
        state.open_chat(TerminalId(4));
        assert!(
            state.take_outbox().is_empty(),
            "nothing goes before it's connected"
        );
        state.connected();
        let sent = state.take_outbox();
        assert_eq!(
            sent,
            [
                ClientMessage::Hello {
                    protocol_version: PROTOCOL_VERSION
                },
                ClientMessage::Request(Request::WatchWorkspaces),
                ClientMessage::Request(Request::WatchChats),
                ClientMessage::Present { here: true },
                ClientMessage::Request(Request::Attach {
                    terminal: TerminalId(4)
                }),
            ]
        );
        let older = ServerMessage::Hello {
            protocol_version: PROTOCOL_VERSION - 1,
        };
        assert_eq!(state.receive(older), Received::Reload);
    }

    #[test]
    fn a_chats_changes_are_drawn_then_acknowledged() {
        let mut state = State::default();
        state.connected();
        state.open_chat(TerminalId(4));
        state.take_outbox();
        state.receive(snapshot(vec![turn("a"), turn("b")]));
        assert!(state.dirty.all_turns);
        state.drawn();
        assert_eq!(
            state.take_outbox(),
            [ClientMessage::FrameDone {
                terminal: TerminalId(4)
            }]
        );
        state.receive(patch(vec![
            ChatPatch::Append {
                turn: 1,
                item: 0,
                text: "hi".into(),
            },
            ChatPatch::State(ChatState::Answering),
        ]));
        assert_eq!(state.dirty.turns, BTreeSet::from([1]));
        assert!(state.dirty.header && state.dirty.footer && !state.dirty.all_turns);
        let view = state.open.as_ref().unwrap().view.as_ref().unwrap();
        assert_eq!(
            view.chat().unwrap().turns[1].items[0],
            Item::Words("hi".into())
        );
        // Nothing goes back until it's drawn.
        assert!(state.take_outbox().is_empty());
        state.drawn();
        assert_eq!(state.take_outbox().len(), 1);
        state.drawn();
        assert!(state.take_outbox().is_empty(), "acknowledged once");
    }

    #[test]
    fn moving_between_chats_detaches_and_acknowledges_stragglers() {
        let mut state = State::default();
        state.connected();
        state.open_chat(TerminalId(4));
        state.take_outbox();
        state.open_chat(TerminalId(5));
        assert_eq!(
            state.take_outbox(),
            [
                ClientMessage::Request(Request::Detach {
                    terminal: TerminalId(4)
                }),
                ClientMessage::Request(Request::Attach {
                    terminal: TerminalId(5)
                }),
            ]
        );
        // An update of the chat left, sent before the detach landed.
        state.receive(snapshot(vec![]));
        assert_eq!(
            state.take_outbox(),
            [ClientMessage::FrameDone {
                terminal: TerminalId(4)
            }]
        );
        assert!(state.open.as_ref().unwrap().view.is_none());
    }

    #[test]
    fn what_fails_is_said() {
        let mut state = State::default();
        state.connected();
        state.open_chat(TerminalId(4));
        state.chat(ChatAction::Stop, "stopping the agent");
        for _ in 0..3 {
            state.receive(ServerMessage::Response(Response::Done));
        }
        state.receive(ServerMessage::Response(Response::Error(
            clackatoo_protocol::Error::NotAChat(TerminalId(4)),
        )));
        assert_eq!(
            state.note.as_deref(),
            Some("Failed stopping the agent: terminal 4 isn't a chat")
        );
        state.lost(2);
        state.chat(ChatAction::Stop, "stopping the agent");
        assert!(
            state.take_outbox().is_empty(),
            "nothing is sent while it's lost"
        );
        assert_eq!(state.online, Online::Lost(2));
    }
}
