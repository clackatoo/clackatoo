//! The window's connection to the daemon, on a thread of its own.
//!
//! The thread connects, starting the daemon if none is running, attaches
//! to a terminal, and then forwards everything the daemon sends to the
//! window's event loop. A daemon from an older build of clackatoo speaks
//! an older protocol: the thread tells the window, which asks whether to
//! replace it, and waits for the answer.

use std::sync::mpsc;
use std::time::Duration;

use clackatoo_daemon::launch::{self, LaunchError};
use clackatoo_daemon::{Client, ClientError, Receiver, RuntimeDir, Sender};
use clackatoo_protocol::{NewTerminal, ServerMessage, Size, TerminalId, TerminalStatus};
use winit::event_loop::EventLoopProxy;

/// How long the daemon may take to answer while the window attaches.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

/// What the session thread tells the window.
#[derive(Debug)]
pub(crate) enum SessionEvent {
    /// Attached to `terminal`. Messages sent through `sender` are answered
    /// among later `Message`s.
    Attached {
        sender: Sender,
        terminal: TerminalId,
    },
    /// A daemon speaking an older protocol is running. The thread waits
    /// for a [`SessionCommand`].
    OlderDaemon {
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
pub(crate) enum SessionCommand {
    /// Stop the older daemon, closing its terminals, and start a new one.
    ReplaceDaemon,
}

/// Starts the session thread for the daemon in `runtime`. The returned
/// sender answers its questions; dropping it tells the thread to give up.
pub(crate) fn spawn(
    runtime: RuntimeDir,
    proxy: EventLoopProxy<SessionEvent>,
) -> mpsc::Sender<SessionCommand> {
    let (commands, received) = mpsc::channel();
    std::thread::Builder::new()
        .name("session".into())
        .spawn(move || run(&runtime, &proxy, &received))
        .expect("threads can be started");
    commands
}

fn run(
    runtime: &RuntimeDir,
    proxy: &EventLoopProxy<SessionEvent>,
    commands: &mpsc::Receiver<SessionCommand>,
) {
    // Failing to send an event means the window has closed.
    let lost = |reason: String| drop(proxy.send_event(SessionEvent::Lost(reason)));
    let client = loop {
        match launch::connect_or_start(runtime) {
            Ok(client) => break client,
            Err(LaunchError::Client(ClientError::VersionMismatch { daemon, .. })) => {
                if proxy
                    .send_event(SessionEvent::OlderDaemon { protocol: daemon })
                    .is_err()
                {
                    return;
                }
                match commands.recv() {
                    Ok(SessionCommand::ReplaceDaemon) => {
                        if let Err(err) = launch::stop(runtime) {
                            return lost(format!("couldn't stop the older daemon: {err}"));
                        }
                    }
                    Err(mpsc::RecvError) => return,
                }
            }
            Err(err) => return lost(format!("couldn't connect to the daemon: {err}")),
        }
    };
    let mut receiver = match attach(client) {
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

/// Attaches to the newest running terminal, starting one if there is none.
fn attach(mut client: Client) -> Result<(Sender, Receiver, TerminalId), ClientError> {
    // A daemon that accepts but never answers would otherwise leave the
    // window blank forever.
    client.set_read_timeout(Some(ATTACH_TIMEOUT))?;
    let running = client
        .list_terminals()?
        .into_iter()
        .filter(|terminal| terminal.status == TerminalStatus::Running)
        .map(|terminal| terminal.id)
        .max();
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
