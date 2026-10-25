//! What the bottom bar says until the next key, and what it's said lately,
//! which `:Messages` shows again: the window's own messages, as they're
//! said, and an editor's, as the bar shows them.
//!
//! Something gone wrong is said as a warning: in the warning colour, and
//! for a few seconds, whole in a box above the bar if it's too long for
//! it.

use std::time::{Duration, Instant};

use super::Desk;
use crate::chrome::NoteKind;

/// How many messages `:Messages` keeps, the oldest going first.
const KEPT: usize = 200;

/// How long a warning too long for the bar shows whole above it, unless a
/// key comes first.
pub(crate) const NOTICE: Duration = Duration::from_secs(6);

/// What's been said, besides what the bar says now.
#[derive(Debug, Default)]
pub(super) struct Said {
    /// The warning said, and when, while the bar still says it.
    warned: Option<(String, Instant)>,
    /// What's been said lately, oldest first.
    lately: Vec<String>,
    /// Whether `:Messages` shows them, until the next key.
    shown: bool,
    /// What the focused editor said, as last seen in the bar, so it's kept
    /// once however often the bar is drawn.
    seen: Option<String>,
}

impl Said {
    /// Keeps `message`, of something gone wrong, for `:Messages`, and
    /// marks it a warning while the bar says it.
    pub(super) fn warned(&mut self, message: &str) {
        self.warned = Some((message.to_owned(), Instant::now()));
        self.keep(message);
    }

    /// Keeps `message` for `:Messages`.
    pub(super) fn keep(&mut self, message: &str) {
        if self.lately.len() == KEPT {
            self.lately.remove(0);
        }
        self.lately.push(message.to_owned());
    }
}

impl Desk {
    /// Says `message` in the bar, until the next key.
    pub(crate) fn say(&mut self, message: String) {
        self.said.keep(&message);
        self.message = Some(message);
    }

    /// Warns `said`, what the daemon says of the workspaces for the bar
    /// ([`clackatoo_protocol::Event::Said`]), as that a machine lost panes
    /// it lent: on one line, cut short.
    pub(crate) fn daemon_said(&mut self, said: &str) {
        let said: String = (said.chars())
            .filter(|ch| !ch.is_control())
            .take(512)
            .collect();
        self.warn(said);
    }

    /// Says `said`, what the daemon says of a port it forwards to another
    /// machine for the window ([`clackatoo_protocol::Event::Forwarded`]),
    /// as the window says what its own forwards run into.
    pub(crate) fn forwarded(&mut self, said: &str) {
        let mut said: String = (said.chars())
            .filter(|ch| !ch.is_control())
            .take(512)
            .collect();
        if let Some(first) = said.get(..1) {
            said.replace_range(..1, &first.to_uppercase());
        }
        self.say(said);
    }

    /// Says `message`, of something gone wrong, in the bar until the next
    /// key: in the warning colour, and if it's too long for the bar, whole
    /// above it for a few seconds.
    pub(crate) fn warn(&mut self, message: String) {
        self.said.warned(&message);
        self.message = Some(message);
    }

    /// The message the bar says, and what kind it is: a warning, while it's
    /// the one warned of.
    pub(super) fn message_note(&self) -> Option<(String, NoteKind)> {
        let message = self.message.as_ref()?;
        let kind = match &self.said.warned {
            Some((warned, at)) if warned == message => NoteKind::Warning {
                notice: at.elapsed() < NOTICE,
            },
            _ => NoteKind::Message,
        };
        Some((message.clone(), kind))
    }

    /// When a warning shown whole above the bar goes, for the window to
    /// wake then.
    pub(crate) fn notice_ends(&self) -> Option<Instant> {
        let (warned, at) = self.said.warned.as_ref()?;
        let ends = *at + NOTICE;
        (self.message.as_ref() == Some(warned) && ends > Instant::now()).then_some(ends)
    }

    /// Keeps what the focused editor says in the bar, such as `not found`,
    /// for `:Messages`, if it's new since it was last looked at. The window
    /// calls it as it draws the bar.
    pub(crate) fn remember(&mut self) {
        let said = (self.focused_editor()).and_then(|(editor, _)| editor.said());
        if said == self.said.seen.as_deref() {
            return;
        }
        let said = said.map(str::to_owned);
        if let Some(said) = &said {
            self.said.keep(said);
        }
        self.said.seen = said;
    }

    /// `:Messages`: shows what's been said lately, above the bar, until
    /// the next key.
    pub(super) fn show_messages(&mut self) {
        if self.said.lately.is_empty() {
            // Not kept: it's no message of its own.
            self.message = Some("nothing's been said yet".into());
        }
        self.said.shown = true;
    }

    /// What's been said lately, oldest first, while `:Messages` shows it.
    pub(crate) fn messages_listed(&self) -> Option<&[String]> {
        self.said.shown.then_some(&self.said.lately[..])
    }

    /// A key came: `:Messages` closes, and a warning goes from above the
    /// bar. Returns whether anything went, to draw.
    pub(super) fn key_closes_messages(&mut self) -> bool {
        let shown = std::mem::take(&mut self.said.shown);
        let warned = self.said.warned.take().is_some();
        shown || warned
    }
}
