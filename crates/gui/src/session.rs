//! The window's connection to the daemon, on a thread of its own.
//!
//! The thread connects, starting the daemon if none is running, attaches
//! to a terminal, and then forwards everything the daemon sends to the
//! window's event loop. A daemon from another build of clackatoo speaks
//! another protocol: the thread tells the window, which asks whether to
//! replace it, and waits for the answer. When the connection is lost, the
//! thread ends, and reconnecting starts another.

use std::sync::mpsc;
use std::time::Duration;

use clackatoo_daemon::launch::{self, LaunchError};
use clackatoo_daemon::remote::{self, Remote, RemoteError};
use clackatoo_daemon::{Client, ClientError, Receiver, RuntimeDir, Sender};
use clackatoo_protocol::{
    NewTerminal, PROTOCOL_VERSION, ServerMessage, Size, TerminalId, TerminalStatus,
};
use winit::event_loop::EventLoopProxy;

/// How long the daemon may take to answer while the window attaches.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

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
        match self {
            Self::Local(_) => None,
            Self::Remote(remote) => Some(&remote.host),
        }
    }
}

/// What the session thread tells the window.
#[derive(Debug)]
pub(crate) enum SessionEvent {
    /// Attached to `terminal`. Messages sent through `sender` are answered
    /// among later `Message`s.
    Attached {
        sender: Sender,
        terminal: TerminalId,
    },
    /// A daemon speaking another protocol is running. The thread waits
    /// for a [`SessionCommand`].
    OtherDaemon {
        /// The protocol it speaks.
        protocol: u32,
    },
    /// A message from the daemon.
    Message(ServerMessage),
    /// The connection failed or closed.
    Lost(String),
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
    location: Location,
    proxy: EventLoopProxy<SessionEvent>,
    /// Answers the thread's questions. Replacing it tells the last thread
    /// to give up.
    commands: mpsc::Sender<SessionCommand>,
}

impl Session {
    /// Starts connecting to the daemon at `location`. Its events arrive
    /// through `proxy`.
    pub(crate) fn start(location: Location, proxy: EventLoopProxy<SessionEvent>) -> Self {
        let commands = spawn(location.clone(), None, proxy.clone());
        Self {
            location,
            proxy,
            commands,
        }
    }

    pub(crate) fn location(&self) -> &Location {
        &self.location
    }

    /// Connects again after the connection was lost, attaching to
    /// `preferred` if it's still running.
    pub(crate) fn reconnect(&mut self, preferred: Option<TerminalId>) {
        self.commands = spawn(self.location.clone(), preferred, self.proxy.clone());
    }

    /// Answers [`SessionEvent::OtherDaemon`]: replace it.
    pub(crate) fn replace_daemon(&self) {
        // The thread has gone if this fails, and has said why.
        let _ = self.commands.send(SessionCommand::ReplaceDaemon);
    }
}

/// Starts a session thread for the daemon at `location`, attaching to
/// `preferred` if it's running. The returned sender answers the thread's
/// questions; dropping it tells the thread to give up.
fn spawn(
    location: Location,
    preferred: Option<TerminalId>,
    proxy: EventLoopProxy<SessionEvent>,
) -> mpsc::Sender<SessionCommand> {
    let (commands, received) = mpsc::channel();
    std::thread::Builder::new()
        .name("session".into())
        .spawn(move || run(&location, preferred, &proxy, &received))
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
    preferred: Option<TerminalId>,
    proxy: &EventLoopProxy<SessionEvent>,
    commands: &mpsc::Receiver<SessionCommand>,
) {
    // Failing to send an event means the window has closed.
    let lost = |reason: String| drop(proxy.send_event(SessionEvent::Lost(reason)));
    let mut replace = false;
    let client = loop {
        match connect(location, replace) {
            Connected::Client(client) => break client,
            // Replacing it started a daemon from another build: the one
            // installed on the remote machine.
            Connected::OtherProtocol(protocol) if replace => {
                return lost(format!(
                    "clackatoo{} is from another build: it starts daemons speaking protocol \
                     {protocol}, and this window speaks {PROTOCOL_VERSION}. Update it there.",
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
    let mut receiver = match attach(client, preferred) {
        Ok((sender, receiver, terminal)) => {
            if proxy
                .send_event(SessionEvent::Attached { sender, terminal })
                .is_err()
            {
                return;
            }
            receiver
        }
        Err(err) => return lost(err.to_string()),
    };
    loop {
        let event = match receiver.receive() {
            Ok(message) => SessionEvent::Message(message),
            Err(err) => return lost(err.to_string()),
        };
        if proxy.send_event(event).is_err() {
            return;
        }
    }
}

/// Connects to the daemon at `location`, starting it if it isn't running,
/// or with `replace`, stopping it first.
fn connect(location: &Location, replace: bool) -> Connected {
    let result = match location {
        Location::Local(runtime) => {
            if replace && let Err(err) = launch::stop(runtime) {
                return Connected::Failed(format!(
                    "couldn't stop the other daemon: {}",
                    with_causes(&err)
                ));
            }
            launch::connect_or_start(runtime).map_err(|err| match err {
                LaunchError::Client(ClientError::VersionMismatch { daemon, .. }) => Ok(daemon),
                err => Err(with_causes(&err)),
            })
        }
        Location::Remote(remote) => remote::connect(remote, replace).map_err(|err| match err {
            RemoteError::Client(ClientError::VersionMismatch { daemon, .. }) => Ok(daemon),
            err => Err(with_causes(&err)),
        }),
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

/// Attaches to `preferred` if it's running, or else the newest running
/// terminal, starting one if there is none.
fn attach(
    mut client: Client,
    preferred: Option<TerminalId>,
) -> Result<(Sender, Receiver, TerminalId), ClientError> {
    // A daemon that accepts but never answers would otherwise leave the
    // window blank forever.
    client.set_read_timeout(Some(ATTACH_TIMEOUT))?;
    let running: Vec<TerminalId> = client
        .list_terminals()?
        .into_iter()
        .filter(|terminal| terminal.status == TerminalStatus::Running)
        .map(|terminal| terminal.id)
        .collect();
    let running = preferred
        .filter(|terminal| running.contains(terminal))
        .or_else(|| running.into_iter().max());
    let terminal = match running {
        Some(terminal) => terminal,
        // Started at a typical size; the window resizes it to fit at once.
        None => client.new_terminal(NewTerminal {
            size: Size::new(80, 24).expect("80x24 is a valid size"),
            command: None,
            cwd: None,
            record_to: None,
        })?,
    };
    client.attach(terminal)?;
    client.set_read_timeout(None)?;
    let (sender, receiver) = client.split()?;
    Ok((sender, receiver, terminal))
}
