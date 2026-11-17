//! The settings menu's MCP Servers page: the servers clackatoo connects
//! chats to, as the daemon lists them for the project Settings opened in.
//!
//! Its list has a row for each server, saying how it's connected, then
//! what else there is to do: keep a private credential, or copy servers
//! over from an agent's own settings. A server opens as a form (see
//! [`server`]): how it's connected and what can be done about that, its
//! fields, and its tools. A credential's value, and the address a sign in
//! ends on, are drawn as dots and never said back.
//!
//! The list is read as the page is shown and every couple of seconds
//! after, as chats connect and sign ins finish by themselves. A read
//! never takes away what an action came to, nor anything typed. Each
//! thing asked of the daemon has a number, so an answer to something no
//! longer waited for, as after the connection was lost, changes nothing.

mod server;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clackatoo_protocol::{McpAction, McpImport, McpReply, McpServerInfo, McpState};

use self::server::{Field, ServerForm};
use super::form::{self, Catalogue, Did, FormView, Listing, MOST_TEXT, Notice, Text, Typing};
use super::view::{ButtonView, ControlView, FieldView, Look, Part, RowView};
use super::{Outcome, SettingsMenu};
use crate::render::Ink;

/// How long after the list was read it's read again, while its page is
/// shown.
const READ_EVERY: Duration = Duration::from_secs(2);

/// What numbers the next thing asked of a daemon, in any window's menu.
static ASKED: AtomicU64 = AtomicU64::new(1);

/// An agent whose own servers can be copied over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    Claude,
    Codex,
}

impl Provider {
    /// What the daemon knows it as.
    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// What it's called.
    fn title(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    /// The one the daemon knows as `name`.
    fn named(name: &str) -> Option<Self> {
        [Self::Claude, Self::Codex]
            .into_iter()
            .find(|provider| provider.name() == name)
    }
}

/// A row of the page.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// A server in the list, by its name.
    Server(String),
    /// Adds a private credential.
    Credential,
    /// Copies servers over from an agent's own settings.
    Import(Provider),
    /// In a server's form: how it's connected.
    Status,
    /// Allows a server from the project or a package to run.
    Review,
    Test,
    Reconnect,
    SignIn,
    /// Finishes a sign in whose browser was on another machine.
    FinishSignIn,
    ForgetLogin,
    /// One of the server's fields.
    Field(Field),
    /// One of its tools, by its place.
    Tool(usize),
    Remove,
    /// In a credential's form: its name, and its value.
    CredentialName,
    CredentialValue,
    /// The address a sign in's browser ended on.
    CallbackUrl,
    /// A server that can be copied over, by its place.
    Candidate(usize),
}

/// What the page shows.
#[derive(Debug)]
enum View {
    List,
    Server(Box<ServerForm>),
    Credential {
        name: String,
        value: String,
    },
    /// Finishing server `form`'s sign in, which its form comes back to.
    Callback {
        form: Box<ServerForm>,
        url: String,
    },
    Import {
        provider: Provider,
        candidates: Vec<McpImport>,
        /// Those picked, by their place.
        picked: Vec<bool>,
    },
}

/// Something asked of the daemon, not yet answered.
struct Asked {
    number: u64,
    action: McpAction,
    /// The form it was asked from is still the one shown: its answer
    /// closes that, and no other.
    from_form: bool,
}

/// A server's last test from here, which the daemon doesn't keep: shown
/// while the server is as it was tested and no chat has connected to it.
#[derive(Debug)]
struct Tested {
    revision: String,
    state: McpState,
    detail: String,
    tools: Vec<clackatoo_protocol::McpToolInfo>,
}

/// The MCP Servers page's state.
pub(super) struct Mcp {
    /// The servers, by name, as the daemon last listed them.
    servers: Vec<McpServerInfo>,
    listing: Listing,
    /// What's wrong with the servers' files, as the daemon says with its
    /// list.
    problems: String,
    view: View,
    typing: Option<Typing>,
    /// The read of the list asked for, by its number.
    reading: Option<u64>,
    asked: Option<Asked>,
    /// When the list is to be read next.
    due: Option<Instant>,
    notice: Option<Notice>,
    /// The servers signed in to from here, each with why it failed, if
    /// it did: their rows say how it went.
    sign_ins: BTreeMap<String, Option<String>>,
    tested: BTreeMap<String, Tested>,
    /// The button the keys are on in a server's row of the list, if not
    /// its last: the server's name, and the button's place.
    button: Option<(String, usize)>,
    /// A credential's form says where it's kept: the daemon's machine, if
    /// it isn't this one.
    pub(super) machine: Option<String>,
    /// The home directory on the daemon's machine, which files' paths are
    /// said from, once the daemon's said it.
    pub(super) home: String,
}

impl std::fmt::Debug for Mcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never a credential's value, nor an address that signs in.
        f.debug_struct("Mcp")
            .field("servers", &self.servers.len())
            .field("listing", &self.listing)
            .field("asked", &self.asked.is_some())
            .finish_non_exhaustive()
    }
}

impl Mcp {
    pub(super) fn new() -> Self {
        Self {
            servers: Vec::new(),
            listing: Listing::Reading,
            problems: String::new(),
            view: View::List,
            typing: None,
            reading: None,
            asked: None,
            due: Some(Instant::now()),
            notice: None,
            sign_ins: BTreeMap::new(),
            tested: BTreeMap::new(),
            button: None,
            machine: None,
            home: String::new(),
        }
    }

    /// The servers listed, by name.
    pub(super) fn servers(&self) -> &[McpServerInfo] {
        &self.servers
    }

    /// What the page knows of its list.
    pub(super) fn listing(&self) -> &Listing {
        &self.listing
    }

    /// What the last action came to.
    pub(super) fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// What's wrong with the servers' files, if anything is.
    pub(super) fn problems(&self) -> Option<&str> {
        (!self.problems.is_empty()).then_some(&*self.problems)
    }

    /// When the list is next to be read, if it's to be and isn't being.
    pub(super) fn due(&self) -> Option<Instant> {
        self.due.filter(|_| self.reading.is_none())
    }

    /// The list is to be read now, as its page is shown, or the
    /// connection's back: unless it's being read, which is as good.
    pub(super) fn read_now(&mut self) {
        if self.reading.is_none() {
            self.due = Some(Instant::now());
        }
    }

    /// The list is being read: the number its answer is known by.
    pub(super) fn reading(&mut self) -> u64 {
        let number = ASKED.fetch_add(1, Ordering::Relaxed);
        self.reading = Some(number);
        self.due = None;
        number
    }

    /// The list can't be read, and `why`: the page says so until it can.
    pub(super) fn unavailable(&mut self, why: String) {
        self.reading = None;
        self.due = None;
        self.listing = Listing::Unavailable(why);
    }

    /// The connection was lost: what was asked will never be answered,
    /// and nothing's read until it's back.
    pub(super) fn lost(&mut self) {
        self.reading = None;
        self.due = None;
        if self.asked.take().is_some() {
            self.notice = Some(Notice::Failed(super::LOST_ASKING.into()));
        }
    }

    /// The server called `name`, as listed.
    fn server(&self, name: &str) -> Option<&McpServerInfo> {
        self.servers.iter().find(|server| server.name == name)
    }

    /// Whether `action` is what's been asked and not answered.
    fn asking(&self, is: impl Fn(&McpAction) -> bool) -> bool {
        self.asked.as_ref().is_some_and(|asked| is(&asked.action))
    }

    /// Asks the daemon for `action`, saying `working` meanwhile. One
    /// thing is asked at a time: while one waits, nothing more is.
    fn ask(&mut self, action: McpAction, working: &str) -> Did {
        if self.asked.is_some() {
            return Did::Nothing;
        }
        if let McpAction::Authenticate { name } = &action {
            self.sign_ins.insert(name.clone(), None);
        }
        let number = ASKED.fetch_add(1, Ordering::Relaxed);
        self.notice = Some(Notice::Working(working.to_owned()));
        self.asked = Some(Asked {
            number,
            action: action.clone(),
            from_form: !matches!(self.view, View::List),
        });
        Did::Outcome(Outcome::Mcp { action, id: number })
    }

    /// Takes in the daemon's answer numbered `number`. Returns a sign in's
    /// address, to open in the browser, and the row the keys go to, if
    /// the page shows something else for it.
    pub(super) fn came(
        &mut self,
        number: u64,
        result: Result<McpReply, String>,
    ) -> (Option<String>, Option<usize>) {
        if self.reading == Some(number) {
            self.reading = None;
            self.due = Some(Instant::now() + READ_EVERY);
            match result {
                Ok(reply) => {
                    self.problems = reply.notice;
                    self.listed(reply.servers);
                }
                Err(why) => self.listing = Listing::Unavailable(why),
            }
            return (None, None);
        }
        let Some(asked) = self.asked.take_if(|asked| asked.number == number) else {
            // An answer nothing waits for any more.
            return (None, None);
        };
        // Whatever it did, the list is read again: it says what the
        // servers' files are like now.
        self.due = Some(Instant::now());
        match result {
            Ok(reply) => self.answered(asked, reply),
            Err(why) => {
                if let McpAction::Authenticate { name } = &asked.action {
                    self.sign_ins.insert(name.clone(), Some(why.clone()));
                }
                self.notice = Some(Notice::Failed(why));
                (None, None)
            }
        }
    }

    /// Takes in the servers as the daemon lists them, and what a test
    /// from here found of each that's still as it was tested.
    fn listed(&mut self, mut servers: Vec<McpServerInfo>) {
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        for server in &mut servers {
            let Some(tested) = self.tested.get(&server.name) else {
                continue;
            };
            let untouched =
                tested.revision == server.revision && server.state == McpState::Disconnected;
            if untouched {
                server.state = tested.state;
                server.detail.clone_from(&tested.detail);
                server.tools.clone_from(&tested.tools);
            } else if tested.revision != server.revision {
                self.tested.remove(&server.name);
            }
        }
        self.servers = servers;
        self.listing = Listing::Ready;
        // A server's form follows how it's connected; one whose server
        // has gone goes back to the list.
        if let View::Server(form) = &mut self.view
            && let Some(original) = &form.original
        {
            match self
                .servers
                .iter()
                .find(|server| server.name == original.name)
            {
                Some(server) => form.follow(server),
                None if !form.changed() => self.view = View::List,
                None => {}
            }
        }
    }

    /// Takes in what `asked` came to, the daemon having done it.
    fn answered(&mut self, asked: Asked, reply: McpReply) -> (Option<String>, Option<usize>) {
        let Asked {
            action, from_form, ..
        } = asked;
        let mut went = None;
        match action {
            McpAction::List => {}
            McpAction::Save { name, .. } => {
                self.listed(reply.servers);
                self.notice = Some(Notice::Done(format!(
                    "Saved {name}. Chats started from now on use it as it is now."
                )));
                went = self.back_to_list(from_form, Some(&name));
            }
            McpAction::Remove { name, .. } => {
                self.listed(reply.servers);
                self.notice = Some(Notice::Done(format!("Removed {name}.")));
                went = self.back_to_list(from_form, None);
            }
            McpAction::Secret { name, value } => {
                self.listed(reply.servers);
                self.notice = Some(Notice::Done(if value.is_empty() {
                    format!("Removed the private credential {name}.")
                } else {
                    format!("Saved the private credential {name}.")
                }));
                went = self.back_to_list(from_form, None);
            }
            McpAction::Import { provider, names } if names.is_empty() => {
                self.notice = None;
                if let Some(provider) = Provider::named(&provider) {
                    self.view = View::Import {
                        provider,
                        picked: vec![false; reply.imports.len()],
                        candidates: reply.imports,
                    };
                    self.typing = None;
                    went = Some(0);
                }
            }
            McpAction::Import { names, .. } => {
                self.listed(reply.servers);
                self.notice = Some(Notice::Done(format!(
                    "Copied {} over. The agent's own settings are as they were.",
                    form::counted(names.len(), "server", "servers")
                )));
                went = self.back_to_list(from_form, names.first().map(String::as_str));
            }
            McpAction::Test { name } | McpAction::Reconnect { name } => {
                // The daemon answers with the one server, as the test left it.
                let found = reply.servers.into_iter().find(|server| server.name == name);
                if let Some(server) = found {
                    self.tested(server);
                }
            }
            McpAction::Trust { name, .. } => {
                self.listed(reply.servers);
                self.notice = Some(Notice::Done(format!(
                    "{name} is enabled on this machine, as it is now."
                )));
            }
            McpAction::ForgetAuth { name } => {
                self.listed(reply.servers);
                self.sign_ins.remove(&name);
                self.notice = Some(Notice::Done(format!("Forgot the sign in for {name}.")));
            }
            McpAction::Authenticate { name } => {
                // One that never got as far as the browser failed.
                let Some(url) = reply.login_url else {
                    self.sign_ins.insert(name, Some(reply.notice.clone()));
                    self.notice = Some(Notice::Failed(reply.notice));
                    return (None, None);
                };
                self.notice = Some(Notice::Done(format!(
                    "Finish signing in to {name} in your browser."
                )));
                return (Some(url), None);
            }
            McpAction::CompleteLogin { name, .. } => {
                self.notice = Some(Notice::Done(format!("Signed in to {name}.")));
                if from_form && let View::Callback { .. } = &self.view {
                    let View::Callback { form, .. } = std::mem::replace(&mut self.view, View::List)
                    else {
                        unreachable!("it was just seen")
                    };
                    self.view = View::Server(form);
                    self.typing = None;
                    went = Some(0);
                }
            }
        }
        (None, went)
    }

    /// Takes in `server` as a test from here left it: how it went is
    /// said, and kept to show while the server is as it was tested, since
    /// the daemon keeps none of it; the other servers stay as listed.
    fn tested(&mut self, server: McpServerInfo) {
        let name = server.name.clone();
        self.notice = Some(match server.state {
            McpState::Ready => Notice::Done(format!(
                "{name} answered, with {}.",
                form::counted(server.tools.len(), "tool", "tools")
            )),
            McpState::NeedsLogin => {
                Notice::Done(format!("{name} answered that it needs a sign in."))
            }
            _ => Notice::Failed(format!("{name} didn't answer: {}", server.detail)),
        });
        self.tested.insert(
            name.clone(),
            Tested {
                revision: server.revision.clone(),
                state: server.state,
                detail: server.detail.clone(),
                tools: server.tools.clone(),
            },
        );
        let mut servers = std::mem::take(&mut self.servers);
        servers.retain(|each| each.name != name);
        servers.push(server);
        self.listed(servers);
    }

    /// Shows the list again, if the form that asked for something is
    /// still the one shown, `from_form`. Returns the row the keys go to
    /// then: server `name`'s, if it's given and listed.
    fn back_to_list(&mut self, from_form: bool, name: Option<&str>) -> Option<usize> {
        if !from_form {
            return None;
        }
        self.view = View::List;
        self.typing = None;
        Some(name.and_then(|name| self.place(name)).unwrap_or(0))
    }

    /// The sections shown, each its heading, a sentence under it, and its
    /// rows.
    fn laid(&self) -> Vec<(&'static str, Option<String>, Vec<Row>)> {
        match &self.view {
            View::List => {
                if self.listing != Listing::Ready {
                    return Vec::new();
                }
                let servers: Vec<Row> = (self.servers.iter())
                    .map(|server| Row::Server(server.name.clone()))
                    .collect();
                let none = servers.is_empty().then(|| {
                    "No servers yet. Add one, or copy over the ones Claude Code or Codex \
                     already use."
                        .to_owned()
                });
                vec![
                    ("Servers", none, servers),
                    (
                        "More",
                        None,
                        vec![
                            Row::Credential,
                            Row::Import(Provider::Claude),
                            Row::Import(Provider::Codex),
                        ],
                    ),
                ]
            }
            View::Server(form) => self.server_laid(form),
            View::Credential { .. } => {
                vec![("", None, vec![Row::CredentialName, Row::CredentialValue])]
            }
            View::Callback { .. } => vec![("", None, vec![Row::CallbackUrl])],
            View::Import {
                provider,
                candidates,
                ..
            } => {
                let none = candidates
                    .is_empty()
                    .then(|| format!("{} has no servers of its own to copy.", provider.title()));
                vec![(
                    "Servers to copy",
                    none,
                    (0..candidates.len()).map(Row::Candidate).collect(),
                )]
            }
        }
    }

    /// The sections of a server's form: what can be done about how it's
    /// connected, by how it's reached and how it is now; its fields; its
    /// tools; and removing it, if it's the user's to remove.
    fn server_laid(&self, form: &ServerForm) -> Vec<(&'static str, Option<String>, Vec<Row>)> {
        let fields: Vec<Row> = form.fields().into_iter().map(Row::Field).collect();
        let Some(server) = &form.original else {
            return vec![("", None, fields)];
        };
        let mut connection = vec![Row::Status];
        let http = server::over_http(server);
        if !server.trusted {
            connection.push(Row::Review);
        } else if server.enabled {
            connection.push(Row::Test);
            let live = matches!(
                server.state,
                McpState::Connected | McpState::Starting | McpState::Error
            );
            if live {
                connection.push(Row::Reconnect);
            }
            if http {
                if self.sign_in_button(server).is_some() || server::signs_in(server) {
                    connection.push(Row::SignIn);
                }
                if server.signing_in {
                    connection.push(Row::FinishSignIn);
                }
                if server.state != McpState::NeedsLogin && !server.signing_in {
                    connection.push(Row::ForgetLogin);
                }
            }
        }
        let mut sections = vec![("Connection", None, connection), ("Server", None, fields)];
        if !server.tools.is_empty() {
            sections.push((
                "Tools",
                None,
                (0..server.tools.len()).map(Row::Tool).collect(),
            ));
        }
        if server.editable {
            sections.push(("Remove", None, vec![Row::Remove]));
        }
        sections
    }

    /// Every row shown, in order.
    fn rows(&self) -> Vec<Row> {
        (self.laid().into_iter())
            .flat_map(|(_, _, rows)| rows)
            .collect()
    }

    /// Where server `name`'s row is in the list.
    fn place(&self, name: &str) -> Option<usize> {
        (self.rows().iter()).position(|row| matches!(row, Row::Server(each) if each == name))
    }

    /// What server `server`'s Sign in button says, and whether it acts:
    /// none unless it needs a sign in, or one from here is going or done.
    fn sign_in_button(&self, server: &McpServerInfo) -> Option<ButtonView> {
        let asked = self.asking(
            |action| matches!(action, McpAction::Authenticate { name } if *name == server.name),
        );
        if server.signing_in || asked {
            Some(ButtonView::plain("Signing in…").disabled())
        } else if server.state == McpState::NeedsLogin {
            Some(ButtonView::main("Sign in"))
        } else if self.sign_ins.get(&server.name) == Some(&None)
            && matches!(server.state, McpState::Ready | McpState::Connected)
        {
            Some(ButtonView::plain("Signed in").disabled())
        } else {
            None
        }
    }

    /// Why signing in to `server` from here failed, if it did and it
    /// still needs a sign in.
    fn sign_in_failed(&self, server: &McpServerInfo) -> Option<String> {
        if server.signing_in || server.state != McpState::NeedsLogin {
            return None;
        }
        match self.sign_ins.get(&server.name)? {
            Some(why) if !why.is_empty() => Some(why.clone()),
            // Failed in the browser, which the daemon says.
            _ => (!server.detail.is_empty()).then(|| server.detail.clone()),
        }
    }

    /// The buttons of server `server`'s row in the list: Sign in, if it
    /// has one, then Open.
    fn list_buttons(&self, server: &McpServerInfo) -> Vec<ButtonView> {
        (self.sign_in_button(server).into_iter())
            .chain([ButtonView::plain("Open")])
            .collect()
    }

    /// The button the keys are on, of `server`'s row in the list: Open,
    /// its last, unless they've been moved.
    fn button_on(&self, server: &McpServerInfo) -> usize {
        let buttons = self.list_buttons(server);
        let last = buttons.len() - 1;
        match &self.button {
            // One that can't be pressed any more isn't where the keys stay.
            Some((name, at))
                if *name == server.name && buttons.get(*at).is_some_and(|b| b.enabled) =>
            {
                *at
            }
            _ => last,
        }
    }

    /// Opens server `name`'s form.
    fn open(&mut self, name: &str) -> Did {
        let Some(server) = self.server(name).cloned() else {
            return Did::Nothing;
        };
        self.view = View::Server(Box::new(ServerForm::new(Some(server))));
        self.typing = None;
        self.notice = None;
        // To the first thing there is to do: review it, or test it.
        let first = (self.drawn().iter()).position(|row| row.control != ControlView::None);
        Did::Went(first.unwrap_or(0))
    }

    /// Opens server `name`'s form, as a search that found it does.
    pub(super) fn open_named(&mut self, name: &str) -> Option<usize> {
        match self.open(name) {
            Did::Went(row) => Some(row),
            _ => None,
        }
    }

    /// A row of the list, as it's drawn.
    fn list_row(&self, row: &Row) -> RowView {
        match row {
            Row::Server(name) => {
                let Some(server) = self.server(name) else {
                    return RowView::new(name.as_str(), "", ControlView::None);
                };
                let (state, ink) = server::state_said(server);
                // Where it's from, its tools once it's said them, and what
                // it runs or where it is.
                let tools = (!server.tools.is_empty())
                    .then(|| form::counted(server.tools.len(), "tool", "tools"));
                let help = [Some(server::source_said(server)), tools]
                    .into_iter()
                    .flatten()
                    .chain([server::reaches(server)])
                    .collect::<Vec<_>>()
                    .join(" · ");
                let buttons = self.list_buttons(server);
                let control = ControlView::Buttons {
                    at: self.button_on(server),
                    buttons,
                };
                let view = RowView::new(name.as_str(), help, control).with_status(state, ink);
                if let Some(failed) = self.sign_in_failed(server) {
                    view.noted(failed, Ink::Warning)
                } else if server.detail.is_empty() || !server.trusted {
                    // What needs review says so beside its button.
                    view
                } else if server.state == McpState::Error {
                    view.noted(&server.detail, Ink::Warning)
                } else {
                    view.noted(&server.detail, Ink::Muted)
                }
            }
            Row::Credential => RowView::new(
                "Private credential",
                format!(
                    "A token or password a server needs, kept {}, out of the project's files. A \
                     server's environment or headers name it.",
                    self.kept_where()
                ),
                ControlView::Button(ButtonView::plain("Add")),
            ),
            Row::Import(provider) => {
                let asked = self.asking(|action| {
                    matches!(action, McpAction::Import { provider: name, .. } if name == provider.name())
                });
                RowView::new(
                    format!("Copy servers from {}", provider.title()),
                    "Pick which of its own servers to add here. Its settings aren't changed.",
                    ControlView::Button(server::asking("Choose", "Reading…", asked, Look::Plain)),
                )
            }
            _ => RowView::new("", "", ControlView::None),
        }
    }

    /// Where private credentials are kept, as a row says it.
    fn kept_where(&self) -> String {
        match &self.machine {
            Some(machine) => format!("on {machine}"),
            None => "on this machine".into(),
        }
    }

    /// A row of a server's form that isn't one of its fields, as it's
    /// drawn.
    #[allow(clippy::too_many_lines, reason = "each row's words")]
    fn server_row(&self, form: &ServerForm, row: &Row) -> RowView {
        let Some(server) = &form.original else {
            return RowView::new("", "", ControlView::None);
        };
        let name = &server.name;
        let button = |button: ButtonView| ControlView::Button(button);
        match row {
            Row::Status => {
                let (state, ink) = server::state_said(server);
                let meaning = if server.detail.is_empty() || !server.trusted {
                    server::state_meaning(server).to_owned()
                } else {
                    server.detail.clone()
                };
                let view =
                    RowView::new("Connection", meaning, ControlView::None).with_status(state, ink);
                match self.sign_in_failed(server) {
                    Some(failed) if failed != server.detail => view.noted(failed, Ink::Warning),
                    _ => view,
                }
            }
            Row::Review => RowView::new(
                "Review and enable",
                "It comes from the project or a package, so it runs nothing until you allow it. \
                 Check what it runs, below, then enable it on this machine as it is now.",
                button(server::asking(
                    "Enable",
                    "Enabling…",
                    self.asking(|action| matches!(action, McpAction::Trust { .. })),
                    Look::Main,
                )),
            ),
            Row::Test => RowView::new(
                "Test",
                "Connects to it and lists its tools, without using any.",
                button(server::asking(
                    "Test",
                    "Testing…",
                    self.asking(|action| matches!(action, McpAction::Test { .. })),
                    Look::Plain,
                )),
            ),
            Row::Reconnect => RowView::new(
                "Reconnect",
                "Drops the chats' connections to it, for them to connect again. A call under \
                 way isn't sent twice.",
                button(server::asking(
                    "Reconnect",
                    "Reconnecting…",
                    self.asking(|action| matches!(action, McpAction::Reconnect { .. })),
                    Look::Plain,
                )),
            ),
            Row::SignIn => RowView::new(
                "Sign in",
                "Opens its sign in in your browser.",
                button(
                    self.sign_in_button(server)
                        .unwrap_or_else(|| ButtonView::plain("Sign in")),
                ),
            ),
            Row::FinishSignIn => RowView::new(
                "Finish a sign in from another machine",
                "If the browser ends on a page that won't load, copy that page's address and \
                 paste it here.",
                button(ButtonView::plain("Paste the address")),
            ),
            Row::ForgetLogin => RowView::new(
                "Forget the sign in",
                format!(
                    "Removes the login saved for {name} {}. Chats using it are cut off until \
                     you sign in again.",
                    self.kept_where()
                ),
                button(if form.forgetting {
                    ButtonView::danger("Really forget")
                } else {
                    ButtonView::plain("Forget")
                }),
            ),
            Row::Tool(at) => match server.tools.get(*at) {
                Some(tool) => RowView::new(
                    tool.name.as_str(),
                    tool.description.as_str(),
                    ControlView::None,
                ),
                None => RowView::new("", "", ControlView::None),
            },
            Row::Remove => RowView::new(
                format!("Remove {name}"),
                format!("Takes it out of {}.", form::tilde(&server.path, &self.home)),
                button(if form.removing {
                    ButtonView::danger("Really remove")
                } else {
                    ButtonView::plain("Remove")
                }),
            ),
            _ => RowView::new("", "", ControlView::None),
        }
    }

    /// Presses a row of a server's form that isn't one of its fields.
    fn press_server(&mut self, row: &Row) -> Did {
        let View::Server(form) = &mut self.view else {
            return Did::Nothing;
        };
        let Some(server) = form.original.clone() else {
            return Did::Nothing;
        };
        let name = server.name.clone();
        // A second press is only of the button pressed first.
        let (removing, forgetting) = (form.removing, form.forgetting);
        form.removing = false;
        form.forgetting = false;
        match row {
            Row::Review => self.ask(
                McpAction::Trust {
                    name,
                    expected: server.revision,
                },
                "Enabling it…",
            ),
            Row::Test => self.ask(McpAction::Test { name }, "Testing it…"),
            Row::Reconnect => self.ask(McpAction::Reconnect { name }, "Reconnecting…"),
            Row::SignIn => self.ask(
                McpAction::Authenticate { name },
                "Opening its sign in in your browser…",
            ),
            Row::FinishSignIn => {
                let View::Server(form) = std::mem::replace(&mut self.view, View::List) else {
                    return Did::Nothing;
                };
                self.view = View::Callback {
                    form,
                    url: String::new(),
                };
                self.typing = None;
                Did::Went(0)
            }
            Row::ForgetLogin if !forgetting => {
                form.forgetting = true;
                self.notice = Some(Notice::Confirm(format!(
                    "Forget the sign in for {name}? Press Really forget."
                )));
                Did::Changed
            }
            Row::ForgetLogin => self.ask(McpAction::ForgetAuth { name }, "Forgetting it…"),
            Row::Remove if !removing => {
                form.removing = true;
                self.notice = Some(Notice::Confirm(format!(
                    "Remove {name}? Press Really remove."
                )));
                Did::Changed
            }
            Row::Remove => self.ask(
                McpAction::Remove {
                    name,
                    expected: server.revision,
                },
                "Removing it…",
            ),
            _ => Did::Nothing,
        }
    }

    /// A row of the form to copy servers over, as it's drawn.
    fn candidate_row(&self, at: usize) -> RowView {
        let View::Import {
            candidates, picked, ..
        } = &self.view
        else {
            return RowView::new("", "", ControlView::None);
        };
        match candidates.get(at) {
            Some(candidate) => RowView::new(
                candidate.name.as_str(),
                candidate.description.as_str(),
                ControlView::Toggle(picked.get(at).copied().unwrap_or(false)),
            ),
            None => RowView::new("", "", ControlView::None),
        }
    }

    /// A box of text that's drawn as dots: `value`, on row `at`.
    fn masked(&self, value: &str, at: usize, hint: &'static str) -> ControlView {
        ControlView::Field(FieldView {
            masked: true,
            wide: true,
            hint,
            ..form::field(value, at, self.typing.as_ref())
        })
    }
}

impl Catalogue for Mcp {
    fn sections(&self) -> Vec<(&'static str, Option<String>, usize)> {
        (self.laid().into_iter())
            .map(|(heading, note, rows)| (heading, note, rows.len()))
            .collect()
    }

    fn drawn(&self) -> Vec<RowView> {
        let typing = self.typing.as_ref();
        (self.rows().iter().enumerate())
            .map(|(at, row)| match (&self.view, row) {
                (View::Server(form), Row::Field(field)) => form.row(*field, at, typing, &self.home),
                (View::Server(form), row) => self.server_row(form, row),
                (View::Credential { name, .. }, Row::CredentialName) => RowView::new(
                    "Name",
                    "What a server's environment or headers call it, as {\"secret\": \"name\"}.",
                    ControlView::Field(FieldView {
                        hint: "docs-token",
                        ..form::field(name, at, typing)
                    }),
                ),
                (View::Credential { value, .. }, Row::CredentialValue) => RowView::new(
                    "Value",
                    "Shown as dots, and never shown again. For an Authorization header, start \
                     it with Bearer and a blank. Left empty, the credential of that name is \
                     removed.",
                    self.masked(value, at, "empty"),
                ),
                (View::Callback { url, .. }, Row::CallbackUrl) => RowView::new(
                    "Address the browser ended on",
                    "All of it, from the address bar. It's checked against this sign in, shown \
                     as dots, and kept nowhere.",
                    self.masked(url, at, "http://127.0.0.1…"),
                ),
                (View::Import { .. }, Row::Candidate(candidate)) => self.candidate_row(*candidate),
                (_, row) => self.list_row(row),
            })
            .collect()
    }

    fn press(&mut self, at: usize, part: Option<Part>) -> Did {
        let Some(row) = self.rows().get(at).cloned() else {
            return Did::Nothing;
        };
        let choice = match part {
            Some(Part::Segment(choice)) => Some(usize::from(choice)),
            _ => None,
        };
        // A press of anything else takes back the first press of a button
        // that asks for a second.
        if let View::Server(form) = &mut self.view
            && !matches!(row, Row::Remove | Row::ForgetLogin)
        {
            form.removing = false;
            form.forgetting = false;
        }
        match row {
            Row::Server(name) => {
                let Some(server) = self.server(&name) else {
                    return Did::Nothing;
                };
                let buttons = self.list_buttons(server);
                let pressed = choice.unwrap_or_else(|| self.button_on(server));
                // Its last button opens it; the one before signs in.
                if pressed + 1 >= buttons.len() {
                    return self.open(&name);
                }
                if !buttons[pressed].enabled {
                    return Did::Nothing;
                }
                self.ask(
                    McpAction::Authenticate { name },
                    "Opening its sign in in your browser…",
                )
            }
            Row::Credential => {
                self.view = View::Credential {
                    name: String::new(),
                    value: String::new(),
                };
                self.typing = None;
                self.notice = None;
                Did::Went(0)
            }
            Row::Import(provider) => self.ask(
                McpAction::Import {
                    provider: provider.name().into(),
                    names: Vec::new(),
                },
                "Reading its servers…",
            ),
            Row::Field(field) => match &mut self.view {
                View::Server(form) => changed(form.press(field, choice)),
                _ => Did::Nothing,
            },
            Row::Candidate(candidate) => {
                if let View::Import { picked, .. } = &mut self.view
                    && let Some(picked) = picked.get_mut(candidate)
                {
                    *picked = !*picked;
                    return Did::Changed;
                }
                Did::Nothing
            }
            row => self.press_server(&row),
        }
    }

    fn step(&mut self, at: usize, by: isize) -> Did {
        let Some(row) = self.rows().get(at).cloned() else {
            return Did::Nothing;
        };
        match row {
            // From one of a server's buttons to the next.
            Row::Server(name) => {
                let Some(server) = self.server(&name) else {
                    return Did::Nothing;
                };
                let last = self.list_buttons(server).len() - 1;
                let on = self.button_on(server);
                let to = on.saturating_add_signed(by).min(last);
                if to == on {
                    return Did::Nothing;
                }
                self.button = Some((name, to));
                Did::Changed
            }
            Row::Field(field) => match &mut self.view {
                View::Server(form) => changed(form.step(field, by)),
                _ => Did::Nothing,
            },
            Row::Candidate(_) => self.press(at, None),
            _ => Did::Nothing,
        }
    }

    fn form(&self) -> Option<FormView> {
        let saving = self.asked.as_ref().is_some_and(|asked| {
            asked.from_form
                && matches!(
                    asked.action,
                    McpAction::Save { .. }
                        | McpAction::Secret { .. }
                        | McpAction::CompleteLogin { .. }
                        | McpAction::Import { .. }
                )
        });
        let save =
            |save: &str, saving_said: &str| if saving { saving_said } else { save }.to_owned();
        Some(match &self.view {
            View::List => return None,
            View::Server(form) => FormView {
                title: match &form.original {
                    Some(server) => server.name.clone(),
                    None => "New server".into(),
                },
                subtitle: match &form.original {
                    Some(_) => "How it's connected, and what it's set up as. A chat started \
                                after it's saved uses it as it is then."
                        .into(),
                    None => "A server to connect chats to: a program on the machine, or one at \
                             an address."
                        .into(),
                },
                changed: form.changed(),
                save: save("Save", "Saving…"),
            },
            View::Credential { name, value } => FormView {
                title: "Private credential".into(),
                subtitle: format!("Kept {}, where only you can read it.", self.kept_where()),
                changed: !name.is_empty() || !value.is_empty(),
                save: save("Save", "Saving…"),
            },
            View::Callback { form, url } => FormView {
                title: format!("Finish signing in to {}", form.name()),
                subtitle: "For a daemon on another machine, whose sign in your browser can't \
                           get back to."
                    .into(),
                changed: !url.is_empty(),
                save: save("Finish", "Finishing…"),
            },
            View::Import {
                provider, picked, ..
            } => FormView {
                title: format!("Copy from {}", provider.title()),
                subtitle: "Each one picked is added to your personal servers, any credentials \
                           it holds kept privately. One whose name you already have isn't \
                           copied."
                    .into(),
                changed: picked.contains(&true),
                save: save("Copy", "Copying…"),
            },
        })
    }

    fn text(&mut self, at: usize) -> Option<Text<'_>> {
        let row = self.rows().get(at).cloned()?;
        match (&mut self.view, row) {
            (View::Server(form), Row::Field(field)) => form.text(field),
            (View::Credential { name, .. }, Row::CredentialName) => Some(Text {
                value: name,
                most: 128,
            }),
            (View::Credential { value, .. }, Row::CredentialValue) => Some(Text {
                value,
                most: 64 * 1024,
            }),
            (View::Callback { url, .. }, Row::CallbackUrl) => Some(Text {
                value: url,
                most: 4 * MOST_TEXT,
            }),
            _ => None,
        }
    }

    fn typing(&self) -> Option<&Typing> {
        self.typing.as_ref()
    }

    fn typing_mut(&mut self) -> &mut Option<Typing> {
        &mut self.typing
    }

    fn left(&mut self, at: usize) {
        if let Some(Row::Field(field)) = self.rows().get(at).cloned()
            && let View::Server(form) = &mut self.view
        {
            form.check(field);
        }
    }

    fn picked(&mut self, _at: usize, _value: &str) {}

    fn save(&mut self) -> Did {
        let fail = |mcp: &mut Self, why: &str| {
            mcp.notice = Some(Notice::Failed(why.to_owned()));
            Did::Changed
        };
        match &mut self.view {
            View::List => Did::Nothing,
            View::Server(form) => match form.action() {
                Ok(action) => self.ask(action, "Saving it…"),
                Err(field) => {
                    self.notice = Some(Notice::Marked);
                    let at = (self.rows().iter()).position(|row| *row == Row::Field(field));
                    Did::Went(at.unwrap_or(0))
                }
            },
            View::Credential { name, value } => {
                let name = name.trim().to_owned();
                if name.is_empty() {
                    return fail(self, "Give the credential a name.");
                }
                let action = McpAction::Secret {
                    name,
                    value: value.clone(),
                };
                self.ask(action, "Saving it…")
            }
            View::Callback { form, url } => {
                let url = url.trim().to_owned();
                if url.is_empty() {
                    return fail(self, "Paste the address the browser ended on.");
                }
                let action = McpAction::CompleteLogin {
                    name: form.name().to_owned(),
                    url,
                };
                self.ask(action, "Finishing the sign in…")
            }
            View::Import {
                provider,
                candidates,
                picked,
            } => {
                let names: Vec<String> = (candidates.iter().zip(picked.iter()))
                    .filter(|(_, picked)| **picked)
                    .map(|(candidate, _)| candidate.name.clone())
                    .collect();
                if names.is_empty() {
                    return fail(self, "Pick at least one server to copy.");
                }
                let action = McpAction::Import {
                    provider: provider.name().into(),
                    names,
                };
                self.ask(action, "Copying them over…")
            }
        }
    }

    fn cancel(&mut self) -> usize {
        self.typing = None;
        self.notice.take_if(|notice| notice.of_form());
        if let Some(asked) = &mut self.asked {
            asked.from_form = false;
        }
        match std::mem::replace(&mut self.view, View::List) {
            // Back to the server whose sign in it was.
            View::Callback { form, .. } => {
                self.view = View::Server(form);
                (self.rows().iter())
                    .position(|row| *row == Row::FinishSignIn)
                    .unwrap_or(0)
            }
            View::Server(form) => (form.original.as_ref())
                .and_then(|server| self.place(&server.name))
                .unwrap_or(0),
            View::Credential { .. } => (self.rows().iter())
                .position(|row| *row == Row::Credential)
                .unwrap_or(0),
            View::Import { provider, .. } => (self.rows().iter())
                .position(|row| *row == Row::Import(provider))
                .unwrap_or(0),
            View::List => 0,
        }
    }

    fn add(&mut self) -> Did {
        if !matches!(self.view, View::List) || self.listing != Listing::Ready {
            return Did::Nothing;
        }
        self.view = View::Server(Box::new(ServerForm::new(None)));
        self.typing = None;
        self.notice = None;
        Did::Went(0)
    }
}

/// What a press or a step did, which `changed` something, or didn't.
fn changed(changed: bool) -> Did {
    if changed { Did::Changed } else { Did::Nothing }
}

impl SettingsMenu {
    /// Takes in the daemon's answer to what the MCP Servers page asked
    /// under `number`. Returns a sign in's address, for the browser.
    pub(crate) fn mcp_came(
        &mut self,
        number: u64,
        result: Result<McpReply, String>,
    ) -> Option<String> {
        let on_page = self.catalogue_shown() == Some(super::Catalogued::Servers);
        // The keys stay on the row they're on, wherever a read puts it.
        let before = (on_page).then(|| self.mcp.rows().get(self.easy_focus()).cloned());
        let (url, went) = self.mcp.came(number, result);
        if !on_page {
            return url;
        }
        match (went, before.flatten()) {
            (Some(row), _) => self.easy_went(row),
            (None, Some(row)) => {
                if let Some(at) = self.mcp.rows().iter().position(|each| *each == row) {
                    self.easy_keep_focus(at);
                }
            }
            (None, None) => {}
        }
        url
    }

    /// The MCP Servers page's list is being read: the number its answer
    /// is known by.
    pub(crate) fn mcp_reading(&mut self) -> u64 {
        self.mcp.reading()
    }
}

#[cfg(test)]
mod tests;
