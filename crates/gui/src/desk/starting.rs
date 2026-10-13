//! The start page in the desk (see [`crate::start`]): opening it for
//! `:Agent` with no agent named, asking the daemon what a new chat could
//! be and for the agent's sessions, passing on what comes, and starting,
//! showing or resuming the chat picked, where `:Agent` asked.
//!
//! It takes keys, clicks and the wheel as the finder does, through the
//! finder's own ways in (see `finding.rs`), and is drawn in its place: the
//! two are never open together.
//!
//! Each listing of an agent's sessions is numbered from the finder's
//! count of searches, so what comes for one no longer wanted, as after
//! the agent picked changes, is dropped by its number.

use clackatoo_protocol::{
    ClientMessage, NewAgent, PaneTerminal, Request, Response, ResumeSession, TerminalId,
};

use super::{Asked, Desk};
use crate::command::Place;
use crate::input::KeyPress;
use crate::options::Case;
use crate::start::{LastChat, NewChat, Outcome, StartPage};

impl Desk {
    /// Opens the start page, for a chat at `place`, and asks the daemon
    /// what a new chat could be. Its new chat can be started at once,
    /// before the daemon says.
    pub(super) fn open_start_page(&mut self, place: Place) {
        self.close_finder();
        if !self.connected {
            self.refuse("a chat needs the daemon, which is out of reach".into());
            return;
        }
        if let Some(shown) = self.shown {
            self.close_editor_panel(shown.pane);
        }
        let agent = Some(self.options.agent.clone()).filter(|name| !name.is_empty());
        let case = Case::of(&self.options);
        self.start = Some(StartPage::new(place, self.last_chat.clone(), agent, case));
        let pane = self.shown.map(|shown| shown.pane);
        self.request(Request::ChatChoices { pane }, Asked::ChatChoices);
    }

    /// Takes in the daemon's answer to what a new chat could be, for the
    /// start page, if it's still open: of the chats in no pane, those of
    /// the workspace shown, or of none. Returns whether to draw.
    pub(super) fn chat_choices(&mut self, response: Response) -> bool {
        let Some(page) = &mut self.start else {
            return false;
        };
        let choices = match response {
            Response::ChatChoices(choices) => choices,
            Response::Error(err) => {
                log::warn!("the daemon wouldn't say what a chat could be: {err}");
                return false;
            }
            _ => return false,
        };
        let workspace = self.shown.map(|shown| shown.workspace);
        let chats = (choices.unshown.iter())
            .filter(|chat| chat.workspace.is_none() || chat.workspace == workspace)
            .cloned()
            .collect();
        let outcome = page.choices(choices, chats);
        self.start_did(outcome);
        true
    }

    /// Takes in the sessions the daemon's listing numbered `id` found, or
    /// why it couldn't. Returns whether to draw.
    pub(crate) fn sessions_listed(
        &mut self,
        id: u64,
        sessions: Result<Vec<clackatoo_protocol::AgentSession>, String>,
    ) -> bool {
        let redraw = (self.start.as_mut()).is_some_and(|page| page.listed(id, sessions));
        if redraw {
            self.settle_lesson();
        }
        redraw
    }

    /// Handles a key while the start page is open.
    pub(super) fn start_key(&mut self, press: &KeyPress<'_>) {
        if let Some(page) = &mut self.start {
            let outcome = page.key(press);
            self.start_did(outcome);
        }
    }

    /// Carries out what a key, a click or the daemon did to the start page.
    pub(super) fn start_did(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Nothing | Outcome::Changed => {}
            Outcome::Close => self.start = None,
            Outcome::List { agent, cwd } => {
                self.searches += 1;
                let id = self.searches;
                (self.outbox).push(ClientMessage::ListSessions { id, agent, cwd });
                if let Some(page) = &mut self.start {
                    page.listing(id);
                }
            }
            Outcome::Start(new) => {
                let Some(place) = self.start.take().map(|page| page.place()) else {
                    return;
                };
                self.last_chat = LastChat {
                    agent: new.agent.clone(),
                    model: new.model.clone(),
                    cwd: new.cwd.clone(),
                };
                let chat = self.new_agent(new, place, None);
                self.open_at(place, chat);
            }
            Outcome::Show(chat) => {
                let Some(place) = self.start.take().map(|page| page.place()) else {
                    return;
                };
                self.show_chat(chat, place);
            }
            Outcome::Resume { agent, session } => {
                let Some(place) = self.start.take().map(|page| page.place()) else {
                    return;
                };
                let new = NewChat {
                    agent: Some(agent),
                    model: None,
                    cwd: Some(session.cwd),
                };
                let resume = ResumeSession {
                    session: session.session,
                    title: session.title,
                };
                let chat = self.new_agent(new, place, Some(resume));
                self.open_at(place, chat);
            }
        }
    }

    /// What starts `new` in a new pane at `place`, going on with `resume`
    /// if it's given: with the tools, Task tool and browser the options
    /// give a new chat.
    fn new_agent(&self, new: NewChat, place: Place, resume: Option<ResumeSession>) -> PaneTerminal {
        let side = match place {
            Place::Beside(side) => Some(side),
            Place::Tab | Place::Here => None,
        };
        PaneTerminal::Agent(NewAgent {
            size: self.new_pane_size(side),
            cwd: new.cwd,
            agent: new.agent,
            tools: self.options.agenttools.asked(),
            task: self.options.claudetask,
            browser: self.options.chatbrowser,
            model: new.model,
            resume,
        })
    }

    /// Shows `chat`: by going to the pane it's in, if it's in one, or else
    /// in a new pane at `place`.
    fn show_chat(&mut self, chat: TerminalId, place: Place) {
        let pane = (self.workspaces.list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .find(|pane| pane.content == clackatoo_protocol::Content::Terminal(chat));
        match pane {
            Some(pane) => self.show_pane(pane.id),
            None => self.open_at(place, PaneTerminal::Existing(chat)),
        }
    }

    /// Opens a new pane showing `terminal` at `place`: in a new tab, or
    /// beside the focused pane.
    fn open_at(&mut self, place: Place, terminal: PaneTerminal) {
        let Some(shown) = self.shown else { return };
        let request = match place {
            Place::Tab | Place::Here => Request::NewTab {
                workspace: shown.workspace,
                terminal,
            },
            Place::Beside(side) => Request::Split {
                pane: shown.pane,
                side,
                terminal,
            },
        };
        self.request(request, Asked::Pane);
    }
}
