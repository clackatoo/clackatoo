//! The agent's end of a proxy: its standard input, read a JSON message a
//! line, and its standard output, written the same way. Each has a
//! thread of its own, as pipes block.
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

/// The longest message read from the agent, or written to it.
pub(super) const MAX_MESSAGE: usize = 4 << 20;

/// How much may wait to be written to an agent that isn't reading
/// before it's taken to have gone. An agent slow to read is waited for:
/// only one that has stopped altogether gets this far.
const MAX_WAITING: usize = 64 << 20;

/// A line the agent wrote.
#[derive(Debug)]
pub(super) enum Line {
    /// A JSON message.
    Message(Value),
    /// One that couldn't be read, for this reason: it's passed over.
    Unreadable(&'static str),
}

/// The agent's input, cut into lines a byte at a time.
#[derive(Default)]
struct Lines {
    /// The line so far.
    line: Vec<u8>,
    /// Whether the line so far was found too long: the rest of it is
    /// passed over.
    skipping: bool,
}

impl Lines {
    /// Takes the next byte of input, and returns the line it ends, if it
    /// ends one worth telling of.
    fn push(&mut self, byte: u8) -> Option<Line> {
        if byte == b'\n' {
            return self.end();
        }
        if self.skipping {
            return None;
        }
        self.line.push(byte);
        if self.line.len() <= MAX_MESSAGE {
            return None;
        }
        self.line.clear();
        self.skipping = true;
        Some(Line::Unreadable("A message over 4 MiB was not read"))
    }

    /// The line so far, now it has ended: a message, or why it isn't
    /// one. An empty line, and the end of one passed over, are nothing.
    fn end(&mut self) -> Option<Line> {
        let line = std::mem::take(&mut self.line);
        if std::mem::take(&mut self.skipping) || line.is_empty() {
            return None;
        }
        Some(match serde_json::from_slice(&line) {
            Ok(message) => Line::Message(message),
            Err(_) => Line::Unreadable("A line that isn't JSON was not read"),
        })
    }
}

/// Reads the agent's messages from `input` on a thread of its own. The
/// channel closes as the agent closes its end.
pub(super) fn read(mut input: std::fs::File) -> tokio::sync::mpsc::Receiver<Line> {
    let (told, read) = tokio::sync::mpsc::channel(64);
    std::thread::spawn(move || {
        let mut lines = Lines::default();
        let mut chunk = [0; 16384];
        loop {
            let n = match input.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            for &byte in &chunk[..n] {
                if let Some(line) = lines.push(byte)
                    && told.blocking_send(line).is_err()
                {
                    return;
                }
            }
        }
        // A last message with no newline after it still counts.
        if let Some(line) = lines.end() {
            let _ = told.blocking_send(line);
        }
    });
    read
}

/// What's written to the agent, by a thread of its own.
pub(super) struct Output {
    lines: std::sync::mpsc::Sender<Vec<u8>>,
    /// How many bytes wait to be written.
    waiting: Arc<AtomicUsize>,
}

/// The agent can no longer be written to: it closed its end, or stopped
/// reading with too much waiting for it.
#[derive(Debug)]
pub(super) struct Gone;

impl Output {
    /// Writes to `output` on a thread of its own.
    pub(super) fn new(mut output: std::fs::File) -> Self {
        let (lines, written) = std::sync::mpsc::channel::<Vec<u8>>();
        let waiting = Arc::new(AtomicUsize::new(0));
        let left = Arc::clone(&waiting);
        std::thread::spawn(move || {
            while let Ok(line) = written.recv() {
                let result = output.write_all(&line).and_then(|()| output.flush());
                left.fetch_sub(line.len(), Ordering::Relaxed);
                if result.is_err() {
                    break;
                }
            }
        });
        Self { lines, waiting }
    }

    /// Queues `message` for the agent, behind those before it.
    pub(super) fn write(&self, message: &Value) -> Result<(), Gone> {
        let Some(line) = line(message) else {
            return Ok(());
        };
        if self.waiting.fetch_add(line.len(), Ordering::Relaxed) > MAX_WAITING {
            return Err(Gone);
        }
        self.lines.send(line).map_err(|_| Gone)
    }

    /// Whether everything queued has been written, or can't be.
    pub(super) fn drained(&self) -> bool {
        self.waiting.load(Ordering::Relaxed) == 0
    }
}

/// `message` as a line for the agent. One over [`MAX_MESSAGE`] isn't
/// sent: if it answers a request, an error saying so answers it instead,
/// so the agent isn't left waiting; anything else that long is dropped.
pub(super) fn line(message: &Value) -> Option<Vec<u8>> {
    let mut line = serde_json::to_vec(message).ok()?;
    if line.len() > MAX_MESSAGE {
        let id = message
            .get("id")
            .filter(|_| message.get("method").is_none())?;
        let why = format!(
            "The MCP server's answer is {} MiB, and clackatoo passes on at most {} MiB. Ask it \
             for less at a time.",
            line.len().div_ceil(1 << 20),
            MAX_MESSAGE >> 20
        );
        let refused = json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":why}});
        line = serde_json::to_vec(&refused).ok()?;
    }
    line.push(b'\n');
    Some(line)
}
