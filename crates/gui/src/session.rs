//! The window's connection to the daemon, on a thread of its own.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clackatoo_daemon::{Client, Sender};
use clackatoo_protocol::{NewTerminal, ServerMessage, Size, TerminalId, TerminalStatus};
use winit::event_loop::EventLoopProxy;

/// What the session thread tells the window.
#[derive(Debug)]
pub(crate) enum SessionEvent {
    /// Attached to `terminal`. Messages sent through `sender` are answered
    /// among later `Message`s.
    Attached {
        sender: Sender,
        terminal: TerminalId,
    },
    /// A message from the daemon.
    Message(ServerMessage),
    /// The connection failed or closed.
    Lost(String),
}

/// Connects to the daemon at `socket`, attaches to a terminal, and then
/// forwards every message the daemon sends to the window's event loop,
/// until the connection ends or the window closes.
pub(crate) fn spawn(socket: PathBuf, proxy: EventLoopProxy<SessionEvent>) {
    let run = move || {
        let mut receiver = match attach(&socket) {
            Ok((sender, receiver, terminal)) => {
                if proxy
                    .send_event(SessionEvent::Attached { sender, terminal })
                    .is_err()
                {
                    return;
                }
                receiver
            }
            Err(err) => {
                let _ = proxy.send_event(SessionEvent::Lost(format!("{err:#}")));
                return;
            }
        };
        loop {
            let event = match receiver.receive() {
                Ok(message) => SessionEvent::Message(message),
                Err(err) => SessionEvent::Lost(err.to_string()),
            };
            let lost = matches!(event, SessionEvent::Lost(_));
            // The window has closed if this fails.
            if proxy.send_event(event).is_err() || lost {
                return;
            }
        }
    };
    std::thread::Builder::new()
        .name("session".into())
        .spawn(run)
        .expect("threads can be started");
}

/// Attaches to the newest running terminal, starting one if there is none.
fn attach(socket: &Path) -> anyhow::Result<(Sender, clackatoo_daemon::Receiver, TerminalId)> {
    let mut client = Client::connect(socket).context("could not connect to the daemon")?;
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
    let (sender, receiver) = client.split()?;
    Ok((sender, receiver, terminal))
}
