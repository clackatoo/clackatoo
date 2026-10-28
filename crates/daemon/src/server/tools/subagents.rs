//! Subagents: chats agents start for work of their own, as Claude Code's
//! Task tool starts its helpers, but one way for every agent, and as
//! chats the user can open, read, steer and undo.
//!
//! A chat an agent starts goes in the background unless it asks to show
//! it: it's in no pane, and the chat that started it shows it as a card
//! in the turn that started it, where the user opens it as a pane. It's
//! a chat like any other all the same: its turns can be undone and its
//! changes reviewed, its questions wait for the user (the card says so),
//! and other agents message it. Its tools reach what the chat that
//! started it reaches, and open panes beside that chat, as the nearest
//! chat with a pane above it is its anchor.
//!
//! A chat started as a profile (see [`crate::profiles`]) takes its role,
//! limits, agent and model from it, and never has fewer limits than the
//! chat that started it.
//!
//! Chats in the background close with the chat that started them. To
//! keep the number of chats under one of the user's within bounds, the
//! oldest that have finished, in the background, close to make room for
//! new ones: the agent that started them has had their answers.
//!
//! `start_chats` starts several at once, asking the user once for all of
//! them, and `wait_for_chats` waits for all of them, or the first, and
//! gives back one summary.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clackatoo_protocol::{PaneId, PaneTerminal, SettingKind, Subagent, SubagentStatus, TerminalId};
use mio::Token;
use serde_json::Value;

use super::talk::{MAX_CHILDREN, final_words, head};
use super::{MAX_WAITS, Wait, Waiting};
use crate::chat::{HostedChat, Wanted, role_for};
use crate::mcp::NewChat;
use crate::profiles;
use crate::server::Daemon;

/// The most of each chat's answer a summary of several gives back, in
/// bytes, all together: each gets its share.
const MAX_SUMMARY: usize = 24 * 1024;

/// How far up from a chat its anchor is looked for: further than chats go
/// deep, so a loop in a confused handover can't hang the daemon.
const MAX_ANCHOR: usize = 8;

impl Daemon {
    /// The pane the chat `chat` is in, or else the nearest chat's above it
    /// that is: what its agent's tools reach from, and open panes beside.
    pub(in crate::server) fn anchor_pane(&self, chat: TerminalId) -> Option<PaneId> {
        let workspaces = self.workspaces.get();
        let mut at = chat;
        for _ in 0..MAX_ANCHOR {
            if let Some(pane) = workspaces.pane_showing(at) {
                return Some(pane);
            }
            at = self.chat_ref(at)?.relations().parent?;
        }
        None
    }

    /// The chat `chat`'s card in the chat that started it says how it's
    /// going now, if it was out of date. Cheap when it isn't, as it's
    /// asked on every change to a chat an agent started.
    pub(in crate::server) fn note_subagent(&mut self, chat: TerminalId) {
        let Some(child) = self.chat_ref(chat) else {
            return;
        };
        let Some(parent) = child.relations().parent else {
            return;
        };
        let shown = self.workspaces.get().pane_showing(chat).is_some();
        let Some(of) = self.chat_ref(parent) else {
            return;
        };
        let entry = match of.subagent(chat) {
            Some(entry) if !child.outdates(entry, shown) => return,
            Some(entry) => child.as_subagent(entry.turn, shown),
            None => {
                let started = (child.node().turns.first()).map_or(u64::MAX, |turn| turn.sent);
                child.as_subagent(of.turn_at(started), shown)
            }
        };
        // Finished in the background, it lets its session go in a while.
        let idle = !shown && !entry.status.going() && child.is_connected();
        self.park_later(chat, idle);
        if let Some(of) = self.chat(parent) {
            of.set_subagent(entry);
            self.collect_damage(parent);
        }
    }

    /// Brings every card up to date, as when panes open or close, or the
    /// daemon has just taken over from an upgrade.
    pub(in crate::server) fn note_subagents(&mut self) {
        let children: Vec<TerminalId> = (self.terminals.iter())
            .filter(|(_, slot)| {
                (slot.terminal.chat_ref()).is_some_and(|chat| chat.relations().parent.is_some())
            })
            .map(|(&id, _)| id)
            .collect();
        for child in children {
            self.note_subagent(child);
        }
    }

    /// The chat `chat`, which `parent`'s agent started, has closed, with
    /// `cards` for the chats it started: its card says so, and the chats
    /// it started that are in the background close with it.
    ///
    /// Those in a pane, and those still going or with changes waiting for
    /// review, themselves or in a chat under them, go to the chat above
    /// instead, under the turn `chat` was started in, so a review of that
    /// turn still lists their changes; as do those closed already whose
    /// changes wait for review, by their cards.
    pub(in crate::server) fn subagent_closed(
        &mut self,
        chat: TerminalId,
        parent: Option<TerminalId>,
        cards: &[Subagent],
    ) {
        let mut started_in = None;
        if let Some(parent) = parent
            && let Some(of) = self.chat(parent)
            && let Some(mut entry) = of.subagent(chat).cloned()
        {
            started_in = Some(entry.turn);
            entry.status = SubagentStatus::Closed;
            entry.shown = false;
            entry.ended = entry.ended.or_else(|| Some(crate::chat::now()));
            of.set_subagent(entry);
            self.collect_damage(parent);
        }
        let workspaces = self.workspaces.get();
        let children: Vec<(TerminalId, bool)> = (self.terminals.iter())
            .filter(|(_, slot)| {
                (slot.terminal.chat_ref())
                    .is_some_and(|child| child.relations().parent == Some(chat))
            })
            .map(|(&id, _)| (id, workspaces.pane_showing(id).is_some()))
            .collect();
        // One in a pane, or still going, or with changes waiting for
        // review, goes to the chat above, if there is one, rather than
        // lose its work: the user's own chat closing ends everything under
        // it but what's in panes.
        let above = parent.filter(|&parent| self.chat_ref(parent).is_some());
        if let (Some(above), Some(turn)) = (above, started_in) {
            let waiting: Vec<Subagent> = (cards.iter())
                .filter(|card| card.status == SubagentStatus::Closed)
                .filter(|card| self.awaits_review(card.chat))
                .map(|card| Subagent {
                    turn,
                    ..card.clone()
                })
                .collect();
            if let Some(of) = self.chat(above)
                && !waiting.is_empty()
            {
                for card in waiting {
                    of.set_subagent(card);
                }
                self.collect_damage(above);
            }
        }
        for (child, shown) in children {
            if let Some(above) = above
                && (shown || self.keeps_work(child, 0))
                && let Some(kid) = self.chat(child)
            {
                log::info!(
                    "chat {child} goes to chat {above}, as chat {chat}, which started it, closed"
                );
                kid.adopted_by(above);
                if let Some(turn) = started_in
                    && let Some(card) =
                        (self.chat_ref(child)).map(|kid| kid.as_subagent(turn, shown))
                    && let Some(of) = self.chat(above)
                {
                    of.set_subagent(card);
                }
                self.note_subagent(child);
                continue;
            }
            if shown {
                continue;
            }
            log::info!("chat {child} closes with chat {chat}, which started it");
            if let Err(err) = self.close_terminal(child) {
                log::warn!("chat {child} couldn't be closed: {err}");
            }
        }
        // Cards have moved, and a chat has closed.
        self.note_chats_above();
    }

    /// Whether chat `chat`, or a chat under it, open or closed, is still
    /// going or has changes waiting for review: closing it would lose
    /// them, or the cards that find them. `depth` is how far down from the
    /// first it's looked; past [`MAX_ANCHOR`] it says yes, to keep what
    /// it can't see.
    fn keeps_work(&self, chat: TerminalId, depth: usize) -> bool {
        let Some(open) = self.chat_ref(chat) else {
            return self.awaits_review(chat);
        };
        if !super::talk::idle(open) || self.awaits_review(chat) {
            return true;
        }
        if depth >= MAX_ANCHOR {
            return true;
        }
        let carded = open.node().subagents.iter().map(|card| card.chat);
        let started = (self.terminals.iter())
            .filter(|(_, slot)| {
                (slot.terminal.chat_ref()).is_some_and(|kid| kid.relations().parent == Some(chat))
            })
            .map(|(&id, _)| id);
        let mut under: Vec<TerminalId> = carded.chain(started).collect();
        under.sort_unstable();
        under.dedup();
        (under.into_iter()).any(|kid| self.keeps_work(kid, depth + 1))
    }

    /// Shows the chat `child`, which the chat `chat`'s agent started, in a
    /// pane beside `chat`, as the user asked from its card; or if it's in
    /// one, points it out.
    pub(in crate::server) fn open_subagent(&mut self, chat: TerminalId, child: TerminalId) {
        let theirs = (self.chat_ref(child)).is_some_and(|kid| kid.relations().parent == Some(chat));
        if !theirs {
            return;
        }
        if let Some(pane) = self.workspaces.get().pane_showing(child) {
            self.show(pane, None);
            return;
        }
        match self.place_beside(chat, PaneTerminal::Existing(child)) {
            Ok(pane) => self.show(pane, None),
            Err(why) => log::warn!("chat {child} couldn't be shown: {why}"),
        }
    }

    /// Takes `pane`, which shows the chat `chat`, out of its tab without
    /// closing the chat, if an agent in a chat that's still open started
    /// it: it goes on in the background, as a card in that chat again,
    /// as one started without a pane is, and opens from the card again.
    /// Returns whether it did; if not, closing the pane is closing the
    /// chat, as for any other.
    pub(in crate::server) fn send_subagent_back(&mut self, pane: PaneId, chat: TerminalId) -> bool {
        let parent = (self.chat_ref(chat)).and_then(|kid| kid.relations().parent);
        let Some(parent) = parent.filter(|&parent| self.chat_ref(parent).is_some()) else {
            return false;
        };
        if !self.workspaces.remove_pane(pane) {
            return false;
        }
        log::info!("chat {chat} goes to the background, as a card in chat {parent}");
        // A window older than protocol 108 takes the pane's going for the
        // chat's closing, and never stops following it: it stops here.
        let older: Vec<Token> = (self.terminals.get(&chat).into_iter())
            .flat_map(|slot| slot.viewers.iter().copied())
            .filter(|token| match self.sources.get(token.0) {
                Some(crate::server::Source::Client(client)) => {
                    client.wire < clackatoo_protocol::wire::SUBAGENT_PROFILE
                }
                _ => false,
            })
            .collect();
        for token in older {
            self.detach(token, chat);
        }
        // Its card says so.
        self.workspaces_changed();
        true
    }

    /// The chats under the user's chat `root` that can close to make room
    /// for new ones, oldest first: in the background, finished, asking
    /// nothing, with no chats of their own, their answers had by the chat
    /// that started them, and none of their changes waiting for review.
    /// Closing one loses nothing anyone is waiting on.
    pub(super) fn closable(&self, root: TerminalId) -> Vec<TerminalId> {
        let workspaces = self.workspaces.get();
        let parents: std::collections::HashSet<TerminalId> = (self.terminals.values())
            .filter_map(|slot| slot.terminal.chat_ref()?.relations().parent)
            .collect();
        let mut found: Vec<(u64, TerminalId)> = (self.terminals.iter())
            .filter_map(|(&id, slot)| {
                let chat = slot.terminal.chat_ref()?;
                let ours = chat.relations().root == Some(root) && chat.relations().parent.is_some();
                let done = !chat.node().turns.is_empty()
                    && super::talk::idle(chat)
                    && matches!(
                        chat.subagent_status(),
                        SubagentStatus::Done | SubagentStatus::Failed(_)
                    );
                let spent = done
                    && chat.parent_has_heard()
                    && !parents.contains(&id)
                    && !self.awaits_review(id);
                let started = chat.node().turns.first().map_or(0, |turn| turn.sent);
                (ours && spent && workspaces.pane_showing(id).is_none()).then_some((started, id))
            })
            .collect();
        found.sort_unstable();
        found.into_iter().map(|(_, id)| id).collect()
    }

    /// `start_chat` and `start_chats`: starts each of `chats` for the chat
    /// `chat`'s agent, as its profile says, and sends it its first
    /// message. Each is checked before any starts, so none starts if one
    /// is refused; one that fails as it starts leaves the others going.
    pub(super) fn start_chats(
        &mut self,
        chat: TerminalId,
        chats: Vec<NewChat>,
    ) -> Result<String, String> {
        let tool = crate::mcp::Tool::StartChats(chats.clone());
        if let Some(why) = self.talk_refused(chat, &tool) {
            return Err(why);
        }
        let home = self.chat_dir(chat)?;
        let mut planned = Vec::new();
        for new in chats {
            let dir = match &new.cwd {
                Some(cwd) => {
                    crate::server::buffers::resolve(cwd, Path::new(&home), &self.config.home)
                }
                None => PathBuf::from(&home),
            };
            if !dir.is_dir() {
                return Err(format!("there's no directory {}", dir.display()));
            }
            let profile = (self.config.agents)
                .profile(new.profile.as_deref().unwrap_or(profiles::DEFAULT))?;
            planned.push((new, profile, dir));
        }
        let root = self
            .chat_ref(chat)
            .ok_or("the chat has closed")?
            .relations()
            .root;
        let root = root.unwrap_or(chat);
        let live = (self.terminals.values())
            .filter_map(|slot| slot.terminal.chat_ref())
            .filter(|other| other.relations().root == Some(root))
            .count();
        let mut text = String::new();
        let over = (live + planned.len()).saturating_sub(MAX_CHILDREN);
        for old in self.closable(root).into_iter().take(over) {
            let _ = writeln!(
                text,
                "Closed chat {old}, which had finished, in the background, to make room."
            );
            if let Err(err) = self.close_terminal(old) {
                log::warn!("chat {old} couldn't be closed: {err}");
            }
        }
        let mut started = 0;
        for (number, (new, profile, dir)) in planned.into_iter().enumerate() {
            // Checked first, one fails only as the chat it starts closes
            // at once: those started go on, and the agent hears of both.
            match self.start_one(chat, new, &profile, &dir) {
                Ok(said) => {
                    started += 1;
                    text.push_str(&said);
                }
                Err(why) if started == 0 && number == 0 => return Err(format!("{text}{why}")),
                Err(why) => {
                    let _ = write!(text, "Chat {} of these couldn't start: {why}", number + 1);
                }
            }
            text.push('\n');
        }
        text.push_str(match started {
            0 => "None started.",
            1 => "wait_for_chat waits for its answer.",
            _ => {
                "wait_for_chats waits for their answers, all of them or the first, in one summary."
            }
        });
        Ok(text)
    }

    /// Starts `new` for the chat `chat`'s agent, as `profile`, in `dir`.
    /// Returns what the agent is told of it.
    fn start_one(
        &mut self,
        chat: TerminalId,
        new: NewChat,
        profile: &profiles::Profile,
        dir: &Path,
    ) -> Result<String, String> {
        let message = (self.message_from(chat, new.prompt)).ok_or("the chat has closed")?;
        let own = self.chat_ref(chat).ok_or("the chat has closed")?;
        let name =
            (new.agent.or_else(|| profile.agent.clone())).unwrap_or_else(|| own.agent().to_owned());
        let same = name == own.agent();
        // It may do what the chat that starts it may, and no more: with
        // the same agent, it starts in the same mode; with another, in
        // that agent's safest (see `HostedChat::connected`), and what the
        // parent's mode rules out, such as Codex's Read-only or Claude
        // Code's planning, holds as limits of its own.
        let mode = same.then(|| own.mode().map(str::to_owned)).flatten();
        let limits = (profile.limits).max(own.limits()).max(own.mode_limits());
        let inherited = |kind| {
            (same && profile.inherit_model)
                .then(|| own.setting_value(kind).map(str::to_owned))
                .flatten()
        };
        let wanted = Wanted {
            model: (new
                .model
                .or_else(|| profile.model_for(&name).map(str::to_owned)))
            .or_else(|| inherited(SettingKind::Model)),
            effort: (profile.effort_for(&name).map(str::to_owned))
                .or_else(|| inherited(SettingKind::Effort)),
            read_only: limits.no_commands(),
        };
        let role = role_for(&profile.name, &profile.prompt, limits);
        let (root, depth) = (own.relations().root.unwrap_or(chat), own.relations().depth);
        let (size, tools, task, browser) = (own.size(), own.tools(), own.task(), own.browser());
        let spec = self.agent_spec(&name);
        let id = self.next_terminal();
        let cwd = dir.to_string_lossy().into_owned();
        let mut child = HostedChat::new(id, size, cwd, &spec, tools, task, browser);
        child.started_by(chat, root, depth + 1, mode);
        child.start_as(&profile.name, limits, role, wanted);
        self.start_hosted_chat(child);
        let pane = if new.show {
            match self.place_beside(chat, PaneTerminal::Existing(id)) {
                Ok(pane) => Some(pane),
                Err(why) => {
                    let _ = self.close_terminal(id);
                    return Err(why);
                }
            }
        } else {
            None
        };
        let actions = (self.chat(id).ok_or("the chat closed at once")?)
            .deliver(message)
            .map_err(|_| "the new chat refused its first message".to_owned())?;
        self.spend(chat);
        self.act(id, actions);
        self.collect_damage(id);
        self.note_subagent(id);
        log::info!(
            "chat {chat} started chat {id}, with {name}, as {}",
            profile.name
        );
        let place = pane.map_or_else(
            || "in the background, as a card in this chat the user can open".to_owned(),
            |pane| format!("in pane {pane}"),
        );
        Ok(format!(
            "Started chat {id} ({name}, as {}{}), {place}, in {}, and sent it the message.",
            profile.name,
            if limits.no_commands() {
                format!(", {}", limits.name())
            } else {
                String::new()
            },
            dir.display()
        ))
    }

    /// `wait_for_chats`: answers at once if the chats `to` have finished,
    /// all of them or with `first` one, or else waits, until `timeout`,
    /// for the call `id` on the connection at `token`.
    pub(super) fn wait_for_chats(
        &mut self,
        (token, id): (Token, Value),
        chat: TerminalId,
        to: Vec<TerminalId>,
        (first, timeout): (bool, Duration),
    ) -> Option<Result<String, String>> {
        for &other in &to {
            if let Err(why) = self.waitable(chat, other) {
                return Some(Err(why));
            }
        }
        if self.chats_finished(&to, first) || timeout.is_zero() {
            return Some(Ok(self.summary(chat, &to)));
        }
        if let Some(&waiting) = to.iter().find(|&&other| self.waits_on(other, chat)) {
            return Some(Err(format!(
                "chat {waiting} is waiting for this chat: answer it instead, by ending this turn, \
                 or the two of you wait for each other until the waits time out"
            )));
        }
        let waits = (self.tools.waiting.iter())
            .filter(|waiting| {
                waiting.chat == chat
                    && matches!(waiting.what, Wait::Turn { .. } | Wait::Chats { .. })
            })
            .count();
        if waits >= MAX_WAITS {
            return Some(Err(format!(
                "{MAX_WAITS} waits are going on already: let one finish"
            )));
        }
        self.tools.waiting.push(Waiting {
            token,
            id,
            chat,
            what: Wait::Chats {
                chats: to,
                first,
                due: Instant::now() + timeout,
            },
        });
        None
    }

    /// Whether the chats `chats` have finished: all of them, or with
    /// `first`, any. A chat that's closed has.
    pub(super) fn chats_finished(&self, chats: &[TerminalId], first: bool) -> bool {
        let done = |chat: &TerminalId| self.chat_ref(*chat).is_none_or(super::talk::idle);
        if first {
            chats.iter().any(done)
        } else {
            chats.iter().all(done)
        }
    }

    /// One summary of the chats `chats`, for the chat `chat`'s agent: how
    /// each is going, its final answer, and the files it changed.
    pub(super) fn summary(&self, chat: TerminalId, chats: &[TerminalId]) -> String {
        let finished = (chats.iter())
            .filter(|&&other| self.chat_ref(other).is_none_or(super::talk::idle))
            .count();
        let mut text = match chats.len() {
            1 => String::new(),
            count => format!(
                "{count} chats: {finished} finished, {} not yet.\n\n",
                count - finished
            ),
        };
        let share = MAX_SUMMARY / chats.len().max(1);
        for &other in chats {
            text.push_str(&self.summary_of(chat, other, share));
            text.push('\n');
        }
        if finished < chats.len() {
            text.push_str(
                "wait_for_chats again waits on for those not yet finished; read_chat reads how \
                 far one has got.",
            );
        }
        text.trim_end().to_owned()
    }

    /// The chat `to`, as a summary tells the chat `chat`'s agent of it:
    /// what it is, how it's going, its final answer, at most `share`
    /// bytes of it, and the files it changed.
    fn summary_of(&self, chat: TerminalId, to: TerminalId, share: usize) -> String {
        let Some(other) = self.chat_ref(to) else {
            let closed = self.chat_ref(chat).and_then(|own| own.subagent(to));
            return match closed {
                Some(entry) => format!("Chat {to} ({:?}) has closed.\n", entry.task),
                None => format!("Chat {to} has closed.\n"),
            };
        };
        let mut what = vec![other.agent_title().to_owned()];
        if let Some(profile) = other.profile() {
            what.insert(0, profile.to_owned());
        }
        if let Some(model) = other.model_name() {
            what.push(model.to_owned());
        }
        let turns = &other.node().turns;
        let started = turns.first().map_or(0, |turn| turn.sent);
        let took = (turns.last().and_then(|turn| turn.end.as_ref()))
            .filter(|_| super::talk::idle(other))
            .map_or_else(crate::chat::now, |end| end.at);
        let mut text = format!(
            "Chat {to} ({}): {}, after {}.\n",
            what.join(" · "),
            progress(&other.subagent_status()),
            duration(took.saturating_sub(started))
        );
        if !super::talk::idle(other) {
            return text;
        }
        let answer = (turns.iter()).rfind(|turn| turn.switch.is_none() && turn.end.is_some());
        if other.relations().parent == Some(chat) {
            other.told_parent(turns.len());
        }
        match answer.map(final_words) {
            Some(words) if !words.is_empty() => {
                text.push_str(&head(&words, share));
                text.push('\n');
            }
            _ => text.push_str("It said nothing.\n"),
        }
        let changed = self.agent_files.log.changed_in(to, 0..turns.len());
        if changed.is_empty() {
            text.push_str("Files changed: none.\n");
        } else {
            let dir = other.working_directory();
            let files: Vec<String> = (changed.iter())
                .map(|file| {
                    let path = super::shorten(Path::new(&file.path), dir);
                    match (file.undone, file.existed, file.exists) {
                        (true, ..) => format!("{path} (undone)"),
                        (_, false, true) => format!("{path} (new)"),
                        (_, true, false) => format!("{path} (deleted)"),
                        _ => path,
                    }
                })
                .collect();
            let _ = writeln!(text, "Files changed: {}.", files.join(", "));
        }
        text
    }

    /// Checks the chat `chat`'s agent may wait for `to`: one in its sight,
    /// or one it sent a message.
    fn waitable(&self, chat: TerminalId, to: TerminalId) -> Result<(), String> {
        if to == chat {
            return Err(format!("chat {to} is this chat"));
        }
        let Some(target) = self.chat_ref(to) else {
            // Closed, it has finished: the summary says so, if it was this
            // chat's.
            return (self.chat_ref(chat))
                .and_then(|own| own.subagent(to))
                .map(|_| ())
                .ok_or_else(|| format!("there's no chat {to}: list_chats lists them"));
        };
        let sent = (target.node().turns.iter())
            .any(|turn| turn.from.as_ref().is_some_and(|from| from.chat == chat));
        if self.may_see(chat, to) || sent {
            Ok(())
        } else {
            Err(format!(
                "chat {to} is in another workspace: wait only for its answers to your messages"
            ))
        }
    }
}

/// How a chat that's `status` is going, as a summary says.
fn progress(status: &SubagentStatus) -> String {
    match status {
        SubagentStatus::Starting => "starting".to_owned(),
        SubagentStatus::Working => "still working".to_owned(),
        SubagentStatus::Asking => "waiting for the user to answer a question".to_owned(),
        SubagentStatus::Done => "finished".to_owned(),
        SubagentStatus::Failed(why) => format!("failed: {why}"),
        SubagentStatus::Closed => "closed".to_owned(),
    }
}

/// `ms` milliseconds, short: "<1s", "42s", "4m 58s", "1h 3m".
fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0 => "<1s".to_owned(),
        1..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::duration;

    #[test]
    fn durations_are_short() {
        assert_eq!(duration(400), "<1s");
        assert_eq!(duration(42_000), "42s");
        assert_eq!(duration(298_000), "4m 58s");
    }
}
