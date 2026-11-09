//! The daemon's question whether to trust a folder to run its language
//! servers, which may run its code (CLACKA-224), asked in the bar with the
//! shared y/n question ([`Confirm`]), and `:trust`, `:untrust` and
//! `:trusted`.
//!
//! The question waits until an editor showing a file of the folder has
//! the focus, in normal mode, with nothing half typed, and neither the
//! command line nor the finder open. Only a key pressed then answers it:
//! not one a macro or a binding types, nor one held while something was
//! waited for, and none in the first [`ANSWER_PAUSE`] it shows, so a key
//! meant for what was there before can't. Native Control's keys can't
//! either: the window refuses to send what they'd answer. y trusts the
//! folder, n marks it not trusted, and any other key leaves it unanswered
//! and does what it would have done.

use std::cell::Cell;
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use clackatoo_protocol::{BufferId, Request, TrustAction, wire};

use super::{Asked, Desk};
use crate::chrome::Status;
use crate::input::KeyPress;
use crate::vim::Mode;
use crate::vim::confirm::Confirm;

/// How long the question shows before a key answers it.
const ANSWER_PAUSE: Duration = Duration::from_millis(crate::chat::ANSWER_PAUSE);

/// The folders the daemon asked about, the first asked first.
#[derive(Debug, Default)]
pub(super) struct Questions {
    asked: Vec<Question>,
    /// When the first question showed in the bar, while it shows.
    pub(super) shown_since: Cell<Option<Instant>>,
}

/// A folder the daemon asked about.
#[derive(Debug)]
struct Question {
    /// The buffer it was asked for, which says on which machine.
    buffer: BufferId,
    /// The folder, as that machine names it.
    root: String,
    asked: Confirm<bool>,
}

impl Desk {
    /// The daemon asks whether to trust `root`, the folder `buffer`'s
    /// language server would run for. Returns whether the window must be
    /// redrawn.
    pub(super) fn trust_asked(&mut self, buffer: BufferId, root: String) -> bool {
        let questions = &mut self.trust.asked;
        if questions.iter().any(|question| question.root == root) {
            return false;
        }
        let asked = Confirm::new(format!(
            "Trust {}? Its language servers may run its code (y/n)",
            shown(&root)
        ))
        .choice('y', "Trust the folder: start its language servers", true)
        .choice('n', "Don't trust it, and don't ask again", false);
        questions.push(Question {
            buffer,
            root,
            asked,
        });
        true
    }

    /// The question asked now, if any: the first about a folder whose
    /// file the focused editor shows, in normal mode with nothing half
    /// typed, and neither the command line nor the finder open.
    fn trust_question(&self) -> Option<&Question> {
        if self.trust.asked.is_empty()
            || self.input.is_some()
            || self.finding()
            || self.focused_mode() != Some(Mode::Normal)
            || self.pane_pending()
            || !self.keymap.waiting().is_empty()
        {
            return None;
        }
        let (editor, copy) = self.focused_editor()?;
        let path = Path::new(copy.path());
        (self.trust.asked.iter())
            .find(|question| question.buffer == editor.buffer() || path.starts_with(&question.root))
    }

    /// Whether the question is asked now, so `press` goes to it.
    pub(super) fn trust_asking(&self) -> bool {
        self.trust_question().is_some()
    }

    /// Answers the question asked, if `press` does. Returns whether
    /// `press` was taken.
    pub(super) fn trust_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(question) = self.trust_question() else {
            return false;
        };
        // Only a key the user pressed now: a macro's or a binding's goes
        // on as it would, and the question waits.
        if self.script.typing || !self.key_pressed_now() {
            return false;
        }
        let strokes = crate::vim::plain_strokes(press, false);
        let [stroke] = strokes[..] else {
            return false;
        };
        let answer = question.asked.answer(stroke);
        let just_shown =
            (self.trust.shown_since.get()).is_none_or(|since| since.elapsed() < ANSWER_PAUSE);
        if answer.is_some() && just_shown {
            self.say("a question just came: press the key again to answer it".into());
            return true;
        }
        let root = question.root.clone();
        let buffer = question.buffer;
        self.trust.asked.retain(|question| question.root != root);
        self.trust.shown_since.set(None);
        let Some(trusted) = answer else {
            self.say(format!(
                "{} isn't trusted yet: :trust runs its language servers",
                shown(&root)
            ));
            return false;
        };
        let action = if trusted {
            TrustAction::Trust
        } else {
            TrustAction::Untrust
        };
        let trust = Request::Trust {
            action,
            folder: Some(root),
            buffer: Some(buffer),
        };
        self.request(trust, Asked::Trusted);
        true
    }

    /// What Ctrl+? says the question's keys do.
    pub(super) fn describe_trust_key(
        &self,
        press: &KeyPress<'_>,
    ) -> crate::shortcuts::bindings::Hint {
        let strokes = crate::vim::plain_strokes(press, false);
        match (self.trust_question(), &strokes[..]) {
            (Some(question), [stroke]) => question.asked.hint(*stroke),
            _ => crate::shortcuts::bindings::Hint::Text("Cancel"),
        }
    }

    /// The question asked, for the bar.
    pub(super) fn trust_status(&self) -> Option<Status> {
        let Some(question) = self.trust_question() else {
            self.trust.shown_since.set(None);
            return None;
        };
        if self.trust.shown_since.get().is_none() {
            self.trust.shown_since.set(Some(Instant::now()));
        }
        Some(Status::Typing {
            label: None,
            text: question.asked.question().to_owned(),
            choices: Vec::new(),
            selected: None,
            under: None,
        })
    }

    /// Carries out `:trust`, `:untrust` or `:trusted`: `folder`, from the
    /// focused file's directory, or with none, the focused file's project,
    /// trusted to run its language servers, or not, or the folders marked
    /// listed. Only a command typed with keys pressed now trusts one.
    pub(super) fn trust_command(&mut self, action: TrustAction, folder: Option<String>) {
        if !self.needs_daemon(":trust", wire::TRUST) {
            return;
        }
        if action == TrustAction::Trust && !self.typed_now {
            self.refuse("only a :trust you type trusts a folder".into());
            return;
        }
        let buffer = self.focused_editor().map(|(editor, _)| editor.buffer());
        if action != TrustAction::List && folder.is_none() && buffer.is_none() {
            self.refuse("name a folder, or focus a file of its project".into());
            return;
        }
        let trust = Request::Trust {
            action,
            folder,
            buffer,
        };
        self.request(trust, Asked::Trusted);
    }
}

/// `root` as the bar shows it: every character that would hide what's
/// around it, or move it, shown by its number.
fn shown(root: &str) -> String {
    let mut shown = String::with_capacity(root.len());
    for ch in root.chars() {
        if super::shown_in_names(ch) {
            shown.push(ch);
        } else {
            let _ = write!(shown, "<U+{:04X}>", u32::from(ch));
        }
    }
    shown
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_folder_is_shown_with_nothing_hidden() {
        assert_eq!(super::shown("/src/a\u{202e}b"), "/src/a<U+202E>b");
        assert_eq!(super::shown("/src/plain"), "/src/plain");
    }
}
