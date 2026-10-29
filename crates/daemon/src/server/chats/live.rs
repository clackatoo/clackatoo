//! Agents whose processes go on through an upgrade (CLACKA-188), rather
//! than being stopped: each adapter's pipes and pidfd go to the next build
//! as the PTYs' do, with the requests it hasn't answered, what it said it
//! can do, and what's half read of it. It never knows: its background
//! shells, a deploy it waits for, its turns and its tools' calls all go
//! on. Its chats go with it (see `chat/live.rs`), and the connections
//! their tools are served on (see `tools/live.rs`).
//!
//! The build upgrading decides, before it stops anything, which agents
//! go on: only if the next build says it takes them, at the same pinned
//! adapter, and only those that can as they are. Every other agent is
//! stopped, as before, with all it runs, and its chats are told so.

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::time::{Duration, Instant};

use clackatoo_protocol::TerminalId;
use mio::unix::SourceFd;

use super::{AgentId, Asked, Daemon, End, Flood, KILL_AFTER, Source};
use crate::agent::Agent;
use crate::agents::Launch;
use crate::chat::{HostedChat, Limits};
use crate::handover::{Fds, Live, LiveDue, LiveLimits, LiveOpening};
use crate::upgrade::Takes;

/// How long the upgrade waits for the parser's thread to give back what
/// it's parsing of the agents going on, and their tools' connections:
/// what's left with it by then can't be handed over, and that agent is
/// stopped, as before.
const PARSE_WAIT: Duration = crate::rpc::PARSE_WAIT;

/// How long an agent may leave a request it should answer soon unanswered
/// before it's taken to be stuck: one that is isn't carried on through an
/// upgrade, which stops it instead, so an upgrade still unsticks it.
const STUCK: Duration = Duration::from_mins(2);

impl Daemon {
    /// The agents that go on through an upgrade to a build that `takes`
    /// what it says (see [`Takes`]): what the parser's thread had of them,
    /// and of their chats' tools, has been taken in, so all that's left of
    /// each is bytes. None, if agents aren't to go on, or that build can't
    /// take them. It's asked before the file operations, the boards and
    /// the web app are made ready for the upgrade, as what's taken in may
    /// change any of them; what can't wait for the next build is answered
    /// once there's no way back (see [`ready_kept`](Self::ready_kept)).
    pub(in crate::server) fn agents_to_keep(&mut self, takes: &Takes) -> Vec<AgentId> {
        if self.agents.running.is_empty() {
            return Vec::new();
        }
        if takes.adapters.is_none() {
            log::info!("the agents stop: the next build can't take them as they are");
            return Vec::new();
        }
        if !self.config.agents.kept_through_upgrades() {
            log::info!("the agents stop: they're set to stop as the daemon upgrades");
            return Vec::new();
        }
        let candidates: Vec<AgentId> = (self.agents.running.keys().copied())
            .filter(|&id| self.can_keep(id, takes))
            .collect();
        self.take_in_for_upgrade(&candidates)
    }

    /// Answers, for the agents `keep`, which go on through the upgrade now
    /// there's no way back, what can't wait for the next build: listings
    /// clients asked for, whose connections close; their chats' calls
    /// waiting on the browser, which stops, or a window, which goes; and
    /// the forms they asked the user to fill in, as cancelled, which a
    /// window shows as it can't be written down.
    pub(in crate::server) fn ready_kept(&mut self, keep: &[AgentId]) {
        let names = self.agent_names(keep);
        for name in &names {
            self.fail_listings(name, "the daemon upgraded");
        }
        let chats = self.chats_of(&names);
        self.refuse_waits_that_cant_go_on(&chats);
        for id in chats {
            let cancelled = (self.chat(id)).map(HostedChat::cancel_forms_for_upgrade);
            if let Some(cancelled) = cancelled.filter(|actions| !actions.is_empty()) {
                self.act(id, cancelled);
                self.collect_damage(id);
            }
        }
    }

    /// Whether agent `id` can go on through an upgrade to a build that
    /// `takes` what it says, as far as can be told before what it sent
    /// is taken in: why not is logged.
    fn can_keep(&self, id: AgentId, takes: &Takes) -> bool {
        let Some(slot) = self.agents.running.get(&id) else {
            return false;
        };
        let name = &slot.spec.name;
        let chats: Vec<(TerminalId, &HostedChat)> = (self.terminals.iter())
            .filter_map(|(&id, slot)| Some((id, slot.terminal.chat_ref()?)))
            .filter(|(_, chat)| chat.agent() == name)
            .collect();
        // A relay connecting its chat's tools that hasn't become their
        // connection yet would be cut off: one accepted that hasn't said
        // what it's for may be one, of any chat, as an agent can start its
        // tools' relay whenever it likes. Relays try again once cut off
        // (see `mcp::relay`), but none is cut off that needn't be.
        let connecting = (self.sources.iter()).any(|(_, source)| {
            matches!(source, Source::Client(client)
            if matches!(client.link, super::super::Link::Unix(_))
                && !client.finished
                && (!client.asked
                    || client.serving_tools.is_some_and(|chat| {
                        chats.iter().any(|(id, _)| *id == chat)
                    })))
        });
        let files = crate::agent::client_files(name);
        let kind = crate::agent::declaration_kind(files, slot.agent.tells_turns());
        let hash = crate::agent::declaration_hash(slot.agent.declared());
        let refused = if chats.is_empty() {
            Some("no chat talks to it".to_owned())
        } else if chats.iter().any(|(_, chat)| !chat.can_go_on()) {
            Some("a chat of it is loading its session or moving".to_owned())
        } else if chats.iter().any(|(_, chat)| chat.is_opening()) {
            Some("a chat of it is opening its session".to_owned())
        } else if connecting {
            Some("a connection hasn't said what it's for, and may be its tools' relay".to_owned())
        } else if !slot.agent.started() {
            Some("it's still starting".to_owned())
        } else if slot.agent.has_gone() || slot.agent.has_exited() {
            Some("it has gone".to_owned())
        } else if (slot.agent.longest_unanswered()).is_some_and(|waited| waited > STUCK) {
            Some("it has stopped answering".to_owned())
        } else if !takes.takes_agent(name, pinned(&slot.spec.launch), (kind, &hash)) {
            Some(
                "the next build pins another version of its adapter, or declares otherwise to it"
                    .to_owned(),
            )
        } else if (slot.command.first().map(Path::new))
            .is_some_and(|program| program.is_absolute() && !program.exists())
        {
            Some("what it runs from is gone".to_owned())
        } else {
            None
        };
        if let Some(why) = &refused {
            log::info!("agent {name} stops for the upgrade: {why}");
        }
        refused.is_none()
    }

    /// Takes in what the parser's thread has parsed of each agent in
    /// `candidates`, and of its chats' tools' connections, waiting up to
    /// [`PARSE_WAIT`] for what it's still parsing, so all that's left of
    /// each is bytes, for the next build to go on reading. Returns those
    /// that got there: the rest are stopped.
    fn take_in_for_upgrade(&mut self, candidates: &[AgentId]) -> Vec<AgentId> {
        let deadline = Instant::now() + PARSE_WAIT;
        loop {
            let (mut ready, mut parsing) = (Vec::new(), Vec::new());
            for &id in candidates {
                match self.take_parsed(id) {
                    Some(true) => ready.push(id),
                    Some(false) => parsing.push(id),
                    // Gone meanwhile: it's stopped, as any gone is.
                    None => {}
                }
            }
            let names = self.agent_names(&ready);
            let chats = self.chats_of(&names);
            let stuck = self.take_in_tools(&chats);
            let (still, held): (Vec<AgentId>, Vec<AgentId>) = ready.into_iter().partition(|id| {
                let name = (self.agents.running.get(id)).map(|slot| slot.spec.name.as_str());
                !(self.terminals.iter()).any(|(chat, each)| {
                    stuck.contains(chat)
                        && (each.terminal.chat_ref()).is_some_and(|chat| Some(chat.agent()) == name)
                })
            });
            parsing.extend(held);
            if parsing.is_empty() || Instant::now() >= deadline {
                for name in self.agent_names(&parsing) {
                    log::info!(
                        "agent {name} stops for the upgrade: what it sent is still being parsed"
                    );
                }
                return still;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Takes in all the parser's thread has parsed of agent `id`, reading
    /// nothing more, as reading it would. Returns whether it can go on
    /// through an upgrade now, or `None` if it never will, as it has gone.
    fn take_parsed(&mut self, id: AgentId) -> Option<bool> {
        let slot = self.agents.running.get_mut(&id)?;
        let name = slot.spec.name.clone();
        let mut incoming = std::mem::take(&mut self.agents.incoming);
        let ready = slot.agent.take_parsed(&mut incoming);
        for message in incoming.drain(..) {
            self.receive_from_agent(id, &name, message);
        }
        self.agents.incoming = incoming;
        let slot = self.agents.running.get(&id)?;
        (!slot.agent.has_gone() && !slot.agent.has_exited()).then_some(ready)
    }

    /// The chats talking to the agents called `names`.
    pub(in crate::server) fn chats_of(&self, names: &[String]) -> Vec<TerminalId> {
        (self.terminals.iter())
            .filter_map(|(&id, slot)| {
                let chat = slot.terminal.chat_ref()?;
                names.iter().any(|name| name == chat.agent()).then_some(id)
            })
            .collect()
    }

    /// The names of the agents `ids`.
    pub(in crate::server) fn agent_names(&self, ids: &[AgentId]) -> Vec<String> {
        (ids.iter())
            .filter_map(|id| self.agents.running.get(id))
            .map(|slot| slot.spec.name.clone())
            .collect()
    }

    /// Hands the agents `keep` over to the next build, as they are, into
    /// `fds`: unwatched first, so nothing of them is left to this build's
    /// loop, and with when each of `chats`' sessions, being asked for, is
    /// slow, and when each lets its session go or ends the turn its agent
    /// took by itself.
    pub(in crate::server) fn hand_over_agents(
        &mut self,
        keep: &[AgentId],
        chats: &[TerminalId],
        fds: &mut Fds,
    ) -> Live {
        let mut live = Live {
            next_agent: self.agents.next,
            ..Live::default()
        };
        for &id in keep {
            let Some(slot) = self.agents.running.remove(&id) else {
                continue;
            };
            self.unread.retain(|&unread| unread != Flood::Agent(id));
            for token in slot.tokens {
                self.sources.remove(token.0);
            }
            // It went since it was chosen, as writing to it failed: it's
            // stopped as any other, and its chats, handed over to go on
            // with it, go on as theirs do (see `Daemon::resume`).
            if !slot.agent.can_hand_over() {
                log::info!("agent {} stops for the upgrade: it went", slot.spec.name);
                let process = slot.agent.stop();
                let fd = std::os::fd::AsRawFd::as_raw_fd(&process);
                let _ = self.poll.registry().deregister(&mut SourceFd(&fd));
                self.let_agent_go(process, &slot.spec.name, End::Gently(KILL_AFTER));
                continue;
            }
            let agent = &slot.agent;
            for fd in [
                agent.stdout_fd(),
                agent.stderr_fd(),
                agent.stdin_fd(),
                agent.exit_fd(),
            ] {
                if let Err(err) = self.poll.registry().deregister(&mut SourceFd(&fd)) {
                    log::warn!("agent {}: unwatching it failed: {err}", slot.spec.name);
                }
            }
            let mut handed = slot.agent.hand_over(fds);
            handed.id = id.0;
            handed.name = slot.spec.name;
            handed.command = (slot.command.into_iter()).map(OsString::into_vec).collect();
            live.agents.push(handed);
        }
        // What's known of each agent kept, by its name, which its next
        // session would only say again.
        let names: Vec<String> = live.agents.iter().map(|agent| agent.name.clone()).collect();
        for name in names {
            if let Some(settings) = self.agents.settings.get(&name) {
                let settings = (settings.iter().cloned())
                    .map(crate::chat::setting_to_handover)
                    .collect();
                live.settings.push((name.clone(), settings));
            }
            if let Some(&context) = self.agents.context.get(&name) {
                live.context.push((name.clone(), context));
            }
            if let Some(limits) = self.agents.limits.get(&name) {
                let limit = |limit: Option<clackatoo_protocol::Limit>| {
                    limit.map(|limit| (limit.used, limit.resets))
                };
                live.limits.push(LiveLimits {
                    agent: name,
                    five_hour: limit(limits.five_hour),
                    weekly: limit(limits.weekly),
                });
            }
        }
        let now = Instant::now();
        let left = |due: Instant| millis(due.saturating_duration_since(now));
        let ours = |chat: &TerminalId| chats.contains(chat);
        live.opening = (self.agents.opening.iter())
            .filter(|(chat, _)| ours(chat))
            .map(|(chat, asked)| LiveOpening {
                chat: chat.0,
                waited: millis(now.saturating_duration_since(asked.at)),
                left: left(asked.due),
                fetching: asked.fetching,
            })
            .collect();
        let due = |(chat, due): (&TerminalId, &Instant)| {
            ours(chat).then(|| LiveDue {
                chat: chat.0,
                left: left(*due),
            })
        };
        live.parking = self.agents.parking.iter().filter_map(due).collect();
        live.woken = self.agents.woken.iter().filter_map(due).collect();
        live
    }

    /// Takes back the agents the last build handed over in `live`, their
    /// fds from `fds`, watching each as one just started. Returns the
    /// names of those taken back. One that can't be is let go, ending all
    /// it runs, as an agent stopped for an upgrade would be: its chats go
    /// on as theirs would.
    pub(in crate::server) fn resume_agents(
        &mut self,
        live: &mut Live,
        fds: &mut Fds,
    ) -> HashSet<String> {
        self.agents.next = self.agents.next.max(live.next_agent);
        let mut kept = HashSet::new();
        for handed in std::mem::take(&mut live.agents) {
            let id = AgentId(handed.id);
            let name = handed.name.clone();
            let command: Vec<OsString> = (handed.command.iter())
                .map(|word| OsString::from(std::ffi::OsStr::from_bytes(word)))
                .collect();
            let mut agent = match Agent::resume(handed, fds, &self.parser) {
                Ok(agent) => agent,
                Err((err, process)) => {
                    log::error!("agent {name} couldn't be taken over: {err}");
                    // Let go as any agent stopped is, with all it runs.
                    if let Some(process) = process {
                        self.let_agent_go(process, &name, End::Gently(KILL_AFTER));
                    }
                    continue;
                }
            };
            if self.agents.running.contains_key(&id) || self.agents.named(&name).is_some() {
                log::error!("agent {name} couldn't be taken over: there's one already");
                self.let_agent_go(agent.stop(), &name, End::Gently(KILL_AFTER));
                continue;
            }
            if let Some(dir) = self.config.agents.trace.clone() {
                agent.trace_to(&dir, &name);
            }
            let spec = self.agent_spec(&name);
            match self.watch_agent(id, spec, command, agent) {
                Ok(()) => {
                    // What it wrote meanwhile waits in its pipe.
                    self.queue_flood(Flood::Agent(id));
                    self.flush_agent(id);
                    kept.insert(name);
                }
                Err(failed) => {
                    let (err, agent, name) = *failed;
                    log::error!("agent {name} couldn't be watched: {err}");
                    self.let_agent_go(agent.stop(), &name, End::Gently(KILL_AFTER));
                }
            }
        }
        for (name, settings) in std::mem::take(&mut live.settings) {
            let settings = (settings.into_iter())
                .map(crate::chat::setting_from_handover)
                .collect();
            self.agents.settings.insert(name, settings);
        }
        for (name, context) in std::mem::take(&mut live.context) {
            self.agents.context.insert(name, context);
        }
        for limits in std::mem::take(&mut live.limits) {
            let limit = |kept: Option<(u8, Option<u64>)>| {
                kept.map(|(used, resets)| clackatoo_protocol::Limit {
                    used: used.min(100),
                    resets,
                })
            };
            let kept = Limits {
                five_hour: limit(limits.five_hour),
                weekly: limit(limits.weekly),
            };
            self.agents.limits.insert(limits.agent, kept);
        }
        kept
    }

    /// Takes back when each chat of an agent that went on through the
    /// upgrade, in `chats`, is next due something, as `live` had it.
    pub(in crate::server) fn resume_agent_timers(
        &mut self,
        live: &Live,
        chats: &HashSet<TerminalId>,
    ) {
        let now = Instant::now();
        let at = |left: u64| now + Duration::from_millis(left);
        for opening in &live.opening {
            let chat = TerminalId(opening.chat);
            if chats.contains(&chat) {
                let waited = Duration::from_millis(opening.waited);
                let asked = Asked {
                    at: now.checked_sub(waited).unwrap_or(now),
                    due: at(opening.left),
                    fetching: opening.fetching,
                };
                self.agents.opening.insert(chat, asked);
            }
        }
        for (each, timers) in [
            (&live.parking, &mut self.agents.parking),
            (&live.woken, &mut self.agents.woken),
        ] {
            for due in each
                .iter()
                .filter(|due| chats.contains(&TerminalId(due.chat)))
            {
                timers.insert(TerminalId(due.chat), at(due.left));
            }
        }
    }

    /// Whether the agent called `name`, running, can be steered as it
    /// answers, as it said when it started.
    pub(in crate::server) fn agent_steers(&self, name: &str) -> bool {
        (self.agents.named(name))
            .and_then(|id| self.agents.running.get(&id))
            .is_some_and(|slot| slot.agent.can_steer())
    }
}

/// The version `launch` pins its adapter at, if it's an adapter
/// clackatoo knows; `None` for the user's own command.
fn pinned(launch: &Launch) -> Option<&'static str> {
    match launch {
        Launch::Known(known) => Some(known.version),
        Launch::Command(_) => None,
    }
}

/// `left` as a whole number of milliseconds, as the handover keeps it.
fn millis(left: Duration) -> u64 {
    u64::try_from(left.as_millis()).unwrap_or(u64::MAX)
}
