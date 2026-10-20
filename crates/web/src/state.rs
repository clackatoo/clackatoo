//! What the app knows, kept current with what the daemon says: the
//! workspaces, the chats and terminals as lists, and the chat or terminal
//! shown, with what's changed in it since it was last drawn. No browser
//! in here: messages in, messages out, so it's tested on its own.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use clackatoo_protocol::{
    ChatAction, ChatNode, ChatPatch, ChatSummary, ClientMessage, Content, Event, PROTOCOL_VERSION,
    Patch, Request, Response, ServerMessage, Size, TerminalId, TerminalInfo, View, ViewUpdate,
    Workspace, WorkspaceId, Workspaces,
};

use crate::keys::{self, Key, Modifiers};
use crate::term::{Fit, OpenTerminal};

/// How long what was last sent may go unanswered, in milliseconds,
/// before keys for a terminal are turned down rather than sent: a key
/// that reaches a shell long after it was typed may do what the user no
/// longer means. The window's rule, as it has it for remote daemons.
pub const KEYS_UNANSWERED: f64 = 3000.0;

/// How often a fit is asked for again while the terminal is shown, in
/// milliseconds: the daemon puts one back that isn't asked for again in a
/// minute, as a phone put away may never say it's gone.
pub const FIT_EVERY: f64 = 30_000.0;

/// The most text pasted at once, in bytes: a larger paste could be more
/// than the daemon takes in one message, which would cut the phone off.
pub const MOST_PASTE: usize = 1 << 20;

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

/// What a request was for, to take in its answer, or say what went wrong
/// if it fails.
#[derive(Clone, Copy, Debug)]
enum Asked {
    /// Nothing to say: its failure is only logged.
    Quiet,
    /// Something the user did, as "sending the message".
    Doing(&'static str),
    /// The list of terminals.
    Terminals,
    /// History of the terminal shown, while what it held was of this
    /// generation.
    History { generation: u64 },
    /// Keys typed into the terminal shown.
    Typing,
    /// Fitting the terminal shown to this size, or putting it back.
    Fit(Option<Size>),
    /// Keeping the fit of the terminal shown, which the daemon may have
    /// put back meanwhile.
    Renew,
}

/// What the app knows.
#[derive(Debug)]
pub struct State {
    pub online: Online,
    pub workspaces: Workspaces,
    pub chats: Vec<ChatSummary>,
    /// Every terminal, as the daemon last listed them.
    pub terminals: Vec<TerminalInfo>,
    pub open: Option<OpenChat>,
    /// The terminal shown, if one is rather than a chat.
    pub term: Option<OpenTerminal>,
    /// Something to tell the user for a while, such as what failed.
    pub note: Option<String>,
    pub dirty: Dirty,
    /// Now, in milliseconds, as the app last said.
    pub now: f64,
    /// The requests sent and not answered, oldest first, with when each
    /// was sent.
    asked: VecDeque<(Asked, f64)>,
    outbox: Vec<ClientMessage>,
    /// Whether the user is looking at the app, as the daemon was last told.
    here: bool,
    /// The terminals are being listed.
    listing: bool,
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
            terminals: Vec::new(),
            open: None,
            term: None,
            note: None,
            dirty: Dirty::default(),
            now: 0.0,
            asked: VecDeque::new(),
            outbox: Vec::new(),
            here: true,
            listing: false,
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
        self.listing = false;
        self.request(Request::WatchWorkspaces, Asked::Quiet);
        self.request(Request::WatchChats, Asked::Quiet);
        self.list_terminals();
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
        if let Some(term) = &mut self.term {
            // Keys go again: those turned down while lost are of the past.
            term.refused = 0;
        }
        if let Some(term) = self.term.as_mut().filter(|term| !term.paused) {
            term.unacknowledged = false;
            term.fit = Fit::Off;
            term.dirty.header = true;
            let terminal = term.id;
            self.request(
                Request::Attach { terminal },
                Asked::Doing("opening the terminal"),
            );
        }
        self.dirty.home = true;
    }

    /// The connection was lost, and is tried again in `retry` seconds.
    pub fn lost(&mut self, retry: u32) {
        self.online = Online::Lost(retry);
        self.asked.clear();
        self.outbox.clear();
        self.listing = false;
        self.dirty.home = true;
        self.dirty.header = true;
        if let Some(term) = &mut self.term {
            // The daemon puts a fit back as its client goes.
            term.fit = Fit::Off;
            term.dirty.header = true;
        }
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
        let asked = self.asked.pop_front().map(|(asked, _)| asked);
        self.keys_heard();
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
            (Response::Terminals(terminals), _) => {
                self.terminals = terminals;
                self.listing = false;
                self.dirty.home = true;
                if let Some(term) = &mut self.term {
                    term.dirty.header = true;
                }
            }
            (Response::History(lines), Some(Asked::History { generation })) => {
                if let Some(term) = &mut self.term {
                    term.history_came(generation, lines);
                }
            }
            // Fitted: the daemon has made it that size, which the next
            // snapshot shows, if it wasn't already.
            (Response::Done, Some(Asked::Fit(fit))) => {
                if let Some(term) = &mut self.term {
                    term.fit = fit.map_or(Fit::Off, Fit::On);
                    term.dirty.header = true;
                }
            }
            (Response::Error(err), Some(asked)) => self.failed(&err, asked),
            _ => {}
        }
    }

    /// The request for `asked` failed, with `err`: says so, if it's to be
    /// said, and gives up what waited on it.
    fn failed(&mut self, err: &clackatoo_protocol::Error, asked: Asked) {
        let note = match asked {
            Asked::Quiet => None,
            Asked::Doing(what) => Some(format!("Failed {what}: {err}")),
            Asked::Terminals => {
                self.listing = false;
                None
            }
            Asked::History { generation } => {
                if let Some(term) = &mut self.term {
                    term.history_failed(generation);
                }
                None
            }
            Asked::Typing => Some(format!("Keys not sent: {err}")),
            Asked::Fit(_) => {
                if let Some(term) = &mut self.term {
                    term.fit = Fit::Off;
                }
                Some(format!("Failed fitting the terminal: {err}"))
            }
            // Put back meanwhile, as when a window typed: the header says
            // so, and it isn't asked for again.
            Asked::Renew => {
                if let Some(term) = &mut self.term {
                    term.fit = Fit::Off;
                    term.dirty.header = true;
                }
                None
            }
        };
        if let Some(note) = note {
            self.note = Some(note);
            self.dirty.home = true;
            self.dirty.footer = true;
            if let Some(term) = &mut self.term {
                term.dirty.header = true;
            }
        }
    }

    /// Takes in an event.
    fn event(&mut self, event: Event) {
        match event {
            Event::Workspaces(workspaces) => {
                self.workspaces = workspaces;
                self.dirty.home = true;
                // Terminals may have come or gone.
                self.list_terminals();
            }
            // The phone has no clipboard to share with a program: it's told
            // so at once, rather than left waiting.
            Event::ClipboardAsked { terminal, id, .. } => {
                self.outbox.push(ClientMessage::Clipboard {
                    terminal,
                    id,
                    text: None,
                });
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
        if let Some(term) = self.term.as_mut().filter(|term| term.id == terminal) {
            if let Some(request) = term.apply(update) {
                let generation = term.history.generation;
                self.request(request, Asked::History { generation });
            }
            return;
        }
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
                let asking = |view: &View| view.chat().is_some_and(ChatNode::asking);
                let was_asking = asking(view);
                view.apply(ViewUpdate::Patch(patches));
                // The line the turn going folds its work under says
                // whether the agent waits for you.
                if asking(view) != was_asking {
                    let going = view.chat().and_then(|chat| chat.turns.len().checked_sub(1));
                    self.dirty.turns.extend(going);
                }
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
        self.close_terminal();
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

    /// Shows terminal `id`.
    pub fn open_terminal(&mut self, id: TerminalId) {
        if self.term.as_ref().is_some_and(|term| term.id == id) {
            return;
        }
        self.close_chat();
        self.close_terminal();
        self.term = Some(OpenTerminal::new(id));
        self.note = None;
        if self.online == Online::Open {
            self.request(
                Request::Attach { terminal: id },
                Asked::Doing("opening the terminal"),
            );
        }
    }

    /// Stops showing the terminal shown, if one is. The daemon puts back
    /// its fit, if it's fitted.
    pub fn close_terminal(&mut self) {
        if let Some(term) = self.term.take()
            && !term.paused
            && self.online == Online::Open
        {
            self.request(Request::Detach { terminal: term.id }, Asked::Quiet);
        }
        self.dirty.home = true;
    }

    /// The page was hidden, or shown again: a terminal shown stops being
    /// followed while it's hidden, so a phone in a pocket neither holds
    /// it fitted nor takes its updates, and is followed again once shown.
    pub fn terminal_hidden(&mut self, hidden: bool) {
        let online = self.online == Online::Open;
        let Some(term) = &mut self.term else {
            return;
        };
        if term.paused == hidden {
            return;
        }
        term.paused = hidden;
        term.fit = Fit::Off;
        term.unacknowledged = false;
        term.dirty.header = true;
        let terminal = term.id;
        if !online {
            return;
        }
        if hidden {
            self.request(Request::Detach { terminal }, Asked::Quiet);
        } else {
            self.request(
                Request::Attach { terminal },
                Asked::Doing("opening the terminal"),
            );
        }
    }

    /// Asks for the list of terminals again, unless it's being asked for.
    pub fn list_terminals(&mut self) {
        if self.listing || self.online != Online::Open {
            return;
        }
        self.listing = true;
        self.request(Request::ListTerminals, Asked::Terminals);
    }

    /// The terminals of workspace `workspace`, in the order its tabs have
    /// them, or with none, those in no workspace's panes: every terminal
    /// that isn't a chat.
    #[must_use]
    pub fn terminals_of(&self, workspace: Option<WorkspaceId>) -> Vec<&TerminalInfo> {
        let chat = |id: TerminalId| self.chats.iter().any(|chat| chat.chat == id);
        let in_panes = |workspace: &Workspace| -> Vec<TerminalId> {
            (workspace.tabs.iter())
                .flat_map(|tab| tab.layout.panes())
                .filter_map(|pane| match pane.content {
                    Content::Terminal(id) if !chat(id) => Some(id),
                    _ => None,
                })
                .collect()
        };
        let info = |id: TerminalId| self.terminals.iter().find(|info| info.id == id);
        if let Some(workspace) = workspace {
            return (self.workspaces.list.iter())
                .find(|found| found.id == workspace)
                .map(in_panes)
                .unwrap_or_default()
                .into_iter()
                .filter_map(info)
                .collect();
        }
        let shown: Vec<TerminalId> = self.workspaces.list.iter().flat_map(in_panes).collect();
        (self.terminals.iter())
            .filter(|info| !chat(info.id) && !shown.contains(&info.id))
            .collect()
    }

    /// The daemon answered: if keys go again, and the terminal is shown,
    /// the count of keys turned down meanwhile is done with, and the
    /// terminal stops saying they aren't sent.
    fn keys_heard(&mut self) {
        let go = self.keys_go();
        if let Some(term) = self.term.as_mut().filter(|term| term.refused > 0)
            && go
            && term.grid().is_some()
        {
            term.refused = 0;
            term.dirty.header = true;
        }
    }

    /// Whether keys typed now go to the terminal: the daemon is reached,
    /// and has answered what was sent within [`KEYS_UNANSWERED`].
    #[must_use]
    pub fn keys_go(&self) -> bool {
        self.online == Online::Open
            && (self.asked.front()).is_none_or(|&(_, sent)| self.now - sent <= KEYS_UNANSWERED)
    }

    /// When keys stop going, in milliseconds, if what's waited for isn't
    /// answered by then: for the app to look again then.
    #[must_use]
    pub fn keys_stop_at(&self) -> Option<f64> {
        let &(_, sent) = self.asked.front()?;
        Some(sent + KEYS_UNANSWERED)
    }

    /// Types `key` into the terminal shown, with `modifiers` held, and
    /// Ctrl if it was tapped on the row of keys. Returns whether it went.
    pub fn type_key(&mut self, key: &Key, modifiers: Modifiers) -> bool {
        let Some(term) = &mut self.term else {
            return false;
        };
        let modifiers = Modifiers {
            ctrl: modifiers.ctrl || std::mem::take(&mut term.ctrl),
            ..modifiers
        };
        let modes = term.grid().map(|grid| grid.state.modes).unwrap_or_default();
        self.send_keys(keys::encode(key, modifiers, modes))
    }

    /// Pastes `text` into the terminal shown. Returns whether it went.
    pub fn paste(&mut self, text: &str) -> bool {
        let Some(term) = &self.term else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        if text.len() > MOST_PASTE {
            self.note = Some("That's more than a phone pastes at once: 1 MiB at most".to_owned());
            if let Some(term) = &mut self.term {
                term.dirty.header = true;
            }
            return false;
        }
        let modes = term.grid().map(|grid| grid.state.modes).unwrap_or_default();
        self.send_keys(keys::paste(text, modes))
    }

    /// How many lines pasting `text` would run, one command each, if more
    /// than one: the program hasn't asked for pastes to be marked, so
    /// each line ends with Enter, and it's best asked first. A line ends
    /// at CR, LF or both, as each is sent as Enter.
    #[must_use]
    pub fn paste_runs_lines(&self, text: &str) -> Option<usize> {
        let modes = self.term.as_ref()?.grid()?.state.modes;
        if modes.bracketed_paste {
            return None;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines = text.trim_end_matches('\n').split('\n').count();
        (lines > 1).then_some(lines)
    }

    /// Sends `bytes` to the terminal shown, as typed, unless keys don't go
    /// now: then they're turned down, and the terminal says so. They're
    /// never kept to send later, nor drawn before the terminal echoes them.
    fn send_keys(&mut self, bytes: Vec<u8>) -> bool {
        let go = self.keys_go();
        let Some(term) = &mut self.term else {
            return false;
        };
        term.dirty.header = true;
        if bytes.is_empty() {
            return false;
        }
        if !go || term.paused || term.grid().is_none() {
            term.refused += 1;
            return false;
        }
        term.refused = 0;
        let terminal = term.id;
        self.request(Request::SendInput { terminal, bytes }, Asked::Typing);
        true
    }

    /// Taps Ctrl on the row of keys: the next key goes with it, or with a
    /// second tap, doesn't.
    pub fn tap_ctrl(&mut self) {
        if let Some(term) = &mut self.term {
            term.ctrl = !term.ctrl;
            term.dirty.header = true;
        }
    }

    /// Fits the terminal shown to `size`, the phone's, until the daemon
    /// puts it back, or with none, puts it back now.
    pub fn fit(&mut self, size: Option<Size>) {
        let online = self.online == Online::Open;
        let Some(term) = &mut self.term else {
            return;
        };
        term.dirty.header = true;
        if !online {
            self.note = Some("Not connected: try again in a moment".to_owned());
            return;
        }
        term.fit = size.map_or(Fit::Off, Fit::Asked);
        term.fit_asked = self.now;
        let terminal = term.id;
        let renew = false;
        self.request(
            Request::Fit {
                terminal,
                size,
                renew,
            },
            Asked::Fit(size),
        );
    }

    /// Fits the terminal shown, if it's fitted, to `size`, the space the
    /// phone has now: as the keyboard comes up, say.
    pub fn refit(&mut self, size: Size) {
        match self.term.as_ref().map(|term| term.fit) {
            Some(Fit::On(fitted) | Fit::Asked(fitted)) if fitted != size => self.fit(Some(size)),
            _ => {}
        }
    }

    /// Asks for the fit of the terminal shown again, if it's fitted and
    /// was last asked for [`FIT_EVERY`] ago, so the daemon keeps it.
    pub fn renew_fit(&mut self) {
        if self.online != Online::Open {
            return;
        }
        let Some(term) = self.term.as_mut().filter(|term| !term.paused) else {
            return;
        };
        let (Fit::On(size) | Fit::Asked(size)) = term.fit else {
            return;
        };
        if self.now - term.fit_asked < FIT_EVERY {
            return;
        }
        term.fit_asked = self.now;
        let terminal = term.id;
        let size = Some(size);
        let renew = true;
        self.request(
            Request::Fit {
                terminal,
                size,
                renew,
            },
            Asked::Renew,
        );
    }

    /// Asks for the page of history before what's held, as the user nears
    /// the top of what's shown, if there's more and none is being fetched.
    pub fn older_history(&mut self) {
        if self.online != Online::Open {
            return;
        }
        let Some(term) = self.term.as_mut().filter(|term| !term.paused) else {
            return;
        };
        if let Some(request) = term.older() {
            let generation = term.history.generation;
            self.request(request, Asked::History { generation });
        }
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
        if let Some(term) = &mut self.term {
            term.drawn();
            if std::mem::take(&mut term.unacknowledged) && self.online == Online::Open {
                self.outbox
                    .push(ClientMessage::FrameDone { terminal: term.id });
            }
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

    /// Opens the part `fold` of the chat shown if it's folded, or folds it.
    pub fn toggle_fold(&mut self, fold: &str) {
        if let Some(chat) = &mut self.open {
            let open = chat.folds.get(fold).copied().unwrap_or(false);
            chat.folds.insert(fold.to_owned(), !open);
        }
    }

    /// The chat shown's summary, from the list.
    #[must_use]
    pub fn open_summary(&self) -> Option<&ChatSummary> {
        let open = self.open.as_ref()?;
        self.chats.iter().find(|chat| chat.chat == open.id)
    }

    fn request(&mut self, request: Request, asked: Asked) {
        self.asked.push_back((asked, self.now));
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
    use clackatoo_protocol::{ChatNode, ChatState, InputModes, Item, Node, Turn};

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
                ClientMessage::Request(Request::ListTerminals),
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
    fn a_question_coming_or_going_redraws_the_turn_going() {
        use clackatoo_protocol::Question;

        let mut state = State::default();
        state.connected();
        state.open_chat(TerminalId(4));
        state.receive(snapshot(vec![turn("a"), turn("b")]));
        state.drawn();
        let question = Question {
            id: 1,
            title: "Write notes.txt".into(),
            choices: Vec::new(),
            own: false,
        };
        state.receive(patch(vec![ChatPatch::Question(Some(question.clone()))]));
        assert_eq!(state.dirty.turns, BTreeSet::from([1]), "its line says so");
        assert!(state.dirty.footer);
        state.drawn();
        // The same question again changes nothing in the turns.
        state.receive(patch(vec![ChatPatch::Question(Some(question))]));
        assert!(state.dirty.turns.is_empty());
        state.drawn();
        state.receive(patch(vec![ChatPatch::Question(None)]));
        assert_eq!(state.dirty.turns, BTreeSet::from([1]));
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
        for _ in 0..4 {
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

    /// A terminal's view, at `size`, in `modes`.
    fn screen(size: Size, modes: InputModes) -> ServerMessage {
        ServerMessage::Event(Event::View {
            terminal: TerminalId(9),
            update: ViewUpdate::Snapshot(View {
                title: "sh".into(),
                cwd: Some("/home".into()),
                root: Node::Grid(clackatoo_protocol::GridNode {
                    size,
                    state: clackatoo_protocol::GridState {
                        modes,
                        ..clackatoo_protocol::GridState::default()
                    },
                    lines: vec![clackatoo_protocol::Line::default(); usize::from(size.rows())],
                    marks: clackatoo_protocol::BlockMarks::default(),
                    links: Vec::new(),
                }),
            }),
        })
    }

    /// A state connected, showing terminal 9 at 80 by 24, with every
    /// request answered and nothing to send.
    fn showing_a_terminal(modes: InputModes) -> State {
        let mut state = State::default();
        state.connected();
        state.open_terminal(TerminalId(9));
        state.receive(screen(Size::new(80, 24).unwrap(), modes));
        answer_all(&mut state);
        state.drawn();
        state.take_outbox();
        state
    }

    /// Answers every request waiting, as done.
    fn answer_all(state: &mut State) {
        while !state.asked.is_empty() {
            state.receive(ServerMessage::Response(Response::Done));
        }
    }

    /// The bytes of each key sent.
    fn typed(state: &mut State) -> Vec<Vec<u8>> {
        (state.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Request(Request::SendInput { terminal, bytes }) => {
                    assert_eq!(terminal, TerminalId(9));
                    Some(bytes)
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_terminal_is_followed_drawn_then_acknowledged() {
        let mut state = State::default();
        state.connected();
        state.take_outbox();
        state.open_terminal(TerminalId(9));
        assert_eq!(
            state.take_outbox(),
            [ClientMessage::Request(Request::Attach {
                terminal: TerminalId(9)
            })]
        );
        state.receive(screen(Size::new(80, 24).unwrap(), InputModes::default()));
        assert!(state.term.as_ref().unwrap().dirty.all);
        assert!(state.take_outbox().is_empty(), "not until it's drawn");
        state.drawn();
        assert_eq!(
            state.take_outbox(),
            [ClientMessage::FrameDone {
                terminal: TerminalId(9)
            }]
        );
        // Going to a chat leaves it.
        state.open_chat(TerminalId(4));
        assert_eq!(
            state.take_outbox()[0],
            ClientMessage::Request(Request::Detach {
                terminal: TerminalId(9)
            })
        );
        assert!(state.term.is_none());
    }

    #[test]
    fn each_key_goes_as_typed_ctrl_tapped_goes_with_the_next() {
        let mut state = showing_a_terminal(InputModes::default());
        let none = Modifiers::default();
        assert!(state.type_key(&Key::Text("l".into()), none));
        assert!(state.type_key(&Key::Text("s".into()), none));
        assert!(state.type_key(&Key::Enter, none));
        assert_eq!(
            typed(&mut state),
            [b"l".to_vec(), b"s".to_vec(), b"\r".to_vec()]
        );
        state.tap_ctrl();
        assert!(state.term.as_ref().unwrap().ctrl);
        state.type_key(&Key::Text("c".into()), none);
        state.type_key(&Key::Text("c".into()), none);
        assert_eq!(typed(&mut state), [vec![0x03], b"c".to_vec()], "once");
        // Tapped twice, it's off.
        state.tap_ctrl();
        state.tap_ctrl();
        state.type_key(&Key::Escape, none);
        assert_eq!(typed(&mut state), [vec![0x1b]]);
        assert!(state.paste("a\nb"));
        assert_eq!(typed(&mut state), [b"a\rb".to_vec()]);
    }

    #[test]
    fn arrows_go_as_the_programs_cursor_mode_asks() {
        let application = InputModes {
            app_cursor_keys: true,
            bracketed_paste: true,
            ..InputModes::default()
        };
        let mut state = showing_a_terminal(application);
        state.type_key(&Key::Up, Modifiers::default());
        state.paste("x");
        assert_eq!(
            typed(&mut state),
            [b"\x1bOA".to_vec(), b"\x1b[200~x\x1b[201~".to_vec()]
        );
    }

    #[test]
    fn keys_are_turned_down_while_the_daemon_isnt_reached_never_kept() {
        let mut state = showing_a_terminal(InputModes::default());
        let key = Key::Text("x".into());
        state.now = 1000.0;
        assert!(state.type_key(&key, Modifiers::default()));
        assert_eq!(typed(&mut state).len(), 1);
        // Unanswered for three seconds: no more go.
        state.now = 3999.0;
        assert!(state.keys_go(), "not quite yet");
        assert_eq!(state.keys_stop_at(), Some(4000.0));
        state.now = 4001.0;
        assert!(!state.keys_go());
        assert!(!state.type_key(&key, Modifiers::default()));
        assert!(!state.type_key(&key, Modifiers::default()));
        assert!(typed(&mut state).is_empty());
        assert_eq!(state.term.as_ref().unwrap().refused, 2, "each one said");
        // Answered, they go again, and the terminal stops saying they don't.
        state.receive(ServerMessage::Response(Response::Done));
        assert_eq!(state.term.as_ref().unwrap().refused, 0);
        assert!(state.type_key(&key, Modifiers::default()));
        assert_eq!(typed(&mut state), [b"x".to_vec()]);
        // Lost, nothing goes, and nothing is kept for when it's back.
        state.lost(1);
        assert!(!state.type_key(&key, Modifiers::default()));
        assert_eq!(state.term.as_ref().unwrap().refused, 1);
        state.connected();
        assert!(typed(&mut state).is_empty());
        assert_eq!(state.term.as_ref().unwrap().refused, 0, "back again");
    }

    #[test]
    fn a_fit_is_on_once_the_daemon_says_asked_again_and_fitted_again_to_the_space_left() {
        let mut state = showing_a_terminal(InputModes::default());
        let phone = Size::new(40, 30).unwrap();
        state.now = 1000.0;
        state.fit(Some(phone));
        state.take_outbox();
        // Done says it's fitted, whatever the snapshot says, or doesn't.
        state.receive(ServerMessage::Response(Response::Done));
        assert_eq!(state.term.as_ref().unwrap().fit, Fit::On(phone));
        // Asked for again half a minute on, without saying "Fitting…".
        state.now = 20_000.0;
        state.renew_fit();
        assert!(state.take_outbox().is_empty(), "not yet");
        state.now = 31_000.0;
        state.renew_fit();
        assert_eq!(
            state.take_outbox(),
            [ClientMessage::Request(Request::Fit {
                terminal: TerminalId(9),
                size: Some(phone),
                renew: true,
            })]
        );
        assert_eq!(state.term.as_ref().unwrap().fit, Fit::On(phone));
        state.receive(ServerMessage::Response(Response::Done));
        // The keyboard came up: fitted to the rows left, once.
        let smaller = Size::new(40, 18).unwrap();
        state.refit(smaller);
        state.refit(smaller);
        assert_eq!(state.take_outbox().len(), 1);
        assert_eq!(state.term.as_ref().unwrap().fit, Fit::Asked(smaller));
        // Not fitted, it isn't fitted by the screen changing.
        state.receive(ServerMessage::Response(Response::Done));
        state.fit(None);
        state.receive(ServerMessage::Response(Response::Done));
        state.take_outbox();
        state.refit(phone);
        state.now = 100_000.0;
        state.renew_fit();
        assert!(state.take_outbox().is_empty());
        assert_eq!(state.term.as_ref().unwrap().fit, Fit::Off);
    }

    #[test]
    fn a_fit_put_back_meanwhile_is_off_once_asking_again_finds_it_gone() {
        let mut state = showing_a_terminal(InputModes::default());
        let phone = Size::new(40, 30).unwrap();
        state.fit(Some(phone));
        state.receive(ServerMessage::Response(Response::Done));
        state.take_outbox();
        // A window typed, and its size is the phone's: no snapshot says
        // the fit went back, but asking to keep it does.
        state.now = 31_000.0;
        state.renew_fit();
        state.take_outbox();
        state.receive(ServerMessage::Response(Response::Error(
            clackatoo_protocol::Error::NotFitted(TerminalId(9)),
        )));
        assert_eq!(state.term.as_ref().unwrap().fit, Fit::Off);
        assert_eq!(state.note, None, "nothing failed");
        state.now = 100_000.0;
        state.renew_fit();
        assert!(state.take_outbox().is_empty(), "not asked for again");
    }

    #[test]
    fn pastes_are_capped_and_lines_that_would_run_are_counted() {
        let mut state = showing_a_terminal(InputModes::default());
        assert_eq!(state.paste_runs_lines("ls\n"), None, "one line");
        assert_eq!(state.paste_runs_lines("ls\nrm x\r\n"), Some(2));
        assert_eq!(
            state.paste_runs_lines("a\rb\r"),
            Some(2),
            "CR alone ends one too"
        );
        assert_eq!(state.paste_runs_lines("a\r\n"), None);
        assert!(!state.paste(&"x".repeat(MOST_PASTE + 1)));
        assert!(typed(&mut state).is_empty());
        assert!(state.note.as_deref().unwrap().contains("1 MiB"));
        let mut marked = showing_a_terminal(InputModes {
            bracketed_paste: true,
            ..InputModes::default()
        });
        assert_eq!(marked.paste_runs_lines("ls\nrm x\n"), None, "marked");
        assert!(marked.paste(&"x".repeat(MOST_PASTE)));
    }

    #[test]
    fn a_fit_is_asked_for_and_put_back() {
        let mut state = showing_a_terminal(InputModes::default());
        let phone = Size::new(40, 30).unwrap();
        state.fit(Some(phone));
        assert_eq!(
            state.take_outbox(),
            [ClientMessage::Request(Request::Fit {
                terminal: TerminalId(9),
                size: Some(phone),
                renew: false,
            })]
        );
        assert_eq!(state.term.as_ref().unwrap().fit, Fit::Asked(phone));
        state.receive(ServerMessage::Response(Response::Done));
        state.receive(screen(phone, InputModes::default()));
        assert_eq!(state.term.as_ref().unwrap().fit, Fit::On(phone));
        state.fit(None);
        assert_eq!(
            state.take_outbox(),
            [ClientMessage::Request(Request::Fit {
                terminal: TerminalId(9),
                size: None,
                renew: false,
            })]
        );
        assert_eq!(state.term.as_ref().unwrap().fit, Fit::Off);
        // Hidden, the terminal is left, which puts a fit back too.
        state.fit(Some(phone));
        state.take_outbox();
        state.terminal_hidden(true);
        assert_eq!(
            state.take_outbox(),
            [ClientMessage::Request(Request::Detach {
                terminal: TerminalId(9)
            })]
        );
        assert_eq!(state.term.as_ref().unwrap().fit, Fit::Off);
        assert!(!state.type_key(&Key::Enter, Modifiers::default()));
        state.terminal_hidden(false);
        assert_eq!(
            state.take_outbox(),
            [ClientMessage::Request(Request::Attach {
                terminal: TerminalId(9)
            })]
        );
    }

    #[test]
    fn a_programs_question_about_the_clipboard_is_answered_at_once() {
        let mut state = showing_a_terminal(InputModes::default());
        state.receive(ServerMessage::Event(Event::ClipboardAsked {
            terminal: TerminalId(9),
            id: 3,
            selection: clackatoo_protocol::Selection::Clipboard,
        }));
        assert_eq!(
            state.take_outbox(),
            [ClientMessage::Clipboard {
                terminal: TerminalId(9),
                id: 3,
                text: None,
            }]
        );
    }

    #[test]
    fn terminals_are_listed_by_workspace_chats_apart() {
        use clackatoo_protocol::{Axis, Layout, Pane, PaneId, Tab, TabId, Workspace};
        let pane = |id: u32| {
            Layout::Pane(Pane {
                id: PaneId(id),
                content: Content::Terminal(TerminalId(id)),
                home: None,
                key: None,
            })
        };
        let mut state = State {
            workspaces: Workspaces {
                home: None,
                restored: Vec::new(),
                list: vec![Workspace {
                    id: WorkspaceId(1),
                    name: "clackatoo".into(),
                    tabs: vec![Tab {
                        id: TabId(1),
                        layout: Layout::Split {
                            axis: Axis::LeftToRight,
                            parts: vec![pane(2), pane(3)],
                            shares: vec![1, 1],
                        },
                        focused: PaneId(2),
                        name: None,
                    }],
                    active_tab: TabId(1),
                    home: None,
                    lent: None,
                    background: Vec::new(),
                }],
                active: None,
                machine: None,
                others: Vec::new(),
                passed: Vec::new(),
            },
            ..State::default()
        };
        let info = |id: u32| TerminalInfo {
            id: TerminalId(id),
            pid: None,
            size: Size::new(80, 24).unwrap(),
            title: String::new(),
            status: clackatoo_protocol::TerminalStatus::Running,
            cwd: None,
            command: None,
        };
        state.terminals = vec![info(2), info(3), info(5)];
        state.chats = vec![ChatSummary {
            chat: TerminalId(3),
            title: "chat".into(),
            agent: "Claude Code".into(),
            state: ChatState::Ready,
            asking: false,
            unseen: false,
            parent: None,
            workspace: Some(WorkspaceId(1)),
            active: 0,
        }];
        let ids = |list: Vec<&TerminalInfo>| list.iter().map(|info| info.id.0).collect::<Vec<_>>();
        assert_eq!(ids(state.terminals_of(Some(WorkspaceId(1)))), [2]);
        assert_eq!(ids(state.terminals_of(None)), [5], "in no pane");
    }
}
