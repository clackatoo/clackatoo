//! The window's connection to the daemon, on a thread of its own.
//!
//! The thread connects, starting the daemon if none is running, asks for
//! the workspaces, making one if there are none, and then forwards
//! everything the daemon sends to the window's event loop. A daemon from another build of clackatoo speaks
//! another protocol: the thread tells the window, which asks whether to
//! replace it, and waits for the answer. When the connection is lost, the
//! thread ends, and reconnecting starts another.
//!
//! ssh, for a daemon on another machine, asks its questions (a password,
//! whether to trust a new host) through an [`Askpass`] the window answers,
//! since a window may have no terminal for ssh to ask on.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use clackatoo_daemon::launch::{self, LaunchError};
use clackatoo_daemon::remote::{self, Askpass, Remote, RemoteError};
use clackatoo_daemon::{Client, ClientError, Contact, Receiver, RuntimeDir, Sender};
use clackatoo_protocol::{
    BrowserUpdate, Event, NewTerminal, PROTOCOL_VERSION, PaneTerminal, ServerMessage, Size,
    TerminalStatus, Workspaces,
};
use winit::event_loop::EventLoopProxy;

/// How long the daemon may take to answer while the window opens.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the daemon is.
#[derive(Clone, Debug)]
pub(crate) enum Location {
    /// On this machine, in this runtime directory.
    Local(RuntimeDir),
    /// On another machine, reached with ssh.
    Remote(Remote),
}

impl Location {
    /// The other machine's name, if the daemon is on one.
    pub(crate) fn host(&self) -> Option<&str> {
        self.remote().map(|remote| remote.host.as_str())
    }

    /// The other machine, if the daemon is on one.
    pub(crate) fn remote(&self) -> Option<&Remote> {
        match self {
            Self::Local(_) => None,
            Self::Remote(remote) => Some(remote),
        }
    }
}

/// What the session thread tells the window.
#[derive(Debug)]
pub(crate) enum SessionEvent {
    /// Connected, and watching `workspaces`, of which there's at least one.
    /// Messages sent through `sender` are answered among later `Message`s.
    Connected {
        sender: Sender,
        workspaces: Workspaces,
        /// Whether a daemon on another machine is in contact. Changes to it
        /// arrive as [`SessionEvent::Contact`].
        contact: Option<Contact>,
        /// Which release of clackatoo the daemon is, if it said.
        daemon_version: Option<String>,
    },
    /// A daemon on another machine went out of contact, or came back.
    Contact,
    /// A daemon speaking another protocol is running. The thread waits
    /// for a [`SessionCommand`].
    OtherDaemon {
        /// The protocol it speaks.
        protocol: u32,
    },
    /// A message from the daemon.
    Message(ServerMessage),
    /// A picture of a chat's page, decoded on the session's thread, as it
    /// came, so the window never waits on decoding one.
    BrowserPicture {
        chat: clackatoo_protocol::TerminalId,
        image: std::sync::Arc<crate::render::Image>,
    },
    /// ssh asks `question`, such as for a password. ssh waits until the
    /// answer, or `None` to turn the question down, is sent on `answer`.
    Ask {
        question: String,
        answer: mpsc::Sender<Option<String>>,
    },
    /// The connection failed or closed.
    Lost(String),
    /// The config's Lua has something for the window. Sent from the Lua
    /// thread, not the session's.
    Lua,
    /// What's on the clipboard and the primary selection, as far as they
    /// could be read, for the editors. Sent from a thread reading them, not
    /// the session's.
    Clipboard(Vec<(crate::pane::Copy, String)>),
    /// Time to look for a new release, if the options say to. Sent a while
    /// after the window opens (see [`crate::updates`]).
    UpdateDue,
    /// A newer release of clackatoo is out: this one.
    NewRelease(String),
    /// `clackatoo update`, run by `:update`, has finished, and the bar
    /// should say this.
    Updated(crate::updates::Updated),
    /// Machines to connect to, for the picker of one. Sent from a thread
    /// finding them, not the session's (see [`crate::connect::gather`]).
    Hosts(crate::connect::Found),
}

/// What the window tells the session thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionCommand {
    /// Stop the other daemon, closing its terminals, and start a new one.
    ReplaceDaemon,
}

/// The window's side of the session thread.
#[derive(Debug)]
pub(crate) struct Session {
    /// Where the daemon is, once it's known: a window opened as the
    /// picker of a machine to connect to waits for one to be picked.
    location: Option<Location>,
    /// Where the socket ssh asks questions through goes.
    runtime: RuntimeDir,
    proxy: EventLoopProxy<SessionEvent>,
    /// Answers the thread's questions. Replacing it tells the last thread
    /// to give up.
    commands: mpsc::Sender<SessionCommand>,
    /// Where ssh asks its questions, for a daemon on another machine.
    askpass: Option<Askpass>,
}

impl Session {
    /// Starts connecting to the daemon at `location`. Its events arrive
    /// through `proxy`. The socket ssh asks questions through, if the
    /// daemon is on another machine, goes in `runtime`.
    pub(crate) fn start(
        location: Location,
        runtime: &RuntimeDir,
        proxy: EventLoopProxy<SessionEvent>,
    ) -> Self {
        let mut session = Self::waiting(runtime, proxy);
        session.connect(location);
        session
    }

    /// A session that connects to nothing until it's told where the
    /// daemon is, by [`connect`](Self::connect).
    pub(crate) fn waiting(runtime: &RuntimeDir, proxy: EventLoopProxy<SessionEvent>) -> Self {
        Self {
            location: None,
            runtime: runtime.clone(),
            proxy,
            commands: mpsc::channel().0,
            askpass: None,
        }
    }

    /// Starts connecting to the daemon at `location`, in place of any
    /// before, whose thread has ended.
    pub(crate) fn connect(&mut self, location: Location) {
        // The last one's socket goes first: the next is at the same path.
        self.askpass = None;
        self.askpass = match location {
            Location::Local(_) => None,
            Location::Remote(_) => {
                let proxy = self.proxy.clone();
                let asked = Askpass::serve(&self.runtime, move |question| {
                    let (answer, answered) = mpsc::channel();
                    proxy
                        .send_event(SessionEvent::Ask { question, answer })
                        .ok()?;
                    answered.recv().ok().flatten()
                });
                // Without it, ssh asks on the terminal, if there is one.
                asked
                    .inspect_err(|err| log::warn!("ssh can't ask the window for passwords: {err}"))
                    .ok()
            }
        };
        self.commands = spawn(
            location.clone(),
            askpass_path(self.askpass.as_ref()),
            self.proxy.clone(),
        );
        self.location = Some(location);
    }

    /// Forgets where the daemon is, after a try at connecting to it
    /// failed, until told again by [`connect`](Self::connect). A thread
    /// waiting to be told whether to replace another daemon gives up,
    /// saying nothing.
    pub(crate) fn forget(&mut self) {
        self.location = None;
        self.askpass = None;
        self.commands = mpsc::channel().0;
    }

    /// The other machine's name, if the daemon is on one.
    pub(crate) fn host(&self) -> Option<&str> {
        self.location.as_ref()?.host()
    }

    /// The other machine, if the daemon is on one.
    pub(crate) fn remote(&self) -> Option<&Remote> {
        self.location.as_ref()?.remote()
    }

    /// Whether a daemon on this machine is listening, as one is when the
    /// connection was lost because the daemon upgraded. Never starts one.
    pub(crate) fn daemon_listening(&self) -> bool {
        match &self.location {
            Some(Location::Local(runtime)) => {
                std::os::unix::net::UnixStream::connect(runtime.socket_path()).is_ok()
            }
            Some(Location::Remote(_)) | None => false,
        }
    }

    /// A way to wake the window with an event, from another thread.
    pub(crate) fn proxy(&self) -> EventLoopProxy<SessionEvent> {
        self.proxy.clone()
    }

    /// Connects again after the connection was lost.
    pub(crate) fn reconnect(&mut self) {
        let Some(location) = self.location.clone() else {
            return;
        };
        self.commands = spawn(
            location,
            askpass_path(self.askpass.as_ref()),
            self.proxy.clone(),
        );
    }

    /// Answers [`SessionEvent::OtherDaemon`]: replace it.
    pub(crate) fn replace_daemon(&self) {
        // The thread has gone if this fails, and has said why.
        let _ = self.commands.send(SessionCommand::ReplaceDaemon);
    }
}

fn askpass_path(askpass: Option<&Askpass>) -> Option<PathBuf> {
    askpass.map(|askpass| askpass.path().to_owned())
}

/// Starts a session thread for the daemon at `location`, with ssh asking
/// questions through the socket at `askpass`. The returned sender answers
/// the thread's questions; dropping it tells the thread to give up.
fn spawn(
    location: Location,
    askpass: Option<PathBuf>,
    proxy: EventLoopProxy<SessionEvent>,
) -> mpsc::Sender<SessionCommand> {
    let (commands, received) = mpsc::channel();
    std::thread::Builder::new()
        .name("session".into())
        .spawn(move || run(&location, askpass.as_deref(), &proxy, &received))
        .expect("threads can be started");
    commands
}

/// The outcome of connecting.
enum Connected {
    Client(Client),
    /// The daemon speaks this other protocol.
    OtherProtocol(u32),
    Failed(String),
}

fn run(
    location: &Location,
    askpass: Option<&Path>,
    proxy: &EventLoopProxy<SessionEvent>,
    commands: &mpsc::Receiver<SessionCommand>,
) {
    // Failing to send an event means the window has closed.
    let lost = |reason: String| drop(proxy.send_event(SessionEvent::Lost(reason)));
    let mut replace = false;
    let client = loop {
        match connect(location, askpass, replace) {
            Connected::Client(client) => break client,
            // Replacing it started a daemon from another build: the one
            // installed on the remote machine.
            Connected::OtherProtocol(protocol) if replace => {
                return lost(format!(
                    "clackatoo{} is from another build: it starts daemons speaking protocol \
                     {protocol}, and this window speaks {PROTOCOL_VERSION}. `clackatoo update` there updates it.",
                    location
                        .host()
                        .map(|host| format!(" on {host}"))
                        .unwrap_or_default()
                ));
            }
            Connected::OtherProtocol(protocol) => {
                if proxy
                    .send_event(SessionEvent::OtherDaemon { protocol })
                    .is_err()
                {
                    return;
                }
                match commands.recv() {
                    Ok(SessionCommand::ReplaceDaemon) => replace = true,
                    Err(mpsc::RecvError) => return,
                }
            }
            Connected::Failed(reason) => return lost(reason),
        }
    };
    let contact = client.contact();
    if let Some(contact) = &contact {
        let proxy = proxy.clone();
        contact.on_change(move || drop(proxy.send_event(SessionEvent::Contact)));
    }
    let mut receiver = match open(client) {
        Ok((sender, receiver, workspaces, daemon_version)) => {
            let connected = SessionEvent::Connected {
                sender,
                workspaces,
                contact,
                daemon_version,
            };
            if proxy.send_event(connected).is_err() {
                return;
            }
            receiver
        }
        Err(err) => return lost(err.to_string()),
    };
    loop {
        let event = match receiver.receive() {
            Ok(ServerMessage::Event(Event::Browser {
                chat,
                update: BrowserUpdate::Frame(frame),
            })) => match crate::browser::decode(&frame.jpeg) {
                Some(image) => SessionEvent::BrowserPicture {
                    chat,
                    image: std::sync::Arc::new(image),
                },
                None => continue,
            },
            Ok(message) => SessionEvent::Message(message),
            Err(err) => return lost(err.to_string()),
        };
        if proxy.send_event(event).is_err() {
            return;
        }
    }
}

/// Connects to the daemon at `location`, starting it if it isn't running,
/// or with `replace`, replacing it first: upgrading it if it can be, or else
/// stopping it. ssh asks its questions through the socket at `askpass`.
fn connect(location: &Location, askpass: Option<&Path>, replace: bool) -> Connected {
    let result = match location {
        Location::Local(runtime) => {
            if replace && let Err(err) = launch::replace(runtime) {
                return Connected::Failed(format!(
                    "couldn't replace the other daemon: {}",
                    with_causes(&err)
                ));
            }
            launch::connect_or_start(runtime).map_err(|err| match err {
                LaunchError::Client(ClientError::VersionMismatch { daemon, .. }) => Ok(daemon),
                err => Err(with_causes(&err)),
            })
        }
        Location::Remote(remote) => {
            remote::connect(remote, replace, askpass).map_err(|err| match err {
                RemoteError::Client(ClientError::VersionMismatch { daemon, .. }) => Ok(daemon),
                err => Err(with_causes(&err)),
            })
        }
    };
    match result {
        Ok(client) => Connected::Client(client),
        Err(Ok(protocol)) => Connected::OtherProtocol(protocol),
        Err(Err(reason)) => Connected::Failed(reason),
    }
}

/// `err`, followed by what caused it.
fn with_causes(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut cause = err.source();
    while let Some(err) = cause {
        text = format!("{text}: {err}");
        cause = err.source();
    }
    text
}

/// Starts watching the workspaces. With none, makes one, showing the
/// newest running terminal, such as one a script started, or else a new
/// one. Also asks which release the daemon is.
fn open(mut client: Client) -> Result<(Sender, Receiver, Workspaces, Option<String>), ClientError> {
    // A daemon that accepts but never answers would otherwise leave the
    // window blank forever.
    client.set_read_timeout(Some(OPEN_TIMEOUT))?;
    let mut workspaces = client.watch_workspaces()?;
    if workspaces.list.is_empty() {
        let newest = client
            .list_terminals()?
            .into_iter()
            .filter(|terminal| terminal.status == TerminalStatus::Running)
            .map(|terminal| terminal.id)
            .max();
        let terminal = match newest {
            Some(terminal) => PaneTerminal::Existing(terminal),
            // Started at a typical size; the window resizes it to fit at once.
            None => PaneTerminal::New(NewTerminal {
                size: Size::new(80, 24).expect("80x24 is a valid size"),
                command: None,
                cwd: None,
                record_to: None,
            }),
        };
        client.new_workspace(None, terminal)?;
        workspaces = client.watch_workspaces()?;
    }
    let daemon_version = client
        .daemon_version()
        .inspect_err(|err| log::warn!("the daemon didn't say which release it is: {err}"))
        .ok();
    client.set_read_timeout(None)?;
    let (sender, receiver) = client.split()?;
    Ok((sender, receiver, workspaces, daemon_version))
}
