//! The specialists a chat may be started as: which there are, who may
//! ask for each, whether the agent can hold a chat to one's budget, and
//! the time budgets clackatoo holds itself.

use super::Daemon;
use crate::skills::Catalogue;
use crate::specialists::{Budget, Specialist, Unenforced};
use clackatoo_protocol::{Error, TerminalId};
use std::time::{Duration, Instant};

/// The specialists there are with `catalogue`: clackatoo's own, then
/// those the catalogue found. One of the user's own with the name of one
/// built in takes its place; a project's never does, and isn't in the
/// catalogue to (see `skills::skillsets::discover`).
pub(super) fn with_built_in(catalogue: Option<&Catalogue>) -> Vec<Specialist> {
    let mut all = crate::specialists::built_in();
    for specialist in catalogue
        .iter()
        .flat_map(|found| found.specialists.values())
    {
        match all.iter_mut().find(|old| old.name == specialist.name) {
            Some(old) => *old = specialist.clone(),
            None => all.push(specialist.clone()),
        }
    }
    all
}

impl Daemon {
    /// The specialists there are for the chat `chat`.
    pub(super) fn specialists_for(&self, chat: TerminalId) -> Vec<Specialist> {
        with_built_in(self.skill_catalogue(chat))
    }

    /// Whether the agent called `name` holds a session to a number of
    /// turns and a cost. One that's running is what a chat would be
    /// given, so it goes by how that one was started; with none running,
    /// by how the config would start one now. The two differ once the
    /// config changes under an agent that goes on running.
    pub(super) fn holds_budgets(&self, name: &str) -> bool {
        let running = (self.agents.named(name)).and_then(|agent| self.agents.running.get(&agent));
        match running {
            Some(slot) => slot.holds_budgets(),
            None => self.config.agents.holds_budgets(name),
        }
    }

    /// Checks the agent called `agent` can hold a chat to `budget`.
    pub(super) fn check_budget(&self, budget: Budget, agent: &str) -> Result<(), Unenforced> {
        if budget.needs_adapter() && !self.holds_budgets(agent) {
            return Err(Unenforced {
                agent: agent.to_owned(),
            });
        }
        Ok(())
    }

    /// The specialist called `name`, for the chat `chat` to start a chat
    /// as, if `delegated`, or else for the user to choose there; or why
    /// not, as the agent or the user is told.
    pub(super) fn specialist_for(
        &self,
        chat: TerminalId,
        name: &str,
        delegated: bool,
    ) -> Result<Specialist, String> {
        let available = self.specialists_for(chat);
        let found = available.iter().find(|s| s.name == name).cloned();
        let specialist = found.ok_or_else(|| {
            let names: Vec<_> = available.iter().map(|s| s.name.as_str()).collect();
            let names = names.join(", ");
            // One that was found, but can't be used, says why.
            let unloaded =
                (self.skill_catalogue(chat)).and_then(|catalogue| catalogue.unloaded.get(name));
            match unloaded {
                Some(unloaded) => format!(
                    "the specialist {name} ({}) is {}. The specialists are {names}",
                    unloaded.path.display(),
                    unloaded.why
                ),
                None => {
                    format!("there's no specialist called {name}: the specialists are {names}")
                }
            }
        })?;
        if delegated {
            if !specialist.extra.agent_invocable {
                return Err(format!("Specialist {name} cannot be invoked by agents"));
            }
            if let Some(allowed) = self
                .chat_ref(chat)
                .and_then(|chat| chat.specialist_definition())
                .and_then(|s| s.extra.delegates_to.as_ref())
                && !allowed.iter().any(|allowed| allowed == name)
            {
                return Err(format!("This specialist cannot delegate to {name}"));
            }
        } else if !specialist.extra.user_invocable {
            return Err(format!(
                "Specialist {name} is only available for delegation"
            ));
        }
        Ok(specialist)
    }

    /// Starts the chat `chat` as the specialist called `name`, as the
    /// user chose: with no fewer limits than the chat had, or the chat
    /// that started it has.
    pub(super) fn select_specialist(&mut self, chat: TerminalId, name: &str) -> Result<(), Error> {
        let mut specialist = self
            .specialist_for(chat, name, false)
            .map_err(Error::Other)?;
        let own = self.chat_ref(chat).ok_or(Error::NotAChat(chat))?;
        let parent = own.relations().parent.and_then(|id| self.chat_ref(id));
        let floor = parent.map_or_else(
            || {
                if own.agents_chat() {
                    own.limits()
                } else {
                    crate::specialists::Limits::None
                }
            },
            crate::chat::HostedChat::all_limits,
        );
        specialist.extra.budget = specialist.extra.budget.capped(
            parent.map_or(crate::specialists::Budget::default(), |p| {
                p.specialist_budget()
            }),
        );
        let limits = specialist.limits.max(floor);
        self.check_budget(specialist.extra.budget, own.agent())
            .map_err(|unenforced| Error::Other(unenforced.to_string()))?;
        let actions = self
            .chat(chat)
            .ok_or(Error::NotAChat(chat))?
            .choose_specialist(specialist, limits)
            .map_err(Error::Other)?;
        self.forget_skill_catalogue(chat);
        self.act(chat, actions);
        self.collect_damage(chat);
        Ok(())
    }

    /// When the first request with a time budget runs out of it.
    pub(super) fn next_specialist_due(&self) -> Option<Instant> {
        let now = crate::chat::now();
        self.terminals
            .values()
            .filter_map(|slot| slot.terminal.chat_ref()?.specialist_deadline())
            .min()
            .map(|deadline| Instant::now() + Duration::from_millis(deadline.saturating_sub(now)))
    }

    /// Cancels the requests whose time budget has run out.
    pub(super) fn take_specialist_limits(&mut self) {
        let now = crate::chat::now();
        let expired: Vec<_> = self
            .terminals
            .iter()
            .filter_map(|(&id, slot)| {
                (slot.terminal.chat_ref()?.specialist_deadline()? <= now).then_some(id)
            })
            .collect();
        for id in expired {
            if let Some(chat) = self.chat(id) {
                let actions = chat.specialist_timed_out();
                self.act(id, actions);
                self.collect_damage(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;
    use crate::specialists::{Budget, Extras};

    #[test]
    fn invocation_controls_and_delegation_allowlists_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, id) = daemon_with_chat(dir.path());
        for (name, policy) in [
            ("private", "user_invocable: false"),
            ("manual", "agent_invocable: false"),
        ] {
            let root = dir.path().join(".agents/specialists").join(name);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("SPECIALIST.md"),
                format!("---\nname: {name}\n{policy}\n---\nReview\n"),
            )
            .unwrap();
        }
        daemon.read_skills_now(id);
        assert!(daemon.specialist_for(id, "private", true).is_ok());
        assert!(daemon.specialist_for(id, "private", false).is_err());
        assert!(daemon.specialist_for(id, "manual", true).is_err());
        assert!(daemon.specialist_for(id, "manual", false).is_ok());
        daemon.chat(id).unwrap().remember_specialist(Specialist {
            name: "lead".into(),
            extra: Extras {
                delegates_to: Some(vec!["explore".into()]),
                ..Extras::default()
            },
            ..Specialist::default()
        });
        assert!(daemon.specialist_for(id, "explore", true).is_ok());
        assert!(
            daemon
                .specialist_for(id, "general", true)
                .unwrap_err()
                .contains("cannot delegate")
        );
    }

    /// A specialist that was found but can't be used says why to whoever
    /// asks for it, rather than that there's none of the name.
    #[test]
    fn asking_for_a_broken_specialist_says_whats_wrong_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, id) = daemon_with_chat(dir.path());
        let root = dir.path().join(".agents/specialists/reviewer");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("SPECIALIST.md"),
            "---\nname: reviewer\nlimit: read-only\n---\nReview\n",
        )
        .unwrap();
        daemon.read_skills_now(id);
        let why = daemon.specialist_for(id, "reviewer", true).unwrap_err();
        assert!(why.contains("unsupported specialist field limit"), "{why}");
        assert!(why.contains(root.to_str().unwrap()), "{why}");
        let why = daemon.specialist_for(id, "absent", true).unwrap_err();
        assert!(
            why.starts_with("there's no specialist called absent"),
            "{why}"
        );
    }

    /// Turn and cost budgets are held only by an adapter clackatoo
    /// installs and starts itself: not by a command of the user's called
    /// `claude`, whatever it would say it is.
    #[test]
    fn budgets_go_by_the_adapter_and_not_the_agents_name() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let turns = Budget {
            turns: Some(3),
            ..Budget::default()
        };
        let seconds = Budget {
            seconds: Some(30),
            ..Budget::default()
        };
        assert!(daemon.check_budget(turns, "claude").is_ok());
        assert!(daemon.check_budget(turns, "codex").is_err());
        assert!(daemon.check_budget(seconds, "codex").is_ok());
        daemon.config.agents.custom = vec![crate::agents::CustomAgent {
            name: "claude".into(),
            title: None,
            starting_mode: None,
            tasks: None,
            command: vec!["my-own-agent".into()],
        }];
        let refused = daemon.check_budget(turns, "claude").unwrap_err();
        assert!(refused.to_string().contains("cannot enforce"));
        assert!(refused.to_string().contains("Claude Code"));
    }
}
