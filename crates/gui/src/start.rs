//! The start page `:Agent` opens: start a new chat, or go on with one.
//!
//! Its first row is a new chat, with pickers for its agent, model and
//! folder, which start as the last chat started from it had them, or else
//! as `:Agent` alone would: Enter starts it at once, so a new chat is
//! still a key away. Under it, to go on with instead:
//!
//! - this workspace's chats in no pane, such as those agents started in
//!   the background, which Enter shows;
//! - the sessions the agent picked kept for the folder picked, its own
//!   or clackatoo's, newest first, which Enter goes on with in a new chat
//!   (see [`clackatoo_protocol::NewAgent::resume`]). The daemon asks the
//!   agent for them, which may take it a moment to start; changing the
//!   agent or the folder asks again.
//!
//! What's typed filters these as the finder does, and the page is drawn
//! as the finder is (see [`crate::finder::view`]), over the panes. It's
//! the window's alone, as the finder is: nothing of it is the daemon's
//! until a chat is started or shown, in the place `:Agent`, `:|Agent` or
//! `:-Agent` asked for.
//!
//! | Key | Does |
//! |---|---|
//! | Enter | Starts the new chat, or shows or goes on with the one picked |
//! | `n` | Starts the new chat, whatever is picked |
//! | `j`, `k`; Down, Up; Ctrl+N, Ctrl+P | The next or previous row |
//! | Tab, Shift+Tab | The new chat's next or previous picker: agent, model, folder |
//! | `l`, `h`; Right, Left | The picker's next or previous choice |
//! | `/` | Starts typing, to filter |
//! | Typing, Backspace, Ctrl+U, Ctrl+W | Changes what's typed |
//! | Esc, Ctrl+C | Clears what's typed, or else closes the page |
//!
//! `j`, `k`, `n`, `h` and `l` are keys only while nothing's typed; any
//! other letter starts typing, and once typing, every letter types.

use clackatoo_protocol::{
    AgentSession, ChatChoices, ChatState, ChatSummary, Provider, ProviderModels, TerminalId,
};
use winit::keyboard::{Key, NamedKey};

use crate::command::Place;
use crate::finder::rank::{Items, Ranking};
use crate::finder::view::{FinderView, PreviewView, Row, fit};
use crate::input::KeyPress;
use crate::options::Case;
use crate::render::Ink;

/// What the last new chat started from the page had, for the next page to
/// start with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LastChat {
    pub(crate) agent: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) cwd: Option<String>,
}

/// A new chat to start: `None`s leave it to the daemon, as `:Agent` alone
/// does, which starts it with its default agent, on the agent's own
/// model, in the focused pane's directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NewChat {
    pub(crate) agent: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) cwd: Option<String>,
}

/// What a key did to the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing to show.
    Nothing,
    /// Something shown changed.
    Changed,
    /// It's to close.
    Close,
    /// The sessions the agent called `agent` kept for `cwd` are to be
    /// asked for, to list: see [`StartPage::listing`].
    List { agent: String, cwd: String },
    /// It's to close, starting a new chat.
    Start(NewChat),
    /// It's to close, showing this chat, which is in no pane of this
    /// workspace's, or going to the pane it's in.
    Show(TerminalId),
    /// It's to close, going on with `session`, which the agent called
    /// `agent` kept, in a new chat.
    Resume {
        agent: String,
        session: AgentSession,
    },
}

/// One of the new chat's pickers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Picker {
    Agent,
    Model,
    Folder,
}

/// A chat or session to go on with, as the list has it.
#[derive(Clone, Debug)]
enum Entry {
    Chat(ChatSummary),
    Session(AgentSession),
}

/// The start page's state.
#[derive(Debug)]
pub(crate) struct StartPage {
    /// Where a chat started or shown goes.
    place: Place,
    /// The agents, once the daemon has said, each with its models.
    agents: Vec<Provider>,
    agent: usize,
    /// The new chat's agent, by its name, until the daemon has said what
    /// agents there are: the last one started, or the `agent` option's.
    /// `None` is the daemon's default.
    first_agent: Option<String>,
    /// The model, by its value: `None` is the agent's own choice.
    model: Option<String>,
    /// The folders to pick from, each made absolute and as it's shown,
    /// once the daemon has said where the focused pane is.
    folders: Vec<(String, String)>,
    folder: usize,
    picker: Picker,
    last: LastChat,
    /// This workspace's chats in no pane.
    chats: Vec<ChatSummary>,
    /// Other workspaces' chats in no pane, which aren't to be moved here.
    others: Vec<TerminalId>,
    /// What to say in place of the count, until the next key: why what
    /// was picked can't be done.
    said: Option<String>,
    /// The agent's sessions, as last listed.
    sessions: Vec<AgentSession>,
    /// The daemon's listing of the agent's sessions, by the window's
    /// number, while it goes on.
    listing: Option<u64>,
    /// Why the agent's sessions aren't listed, if they couldn't be.
    unlisted: Option<String>,
    query: String,
    /// Letters type, as after `/`, rather than act.
    typing: bool,
    /// Each chat's and session's text, which what's typed is matched
    /// against, chats first.
    items: Items,
    entries: Vec<Entry>,
    ranking: Ranking,
    /// The row picked: 0 is the new chat, and the rest the ranked list.
    selected: usize,
    /// The first row shown.
    top: usize,
    /// How many rows the list showed last, for paging.
    rows: usize,
}

impl StartPage {
    /// A page whose chat goes at `place`, starting as `last` did, or else
    /// with the agent called `agent`, if one is named; matching case as
    /// `case` says. It has nothing but the new chat until the daemon says
    /// what there is (see [`choices`](Self::choices)).
    pub(crate) fn new(place: Place, last: LastChat, agent: Option<String>, case: Case) -> Self {
        Self {
            place,
            agents: Vec::new(),
            agent: 0,
            first_agent: last.agent.clone().or(agent),
            model: last.model.clone(),
            folders: Vec::new(),
            folder: 0,
            picker: Picker::Agent,
            last,
            chats: Vec::new(),
            others: Vec::new(),
            said: None,
            sessions: Vec::new(),
            listing: None,
            unlisted: None,
            query: String::new(),
            typing: false,
            items: Items::default(),
            entries: Vec::new(),
            ranking: Ranking::new(case, false),
            selected: 0,
            top: 0,
            rows: 1,
        }
    }

    /// Where a chat started or shown goes.
    pub(crate) fn place(&self) -> Place {
        self.place
    }

    /// Takes in what the daemon says a new chat could be, and `chats`,
    /// this workspace's chats in no pane. Returns what's to be done: the
    /// agent's sessions are to be listed.
    pub(crate) fn choices(&mut self, choices: ChatChoices, chats: Vec<ChatSummary>) -> Outcome {
        let ChatChoices {
            mut agents,
            default,
            cwd,
            home,
            unshown,
        } = choices;
        // Another workspace's: shown here, they'd be taken from there.
        self.others = (unshown.iter())
            .filter(|chat| !chats.iter().any(|ours| ours.chat == chat.chat))
            .map(|chat| chat.chat)
            .collect();
        let name = self.first_agent.clone().unwrap_or(default);
        let known = agents.iter().position(|agent| agent.agent == name);
        // One the daemon doesn't know, as an `agent` option may name:
        // starting it says why it can't start.
        let agent = known.unwrap_or_else(|| {
            agents.insert(
                0,
                Provider {
                    title: name.clone(),
                    agent: name,
                    models: ProviderModels::Unknown,
                },
            );
            0
        });
        self.agents = agents;
        self.agent = agent;
        let mut folders = vec![cwd];
        folders.extend(self.last.cwd.clone());
        folders.push(home.clone());
        let mut seen = Vec::new();
        folders.retain(|folder| {
            let new = !seen.contains(folder);
            seen.push(folder.clone());
            new
        });
        self.folders = (folders.into_iter())
            .map(|folder| {
                let shown = shown_folder(&folder, &home);
                (folder, shown)
            })
            .collect();
        self.folder = 0;
        self.chats = chats;
        self.chats
            .sort_by_key(|chat| std::cmp::Reverse(chat.active));
        self.list_again()
    }

    /// The agent's sessions are listed by the daemon's listing numbered
    /// `id`.
    pub(crate) fn listing(&mut self, id: u64) {
        self.listing = Some(id);
    }

    /// Takes in the agent's sessions, or why there are none, from the
    /// daemon's listing numbered `id`. Returns whether they were the ones
    /// waited for, to draw.
    pub(crate) fn listed(&mut self, id: u64, sessions: Result<Vec<AgentSession>, String>) -> bool {
        if self.listing != Some(id) {
            return false;
        }
        self.listing = None;
        match sessions {
            Ok(sessions) => {
                // A chat in no pane is listed as a chat already.
                let chats: Vec<TerminalId> = self.chats.iter().map(|chat| chat.chat).collect();
                self.sessions = (sessions.into_iter())
                    .filter(|session| session.chat.is_none_or(|chat| !chats.contains(&chat)))
                    .collect();
            }
            Err(why) => self.unlisted = Some(why),
        }
        self.fill();
        true
    }

    /// Lists the agent picked's sessions for the folder picked anew, once
    /// the daemon has said what they could be.
    fn list_again(&mut self) -> Outcome {
        self.sessions.clear();
        self.listing = None;
        self.unlisted = None;
        self.fill();
        let (Some(agent), Some((cwd, _))) =
            (self.agents.get(self.agent), self.folders.get(self.folder))
        else {
            return Outcome::Changed;
        };
        if agent.models == ProviderModels::NotInstalled {
            self.unlisted = Some(format!(
                "{} isn't installed: starting a chat offers to install it",
                agent.title
            ));
            return Outcome::Changed;
        }
        Outcome::List {
            agent: agent.agent.clone(),
            cwd: cwd.clone(),
        }
    }

    /// Lists the chats and sessions again, as they are now.
    fn fill(&mut self) {
        self.items.clear();
        self.entries.clear();
        self.ranking.clear();
        for chat in &self.chats {
            self.items.push(&chat_text(chat));
            self.entries.push(Entry::Chat(chat.clone()));
        }
        for session in &self.sessions {
            self.items
                .push(&session_text(session, jiff::Timestamp::now()));
            self.entries.push(Entry::Session(session.clone()));
        }
        self.ranking.add(&self.items);
        self.selected = self.selected.min(self.ranking.len());
    }

    /// Handles a key.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> Outcome {
        self.said = None;
        let modifiers = press.modifiers;
        // Not with Shift: Ctrl+Shift+C and the like are the terminal's.
        let ctrl = match press.key {
            Key::Character(text) if modifiers.ctrl && !modifiers.alt && !modifiers.shift => {
                text.chars().next().map(|ch| ch.to_ascii_lowercase())
            }
            _ => None,
        };
        // A letter that acts, while nothing's typed.
        let letter = match press.text {
            Some(text) if !self.typing && !modifiers.ctrl && !modifiers.alt => {
                let mut chars = text.chars();
                chars.next().filter(|_| chars.next().is_none())
            }
            _ => None,
        };
        match (press.key, ctrl, letter) {
            (Key::Named(NamedKey::Escape), ..) | (_, Some('c'), _) => self.escape(),
            (Key::Named(NamedKey::Enter), ..) => self.act(),
            (_, _, Some('n')) => self.start(),
            (Key::Named(NamedKey::ArrowDown), ..) | (_, Some('n'), _) | (_, _, Some('j')) => {
                self.go(1)
            }
            (Key::Named(NamedKey::ArrowUp), ..) | (_, Some('p'), _) | (_, _, Some('k')) => {
                self.go(-1)
            }
            (Key::Named(NamedKey::PageDown), ..) => self.go(self.page()),
            (Key::Named(NamedKey::PageUp), ..) => self.go(-self.page()),
            (Key::Named(NamedKey::Tab), ..) if modifiers.shift => self.next_picker(-1),
            (Key::Named(NamedKey::Tab), ..) => self.next_picker(1),
            (Key::Named(NamedKey::ArrowRight), ..) | (_, _, Some('l')) => self.change(1),
            (Key::Named(NamedKey::ArrowLeft), ..) | (_, _, Some('h')) => self.change(-1),
            (_, _, Some('/')) => {
                self.typing = true;
                Outcome::Changed
            }
            (Key::Named(NamedKey::Backspace), ..) => {
                if self.query.pop().is_none() {
                    return Outcome::Nothing;
                }
                self.typed()
            }
            (_, Some('u'), _) => {
                self.query.clear();
                self.typed()
            }
            (_, Some('w'), _) => {
                let kept = (self.query.trim_end().rfind(' ')).map_or(0, |space| space + 1);
                self.query.truncate(kept);
                self.typed()
            }
            _ if modifiers.ctrl || modifiers.alt => Outcome::Nothing,
            _ => match press.text {
                Some(text) => self.paste(text),
                None => Outcome::Nothing,
            },
        }
    }

    /// Types `text`, which starts typing.
    pub(crate) fn paste(&mut self, text: &str) -> Outcome {
        let before = self.query.len();
        (self.query).extend(text.chars().filter(|ch| !ch.is_control()));
        if self.query.len() == before {
            return Outcome::Nothing;
        }
        self.typing = true;
        self.typed()
    }

    /// What's typed changed: the list is matched against it again, and
    /// the best match picked, if anything's typed.
    fn typed(&mut self) -> Outcome {
        self.ranking.set_query(&self.query, &self.items);
        self.top = 0;
        self.selected = usize::from(!self.query.is_empty() && self.ranking.len() > 0);
        Outcome::Changed
    }

    /// Esc: what's typed goes, or else the page.
    fn escape(&mut self) -> Outcome {
        if !self.typing && self.query.is_empty() {
            return Outcome::Close;
        }
        self.typing = false;
        self.query.clear();
        self.typed()
    }

    /// Moves what's picked by `by` rows.
    fn go(&mut self, by: isize) -> Outcome {
        let last = self.ranking.len();
        let selected = self.selected.saturating_add_signed(by).min(last);
        if selected == self.selected {
            return Outcome::Nothing;
        }
        self.selected = selected;
        Outcome::Changed
    }

    /// Moves what's picked by `by` rows, as the mouse's wheel does.
    pub(crate) fn scroll(&mut self, by: isize) -> Outcome {
        self.go(by)
    }

    /// Picks the row shown at `row`, as a click does, and acts on it if
    /// `open`, as a double click does.
    pub(crate) fn click(&mut self, row: usize, open: bool) -> Outcome {
        let picked = self.top + row;
        if picked > self.ranking.len() {
            return Outcome::Nothing;
        }
        self.selected = picked;
        if open {
            return self.act();
        }
        Outcome::Changed
    }

    /// How many rows a page is.
    fn page(&self) -> isize {
        isize::try_from(self.rows.max(1)).unwrap_or(1)
    }

    /// Moves to the new chat's picker `by` along, and picks the new chat.
    fn next_picker(&mut self, by: isize) -> Outcome {
        let pickers = [Picker::Agent, Picker::Model, Picker::Folder];
        let at = pickers
            .iter()
            .position(|&picker| picker == self.picker)
            .unwrap_or(0);
        self.picker = pickers[step(at, pickers.len(), by)];
        self.selected = 0;
        Outcome::Changed
    }

    /// Changes the picker's choice `by` along, wrapping round: a new agent
    /// or folder lists its sessions anew.
    fn change(&mut self, by: isize) -> Outcome {
        self.selected = 0;
        match self.picker {
            Picker::Agent if self.agents.len() > 1 => {
                self.agent = step(self.agent, self.agents.len(), by);
                // The model last started on goes with its agent.
                let name = &self.agents[self.agent].agent;
                self.model = (self.last.agent.as_ref() == Some(name))
                    .then(|| self.last.model.clone())
                    .flatten();
                self.list_again()
            }
            Picker::Model => {
                let models = self.models();
                let at = (models.iter())
                    .position(|(value, _)| *value == self.model)
                    .unwrap_or(0);
                self.model.clone_from(&models[step(at, models.len(), by)].0);
                Outcome::Changed
            }
            Picker::Folder if self.folders.len() > 1 => {
                self.folder = step(self.folder, self.folders.len(), by);
                self.list_again()
            }
            Picker::Agent | Picker::Folder => Outcome::Changed,
        }
    }

    /// The models to pick from for the agent picked, each by its value and
    /// as it's shown: first the agent's own choice, then those it offers,
    /// as far as they're known, and the one picked, if it's none of them.
    fn models(&self) -> Vec<(Option<String>, String)> {
        let mut models = vec![(None, "its own choice".to_owned())];
        if let Some(Provider {
            models: ProviderModels::Known(choices),
            ..
        }) = self.agents.get(self.agent)
        {
            models.extend(
                (choices.iter()).map(|choice| (Some(choice.value.clone()), choice.name.clone())),
            );
        }
        if let Some(model) = &self.model
            && !models
                .iter()
                .any(|(value, _)| value.as_ref() == Some(model))
        {
            models.push((Some(model.clone()), model.clone()));
        }
        models
    }

    /// The new chat, as the pickers have it.
    fn new_chat(&self) -> NewChat {
        NewChat {
            agent: (self.agents.get(self.agent))
                .map(|agent| agent.agent.clone())
                .or_else(|| self.first_agent.clone()),
            model: self.model.clone(),
            cwd: self.folders.get(self.folder).map(|(cwd, _)| cwd.clone()),
        }
    }

    /// Starts the new chat.
    fn start(&self) -> Outcome {
        Outcome::Start(self.new_chat())
    }

    /// Enter: starts the new chat, or shows or goes on with the chat or
    /// session picked.
    fn act(&mut self) -> Outcome {
        let Some(rank) = self.selected.checked_sub(1) else {
            return self.start();
        };
        let Some(index) = self.ranking.get(rank) else {
            return self.start();
        };
        match &self.entries[index] {
            Entry::Chat(chat) => Outcome::Show(chat.chat),
            Entry::Session(session) => match session.chat {
                Some(chat) if self.others.contains(&chat) => {
                    self.said = Some(format!(
                        "chat {chat} has it open, in another workspace: go there to see it"
                    ));
                    Outcome::Changed
                }
                Some(chat) => Outcome::Show(chat),
                None => Outcome::Resume {
                    agent: self.new_chat().agent.unwrap_or_default(),
                    session: session.clone(),
                },
            },
        }
    }

    /// What to draw, for a list `rows` rows tall and `cols` cells wide,
    /// and a preview of so many lines and cells, if there's room for one.
    pub(crate) fn view(
        &mut self,
        (rows, cols): (usize, usize),
        preview: Option<(usize, usize)>,
    ) -> FinderView {
        let rows = rows.max(1);
        self.rows = rows;
        let count = self.ranking.len() + 1;
        self.selected = self.selected.min(count - 1);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + rows {
            self.top = self.selected + 1 - rows;
        }
        let now = jiff::Timestamp::now();
        let shown: Vec<Row> = (self.top..count.min(self.top + rows))
            .filter_map(|row| match row {
                0 => Some(self.new_row(cols)),
                row => self.row(row - 1, cols),
            })
            .collect();
        let preview = preview.map(|size| self.preview(size, now));
        let query = if self.typing || !self.query.is_empty() {
            format!("{}▏", self.query)
        } else {
            String::new()
        };
        let count = match (&self.unlisted, self.listing) {
            _ if self.said.is_some() => self.said.clone().unwrap_or_default(),
            (Some(why), _) => why.clone(),
            (None, Some(_)) => format!("{} / {}…", self.ranking.len(), self.items.len()),
            (None, None) => format!("{} / {}", self.ranking.len(), self.items.len()),
        };
        FinderView {
            title: "Agent",
            place: self
                .folders
                .get(self.folder)
                .map(|(_, shown)| shown.clone()),
            query,
            count,
            selected: Some(self.selected - self.top),
            rows: shown,
            preview,
            empty: None,
        }
    }

    /// The new chat's row: its agent, model and folder, the picker
    /// moved to picked out.
    fn new_row(&self, cols: usize) -> Row {
        let mut text = String::new();
        let mut inks = Vec::new();
        let mut add = |piece: &str, ink: Ink| {
            text.push_str(piece);
            inks.extend(std::iter::repeat_n(ink, piece.chars().count()));
        };
        add("New chat", Ink::Bold);
        for (picker, value) in self.pickers() {
            add(" · ", Ink::Muted);
            let ink = if picker == self.picker {
                Ink::Accent
            } else {
                Ink::Plain
            };
            add(&value, ink);
        }
        fit(&text, &inks, cols, false)
    }

    /// Each picker, and its choice as it's shown.
    fn pickers(&self) -> [(Picker, String); 3] {
        let agent = match self.agents.get(self.agent) {
            Some(agent) => agent.title.clone(),
            None => (self.first_agent.clone()).unwrap_or_else(|| "the default agent".to_owned()),
        };
        let model = (self.models().into_iter())
            .find(|(value, _)| *value == self.model)
            .map_or_else(|| "its own choice".to_owned(), |(_, name)| name);
        let folder = match self.folders.get(self.folder) {
            Some((_, shown)) => shown.clone(),
            None => "the pane's folder".to_owned(),
        };
        [
            (Picker::Agent, agent),
            (Picker::Model, model),
            (Picker::Folder, folder),
        ]
    }

    /// The row for the chat or session ranked `rank`, `cols` cells wide.
    fn row(&mut self, rank: usize, cols: usize) -> Option<Row> {
        let index = self.ranking.get(rank)?;
        let text = self.items.get(index);
        let accents = self.ranking.matched_chars(text);
        let title_chars = match &self.entries[index] {
            Entry::Chat(chat) => title_of(&chat.title).chars().count(),
            Entry::Session(session) => title_of(&session.title).chars().count(),
        };
        let inks: Vec<Ink> = (0..text.chars().count())
            .map(|at| {
                let at_u32 = u32::try_from(at).unwrap_or(u32::MAX);
                if accents.binary_search(&at_u32).is_ok() {
                    Ink::Matched
                } else if at < title_chars {
                    Ink::Plain
                } else {
                    Ink::Muted
                }
            })
            .collect();
        Some(fit(text, &inks, cols, false))
    }

    /// The preview of the row picked, `rows` lines tall and `cols` wide.
    fn preview(&mut self, (rows, cols): (usize, usize), now: jiff::Timestamp) -> PreviewView {
        let entry = (self.selected.checked_sub(1))
            .and_then(|rank| self.ranking.get(rank))
            .and_then(|index| self.entries.get(index));
        let mut lines: Vec<(String, Ink)> = Vec::new();
        match entry {
            None => {
                for ((picker, value), label) in
                    self.pickers().into_iter().zip(["Agent", "Model", "Folder"])
                {
                    let ink = if picker == self.picker {
                        Ink::Accent
                    } else {
                        Ink::Plain
                    };
                    lines.push((format!("{label:<7} {value}"), ink));
                }
                lines.push((String::new(), Ink::Plain));
                lines.push(("Enter or n starts it.".to_owned(), Ink::Muted));
                lines.push((
                    "Tab picks its agent, model or folder; h and l change it.".to_owned(),
                    Ink::Muted,
                ));
                lines.push((
                    "/ filters the chats and sessions below.".to_owned(),
                    Ink::Muted,
                ));
            }
            Some(Entry::Chat(chat)) => {
                lines.push((title_of(&chat.title).to_owned(), Ink::Bold));
                lines.push((
                    format!("{}, {}", chat.agent, state_words(&chat.state)),
                    Ink::Plain,
                ));
                lines.push((String::new(), Ink::Plain));
                lines.push((
                    "In no pane, as in the background. Enter shows it.".to_owned(),
                    Ink::Muted,
                ));
            }
            Some(Entry::Session(session)) => {
                lines.push((title_of(&session.title).to_owned(), Ink::Bold));
                let when = (session.updated.as_deref())
                    .and_then(|updated| ago(updated, now))
                    .unwrap_or_else(|| "when isn't known".to_owned());
                lines.push((when, Ink::Plain));
                lines.push((session.cwd.clone(), Ink::Muted));
                lines.push((String::new(), Ink::Plain));
                let what = match session.chat {
                    Some(chat) => format!("Open in chat {chat}: Enter goes to it."),
                    None => "Enter goes on with it in a new chat.".to_owned(),
                };
                lines.push((what, Ink::Muted));
            }
        }
        PreviewView {
            lines: (lines.iter().take(rows))
                .map(|(text, ink)| {
                    let inks = vec![*ink; text.chars().count()];
                    fit(text, &inks, cols, false)
                })
                .collect(),
            at: None,
        }
    }
}

/// The place `by` along from `at`, of `len`, wrapping round at either end.
fn step(at: usize, len: usize, by: isize) -> usize {
    let len = isize::try_from(len).unwrap_or(1).max(1);
    let at = isize::try_from(at).unwrap_or(0);
    usize::try_from((at + by).rem_euclid(len)).unwrap_or(0)
}

/// A title as shown, or a word for none.
fn title_of(title: &str) -> &str {
    if title.trim().is_empty() {
        "Untitled"
    } else {
        title
    }
}

/// A chat's text, as it's matched and shown: its title, then its agent
/// and what it's doing.
fn chat_text(chat: &ChatSummary) -> String {
    format!(
        "{}  {}, {}",
        title_of(&chat.title),
        chat.agent,
        state_words(&chat.state)
    )
}

/// A session's text, as it's matched and shown: its title, then how long
/// ago it changed, as it was `now`, and whether a chat has it open.
fn session_text(session: &AgentSession, now: jiff::Timestamp) -> String {
    let mut text = title_of(&session.title).to_owned();
    if let Some(ago) = session
        .updated
        .as_deref()
        .and_then(|updated| ago(updated, now))
    {
        text.push_str("  ");
        text.push_str(&ago);
    }
    if session.chat.is_some() {
        text.push_str(", open");
    }
    text
}

/// What a chat is doing, in words.
fn state_words(state: &ChatState) -> &'static str {
    match state {
        ChatState::Starting => "starting",
        ChatState::Ready => "waiting",
        ChatState::Answering => "working",
        ChatState::Failed(_) => "failed",
    }
}

/// How long before `now` the time `updated` was, in RFC 3339, in words:
/// "5 min ago", "3 h ago", "2 days ago", or past a month the date.
fn ago(updated: &str, now: jiff::Timestamp) -> Option<String> {
    let then: jiff::Timestamp = updated.parse().ok()?;
    let seconds = now.duration_since(then).as_secs().max(0);
    Some(match seconds {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        86_400..172_800 => "a day ago".to_owned(),
        172_800..2_592_000 => format!("{} days ago", seconds / 86_400),
        _ => then.strftime("%Y-%m-%d").to_string(),
    })
}

/// `folder` as people write it: under `home`, from `~`.
fn shown_folder(folder: &str, home: &str) -> String {
    match folder.strip_prefix(home) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => folder.to_owned(),
    }
}

#[cfg(test)]
mod tests;
