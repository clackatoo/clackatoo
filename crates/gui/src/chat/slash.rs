//! The chat's `/` menu: typing `/` at the start of a message offers the
//! slash commands the agent says it has (its own, its skills and its MCP
//! servers' prompts), and those clackatoo does itself, narrowed as the
//! name is typed, each with what it does and what to type after it
//! (CLACKA-189). It's made of what every completion menu is made of (see
//! [`crate::complete`]): the ranking, and which is picked.
//!
//! | Key | Does |
//! |---|---|
//! | Down, Ctrl+N; Up, Ctrl+P | The next or previous command, round the ends |
//! | Tab | Puts the command picked in, and a space after it |
//! | Enter | The same, or if it's all typed, sends it |
//! | Esc | Closes the menu, until what's typed changes |
//!
//! Claude Code's and Codex's own interfaces have commands their adapters
//! don't (see [`clackatoo_protocol::INTERFACE_COMMANDS`]): sent, they'd
//! reach the model as words. So a message that's one of those, and not a
//! command the agent offers, isn't sent: the window does what it's for
//! (see [`Native`]), or says what to use instead. One the agent offers
//! is always the agent's: `/compact`, `/init` and `/review` are sent.

use clackatoo_protocol::{AgentCommand, SettingKind, interface_command};
use winit::keyboard::{Key, NamedKey};

use super::Settled;
use crate::complete::{Choice, Ranking};
use crate::fonts::Fonts;
use crate::input::Modifiers;
use crate::pane::Copy;
use crate::shortcuts::bindings::Hint;

/// The most commands the menu shows at once; more are scrolled to.
pub(super) const HEIGHT: usize = 8;

/// What the window does in place of sending one of the commands only
/// Claude Code's and Codex's own interfaces have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Native {
    /// `/clear`, `/new`: the start page, to start a new chat.
    StartPage,
    /// `/resume`: the start page too, which lists the agent's past
    /// sessions under the new chat.
    Sessions,
    /// `/rewind`: undoes what the last turn changed in files, as `u` does.
    Undo,
    /// `/context`: how full the agent's context is, as `K` shows it.
    Context,
    /// `/status`, `/cost`, `/usage`: the account's usage limits.
    Usage,
    /// `/diff`: `:Review`, of what the chat's agents changed.
    Review,
    /// `/model`: the model menu.
    Model,
    /// `/export`: the conversation, as text, on the clipboard.
    Export,
    /// One with nothing like it here: the bar says what to use instead.
    Say(&'static str),
}

/// The commands the window does itself, in the order the menu offers
/// them, each with what it says it does there.
const OURS: &[(&str, &str, Native)] = &[
    (
        "clear",
        "Start a new chat, from the start page",
        Native::StartPage,
    ),
    (
        "new",
        "Start a new chat, from the start page",
        Native::StartPage,
    ),
    (
        "resume",
        "Go on with a past session, from the start page",
        Native::Sessions,
    ),
    (
        "rewind",
        "Undo what the last turn changed in files, as u does",
        Native::Undo,
    ),
    (
        "context",
        "Show how full the agent's context is",
        Native::Context,
    ),
    ("status", "Show your account's usage limits", Native::Usage),
    ("cost", "Show your account's usage limits", Native::Usage),
    ("usage", "Show your account's usage limits", Native::Usage),
    (
        "diff",
        "Review what agents changed, in :Review",
        Native::Review,
    ),
    (
        "model",
        "Pick the model, from the model menu",
        Native::Model,
    ),
    (
        "export",
        "Copy the conversation to the clipboard",
        Native::Export,
    ),
];

/// What the window says of a command of the agent's own interface it
/// has nothing like, naming what to use instead.
fn instead(name: &str) -> &'static str {
    match name {
        "exit" | "quit" => ":q closes the pane, and :bd the chat for good",
        "help" => "Ctrl+? shows the keys here, and :Tutor teaches them",
        "config" | "theme" | "vim" | "terminal-setup" => {
            "clackatoo's options are :set, and the agent's settings the chips under the box"
        }
        "permissions" => "the mode chip under the box, or Shift+Tab, says what the agent may do",
        "login" | "logout" => "run the agent's own command in a terminal to sign in or out",
        _ => "clackatoo doesn't send it",
    }
}

/// What the window does with `text`, a message to a chat whose agent
/// offers `offered`, in place of sending it, if it's one of the commands
/// only the agent's own interface has (see [`interface_command`]).
///
/// It's one only as the whole message, `/name` alone: with words after
/// it, as `/help me find why this fails`, it's words for the agent. The
/// one exception is `/model NAME`, whose name the window may pick (see
/// [`ChatView::native_command`](super::ChatView::native_command)), so
/// what follows is given back.
pub(crate) fn native<'a>(
    text: &'a str,
    offered: &[AgentCommand],
) -> Option<(&'static str, Native, &'a str)> {
    let (name, rest) = interface_command(text, offered)?;
    let native = (OURS.iter())
        .find(|(ours, ..)| *ours == name)
        .map_or_else(|| Native::Say(instead(name)), |&(_, _, native)| native);
    (rest.is_empty() || native == Native::Model).then_some((name, native, rest))
}

/// One of the commands the menu offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) name: String,
    pub(crate) description: String,
    /// What to type after it, if it takes anything.
    pub(crate) hint: Option<String>,
    /// Whether the window does it, rather than the agent.
    pub(crate) ours: bool,
}

/// The menu: what it offers, what of it matches the name typed, and
/// which is picked.
#[derive(Debug, Default)]
pub(crate) struct Slash {
    /// The agent's commands the items were made from, to make them again
    /// once it says others.
    from: Vec<AgentCommand>,
    items: Vec<Item>,
    ranking: Option<Ranking>,
    choice: Choice,
    /// What's typed after the `/`, while the menu's open.
    typed: Option<String>,
    /// The message as it was when Esc closed the menu, which stays closed
    /// until it changes.
    closed: Option<String>,
}

impl Slash {
    /// Takes in the message being typed, `text`, to a chat whose agent
    /// offers `offered`: the menu opens if it's a `/` and the start of a
    /// command's name, with nothing after it, and narrows to the commands
    /// that match. `typing` is whether the box takes keys now: in normal
    /// mode, or while a question waits, it's closed.
    pub(crate) fn typed(&mut self, text: &str, offered: &[AgentCommand], typing: bool) {
        if self.closed.as_deref().is_some_and(|closed| closed != text) {
            self.closed = None;
        }
        let name = text
            .strip_prefix('/')
            .filter(|name| typing && self.closed.is_none() && !name.contains(char::is_whitespace));
        let Some(name) = name else {
            self.typed = None;
            return;
        };
        if self.ranking.is_none() || self.from != offered {
            self.offer(offered);
        }
        let Some(ranking) = &mut self.ranking else {
            return;
        };
        let before = (ranking.shown().first().copied(), ranking.shown().len());
        ranking.narrow(name);
        if (ranking.shown().first().copied(), ranking.shown().len()) != before {
            self.choice.restart();
        }
        self.typed = Some(name.to_owned());
    }

    /// Makes the items again from `offered`, the agent's commands, then
    /// those of the window's own the agent doesn't offer one of the name
    /// of.
    fn offer(&mut self, offered: &[AgentCommand]) {
        offered.clone_into(&mut self.from);
        self.items = (offered.iter())
            .map(|command| Item {
                name: command.name.clone(),
                description: command.description.clone(),
                hint: command.hint.clone(),
                ours: false,
            })
            .collect();
        for &(name, description, _) in OURS {
            if !offered.iter().any(|command| command.name == name) {
                self.items.push(Item {
                    name: name.to_owned(),
                    description: description.to_owned(),
                    hint: None,
                    ours: true,
                });
            }
        }
        // In the order given until a name is typed: the agent's first.
        let order: Vec<String> = (0..self.items.len()).map(|at| format!("{at:05}")).collect();
        let mut ranking = Ranking::new(
            (self.items.iter().zip(&order))
                .map(|(item, order)| (item.name.as_str(), order.as_str(), false)),
        );
        ranking.keep_given_order();
        self.ranking = Some(ranking);
        self.choice.restart();
    }

    /// Whether the menu shows: it's open, and something matches.
    pub(crate) fn showing(&self) -> bool {
        self.typed.is_some() && self.ranking.as_ref().is_some_and(|r| !r.shown().is_empty())
    }

    /// Closes the menu, as Esc does, until the message, `text`, changes.
    pub(crate) fn close(&mut self, text: &str) {
        self.closed = Some(text.to_owned());
        self.typed = None;
    }

    /// Moves the pick to the next command, or the one before if not
    /// `down`, round the ends.
    pub(crate) fn step(&mut self, down: bool) {
        let count = self.ranking.as_ref().map_or(0, |r| r.shown().len());
        self.choice.step(down, count, HEIGHT);
    }

    /// The command picked, if the menu shows.
    pub(crate) fn picked(&self) -> Option<&Item> {
        let ranking = self.ranking.as_ref().filter(|_| self.showing())?;
        let index = *ranking.shown().get(self.choice.selected())?;
        self.items.get(index as usize)
    }

    /// What's typed after the `/`, while the menu's open.
    pub(crate) fn name_typed(&self) -> Option<&str> {
        self.typed.as_deref()
    }

    /// The commands in view, at most `height`, each with the characters
    /// of its name that match what's typed and whether it's picked; and
    /// how many match in all.
    pub(crate) fn in_view(&mut self, height: usize) -> (Vec<(Item, Vec<u32>, bool)>, usize) {
        let Some(ranking) = self.ranking.as_mut().filter(|_| self.typed.is_some()) else {
            return (Vec::new(), 0);
        };
        let count = ranking.shown().len();
        let view = self.choice.in_view(count, height);
        let picked = self.choice.selected();
        let shown: Vec<u32> = ranking.shown()[view.clone()].to_vec();
        let rows = (view.zip(shown))
            .map(|(place, index)| {
                let item = self.items[index as usize].clone();
                let matched = ranking.matched(&item.name);
                (item, matched, place == picked)
            })
            .collect();
        (rows, count)
    }

    /// Picks the command `row` rows down those in view, as a click does.
    pub(crate) fn click(&mut self, row: usize) {
        let count = self.ranking.as_ref().map_or(0, |r| r.shown().len());
        let place = self.choice.top() + row;
        if place < count {
            self.choice.go_to(place, HEIGHT);
        }
    }
}

impl super::ChatView {
    /// Takes in the message being typed and the agent's commands, as the
    /// `/` menu offers them, once either may have changed.
    pub(crate) fn refresh_slash(&mut self) {
        let typing = !self.reading
            && self.node.question.is_none()
            && self.menu.is_none()
            && self.handoff_sent.is_none()
            && !self.viewing_modal()
            && !self.recalling();
        (self.slash).typed(self.editor.raw_text(), &self.node.commands, typing);
        self.refresh_mentions();
    }

    crate::shortcuts::bindings::described_keys! {
        pub(super) fn slash_key / describe_slash_key(&mut self, key: &Key, modifiers: Modifiers) -> bool;
        execute(fonts: &mut Fonts);
        prepare {
            let (shift, ctrl) = (modifiers.shift, modifiers.ctrl);
            let letter = match key {
                Key::Character(text) if ctrl => text.chars().next().map(|ch| ch.to_ascii_lowercase()),
                _ => None,
            };
        }
        match (key, letter) => {
            _ if !self.slash.showing() =>
                Hint::None,
                false;
            (Key::Named(NamedKey::ArrowDown), _) | (_, Some('n')) if !shift =>
                Hint::Action("Next command"),
                {
                    self.slash.step(true);
                    true
                };
            (Key::Named(NamedKey::ArrowUp), _) | (_, Some('p')) if !shift =>
                Hint::Action("Previous command"),
                {
                    self.slash.step(false);
                    true
                };
            (Key::Named(NamedKey::Tab), _) if !shift && !ctrl =>
                Hint::Action("Complete the command"),
                self.complete_slash(fonts);
            (Key::Named(NamedKey::Enter), _) if !shift && !ctrl && !self.slash_typed_whole() =>
                Hint::Action("Complete the command"),
                self.complete_slash(fonts);
            (Key::Named(NamedKey::Escape), _) =>
                Hint::Action("Close the command menu"),
                {
                    self.slash.close(self.editor.raw_text());
                    true
                };
            _ =>
                Hint::None,
                false;
        }
        result
    }

    /// Opens the model menu, as clicking the model's chip under the box
    /// does, unless it's open; or says there's none.
    fn open_model_menu(&mut self, settled: &mut Settled) {
        let of = |kind| (self.node.settings.iter()).find(|setting| setting.kind == kind);
        let Some(model) = of(SettingKind::Model) else {
            let agent = self.agent().to_owned();
            self.say(format!("{agent} offers no models to pick from"));
            return;
        };
        let ids: Vec<String> = (std::iter::once(model).chain(of(SettingKind::Effort)))
            .map(|setting| setting.id.clone())
            .collect();
        if self.menu.as_ref().is_some_and(|menu| menu.settings == ids) {
            return;
        }
        if self.toggle_menu(ids) {
            settled
                .actions
                .push(clackatoo_protocol::ChatAction::LearnModels);
        }
    }

    /// Whether what's typed is all of the command picked, so Enter sends
    /// it rather than completing it.
    fn slash_typed_whole(&self) -> bool {
        let picked = self.slash.picked().map(|item| item.name.as_str());
        picked.is_some() && picked == self.slash.name_typed()
    }

    /// Puts the command picked in the box, in place of what's typed, with
    /// a space after it for what follows. Returns whether one was.
    #[allow(
        clippy::redundant_closure_for_method_calls,
        reason = "the method itself isn't general enough over the driver's lifetime"
    )]
    pub(super) fn complete_slash(&mut self, fonts: &mut Fonts) -> bool {
        let Some(name) = self.slash.picked().map(|item| item.name.clone()) else {
            return false;
        };
        self.editor.set_text(&format!("/{name} "));
        self.drive(fonts, |driver| driver.move_to_text_end());
        self.refresh_slash();
        true
    }

    /// Does what the message typed is for, in place of sending it, if
    /// it's one of the commands only the agent's own interface has (see
    /// [`native`]), and empties the box. Returns whether it was one.
    ///
    /// The box is emptied only once something's done: `/exit`, which has
    /// nothing like it here, says what to use and keeps what's typed, and
    /// `/model NAME` naming no model the agent has is sent as words.
    pub(super) fn native_command(&mut self, settled: &mut Settled) -> bool {
        use crate::command::{Command, Kind, Place, ReviewCommand};
        use clackatoo_protocol::ReviewScope;
        let typed = self.editor.raw_text().to_owned();
        let Some((name, native, rest)) = native(&typed, &self.node.commands) else {
            return false;
        };
        if !rest.is_empty() && !self.pick_model(rest, settled) {
            return false;
        }
        if let Native::Say(instead) = native {
            self.say(format!(
                "/{name} is the agent's own interface's, so it isn't sent: {instead}"
            ));
            return true;
        }
        self.editor.set_text("");
        self.refresh_slash();
        if !rest.is_empty() {
            return true;
        }
        let agent = self.agent().to_owned();
        match native {
            Native::StartPage | Native::Sessions => settled.commands.push(Command::Open {
                place: Place::Tab,
                kind: Kind::Agent,
                agent: None,
            }),
            Native::Undo => self.undo_turn(self.turn_to_undo(false), false),
            Native::Context => {
                if !self.usage_modal
                    && self.detail != Some(super::paint::Detail::Asked)
                    && !self.toggle_detail()
                {
                    self.say(format!("{agent} hasn't said how full its context is yet"));
                }
            }
            Native::Usage if self.usage_supported => {
                if !self.usage_modal {
                    self.open_usage();
                }
            }
            Native::Usage => {
                self.say("usage limits need a newer clackatoo daemon: update it".into());
            }
            Native::Review => settled.commands.push(Command::Review(ReviewCommand::Open {
                scope: ReviewScope::Chat,
                side: None,
            })),
            Native::Model => self.open_model_menu(settled),
            Native::Export => {
                let text = self.node.text();
                let turns = self.node.turns.len();
                settled.copied.push((text, Copy::Clipboard));
                let older = if self.node.first_turn > 0 {
                    " (older turns aren't loaded: scroll up for them first)"
                } else {
                    ""
                };
                self.say(format!(
                    "copied the conversation, {turns} turns, to the clipboard{older}"
                ));
            }
            Native::Say(_) => {}
        }
        true
    }

    /// Picks the agent's model `named`, its value or its name, ignoring
    /// case, as `/model opus` does. Returns whether there's one so named.
    fn pick_model(&mut self, named: &str, settled: &mut Settled) -> bool {
        let Some(setting) = (self.node.settings.iter()).find(|s| s.kind == SettingKind::Model)
        else {
            return false;
        };
        let Some(choice) = (setting.choices.iter()).find(|choice| {
            choice.value.eq_ignore_ascii_case(named) || choice.name.eq_ignore_ascii_case(named)
        }) else {
            return false;
        };
        let (id, value, name) = (
            setting.id.clone(),
            choice.value.clone(),
            choice.name.clone(),
        );
        self.choose(id, value, settled);
        self.say(format!("model: {name}"));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(name: &str, hint: Option<&str>) -> AgentCommand {
        AgentCommand {
            name: name.into(),
            description: format!("Does {name}"),
            hint: hint.map(str::to_owned),
        }
    }

    fn names(slash: &mut Slash) -> Vec<String> {
        let (rows, _) = slash.in_view(HEIGHT);
        rows.into_iter().map(|(item, ..)| item.name).collect()
    }

    #[test]
    fn a_slash_alone_offers_the_agents_commands_then_the_windows_own() {
        let offered = [command("compact", None), command("clear", None)];
        let mut slash = Slash::default();
        slash.typed("/", &offered, true);
        assert!(slash.showing());
        let shown = names(&mut slash);
        assert_eq!(&shown[..3], ["compact", "clear", "new"]);
        // The agent's /clear is the agent's: offered once, not as ours.
        let (rows, count) = slash.in_view(100);
        assert_eq!(
            rows.iter()
                .filter(|(item, ..)| item.name == "clear")
                .count(),
            1
        );
        assert!(!rows[1].0.ours && rows[2].0.ours);
        assert_eq!(count, OURS.len() + 1);
    }

    #[test]
    fn typing_narrows_and_anything_after_the_name_closes_it() {
        let offered = [command("review", Some("what")), command("compact", None)];
        let mut slash = Slash::default();
        slash.typed("/rev", &offered, true);
        assert_eq!(names(&mut slash), ["review"]);
        assert_eq!(slash.picked().unwrap().hint.as_deref(), Some("what"));
        slash.typed("/review ", &offered, true);
        assert!(!slash.showing());
        slash.typed("hello /re", &offered, true);
        assert!(!slash.showing());
        slash.typed("/zzz", &offered, true);
        assert!(!slash.showing());
        // Nor in normal mode, or while a question waits.
        slash.typed("/rev", &offered, false);
        assert!(!slash.showing());
    }

    #[test]
    fn esc_closes_it_until_the_message_changes() {
        let offered = [command("compact", None)];
        let mut slash = Slash::default();
        slash.typed("/co", &offered, true);
        slash.close("/co");
        slash.typed("/co", &offered, true);
        assert!(!slash.showing());
        slash.typed("/com", &offered, true);
        assert!(slash.showing());
    }

    #[test]
    fn picking_goes_round_and_new_commands_start_it_again() {
        let mut offered = vec![command("zz1", None), command("zz2", None)];
        let mut slash = Slash::default();
        slash.typed("/zz", &offered, true);
        slash.step(false);
        assert_eq!(slash.picked().unwrap().name, "zz2");
        slash.step(true);
        assert_eq!(slash.picked().unwrap().name, "zz1");
        offered.push(command("zz3", None));
        slash.typed("/zz", &offered, true);
        assert_eq!(names(&mut slash), ["zz1", "zz2", "zz3"]);
        slash.click(2);
        assert_eq!(slash.picked().unwrap().name, "zz3");
    }

    #[test]
    fn the_interfaces_commands_are_done_here_unless_the_agent_offers_them() {
        assert_eq!(
            native("/clear", &[]),
            Some(("clear", Native::StartPage, ""))
        );
        assert_eq!(native("/Diff ", &[]), Some(("diff", Native::Review, "")));
        assert!(matches!(
            native("/exit", &[]),
            Some(("exit", Native::Say(_), ""))
        ));
        // With words after it, it's words for the agent; but `/model`'s
        // name is the window's to pick.
        assert_eq!(native("/help me find why this test fails", &[]), None);
        assert_eq!(native("/clear the cache, then rerun", &[]), None);
        assert_eq!(
            native("/model opus", &[]),
            Some(("model", Native::Model, "opus"))
        );
        assert_eq!(native("/compact", &[]), None);
        assert_eq!(native("/clear", &[command("clear", None)]), None);
        assert_eq!(native("not /clear", &[]), None);
    }
}
