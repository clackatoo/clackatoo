//! The daemon's offers to install a file's language, and its language
//! server, when they're missing: asked in the bar, as `languageinstall`
//! says, or taken or turned down without asking.
//!
//! The question is asked while an editor has the focus, and neither the
//! command line nor the finder is open, and waits for y or Enter, which installs, or n, which
//! declines for good. Any other key drops it unanswered, and goes on as it
//! would have: each window is offered each language once, so the next
//! window opened asks again.

use clackatoo_protocol::{BufferId, Request, wire};
use winit::keyboard::{Key, NamedKey};

use super::{Asked, Desk};
use crate::chrome::Status;
use crate::input::KeyPress;
use crate::options::LanguageInstall;

/// What the daemon offered to install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Offer {
    /// The buffer it was offered for, which says on which machine.
    buffer: BufferId,
    language: String,
    /// The language's grammar.
    grammar: bool,
    /// The language server, by its program's name.
    server: Option<String>,
}

impl Offer {
    /// What it installs, for the bar.
    fn what(&self) -> String {
        let language = &self.language;
        match (&self.server, self.grammar) {
            (Some(server), true) => format!("{language}'s grammar and {server}"),
            (Some(server), false) => format!("{server}, {language}'s language server"),
            (None, _) => format!("{language}'s grammar"),
        }
    }
}

impl Desk {
    /// The daemon offers to install `language`'s grammar if `grammar`, and
    /// the language server `server`. Returns whether the window must be
    /// redrawn.
    pub(super) fn offered(
        &mut self,
        buffer: BufferId,
        language: String,
        grammar: bool,
        server: Option<String>,
    ) -> bool {
        let offer = Offer {
            buffer,
            language,
            grammar,
            server,
        };
        match self.options.languageinstall {
            LanguageInstall::Always => self.install(offer),
            LanguageInstall::Never => return false,
            // One question at a time: the next window asks the next.
            LanguageInstall::Ask if self.offer.is_some() => return false,
            LanguageInstall::Ask => self.offer = Some(offer),
        }
        true
    }

    fn install(&mut self, offer: Offer) {
        self.say(format!("Installing {}…", offer.what()));
        let request = Request::Install {
            language: offer.language,
            grammar: offer.grammar,
            server: offer.server,
            buffer: self.daemon_has(wire::RELAY).then_some(offer.buffer),
        };
        self.request(request, Asked::Command);
    }

    /// Answers the question asked, if `press` does: y or Enter installs,
    /// n declines. Any other key drops the question. Returns whether
    /// `press` was the answer.
    pub(super) fn offer_key(&mut self, press: &KeyPress<'_>) -> bool {
        if !self.offer_asked() {
            return false;
        }
        let Some(offer) = self.offer.take() else {
            return false;
        };
        let yes = matches!(press.key, Key::Named(NamedKey::Enter))
            || matches!(press.text, Some("y" | "Y"));
        if yes {
            self.install(offer);
            return true;
        }
        if matches!(press.text, Some("n" | "N")) {
            self.say(format!(
                "{} won't be offered again; `clackatoo language install {}` installs it",
                offer.what(),
                offer.language
            ));
            let decline = Request::Decline {
                language: offer.language,
                buffer: self.daemon_has(wire::RELAY).then_some(offer.buffer),
            };
            self.request(decline, Asked::Quiet);
            return true;
        }
        false
    }

    /// Whether an offer is being asked about: there's one, an editor has
    /// the focus, and neither the command line nor the finder is open,
    /// whose keys they are. Until then it waits.
    pub(super) fn offer_asked(&self) -> bool {
        self.offer.is_some()
            && self.input.is_none()
            && !self.finding()
            && self.focused_editor().is_some()
    }

    /// The question asked, for the bar.
    pub(super) fn offer_status(&self) -> Option<Status> {
        if !self.offer_asked() {
            return None;
        }
        let offer = self.offer.as_ref()?;
        Some(Status::Typing {
            label: None,
            text: format!("Install {}? [Y/n]", offer.what()),
            choices: Vec::new(),
            selected: None,
            under: None,
        })
    }

    /// An install the window asked for has ended, as `outcome` says: what
    /// was installed, or why it couldn't be.
    pub(crate) fn installed(&mut self, outcome: Result<String, String>) {
        match outcome {
            Ok(what) => self.say(format!("Installed {what}")),
            Err(why) => self.warn(why),
        }
    }
}
