//! Specialist selection, invocation visibility, and host-owned time budgets.

use super::Daemon;
use crate::specialists::Specialist;
use clackatoo_protocol::{Error, TerminalId};
use std::time::{Duration, Instant};

impl Daemon {
    pub(super) fn specialists_for(&self, chat: TerminalId) -> Vec<Specialist> {
        let mut all = crate::specialists::built_in();
        if let Some(catalogue) = self.skill_catalogue(chat) {
            for specialist in catalogue.specialists.values() {
                if let Some(old) = all.iter_mut().find(|s| s.name == specialist.name) {
                    *old = specialist.clone();
                } else {
                    all.push(specialist.clone());
                }
            }
        }
        all
    }

    pub(super) fn specialist_for(
        &self,
        chat: TerminalId,
        name: &str,
        delegated: bool,
    ) -> Result<Specialist, String> {
        let available = self.specialists_for(chat);
        let specialist = available
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .ok_or_else(|| {
                let names = available
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("there's no specialist called {name}: the specialists are {names}")
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

    pub(super) fn next_specialist_due(&self) -> Option<Instant> {
        let now = crate::chat::now();
        self.terminals
            .values()
            .filter_map(|slot| slot.terminal.chat_ref()?.specialist_deadline())
            .min()
            .map(|deadline| Instant::now() + Duration::from_millis(deadline.saturating_sub(now)))
    }

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
    use crate::specialists::Extras;

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
        let roots = daemon.skill_roots(dir.path().to_owned());
        daemon.refresh_specialist_catalogue(&roots, crate::skills::Catalogue::discover(&roots));
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
}
