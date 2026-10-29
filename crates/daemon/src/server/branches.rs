//! Rewinding a chat's conversation and switching between its branches,
//! as the server does it (CLACKA-198): the chat keeps the branches (see
//! `chat/branches.rs`); the server forgets what it kept of turns let go,
//! tells windows the turns of agents' hunks again, as their indexes may
//! have changed, closes the sessions branches leave, and keeps branches'
//! sessions off the start page.

use std::collections::HashSet;

use clackatoo_protocol::{Error, ForkFiles, TerminalId, TurnFilesState};

use super::Daemon;
use super::turn_undo::Reversal;
use crate::agent::Listed;

impl Daemon {
    /// Answers [`ChatAction::Rewind`](clackatoo_protocol::ChatAction::Rewind).
    pub(super) fn rewind_chat(
        &mut self,
        id: TerminalId,
        at: usize,
        files: ForkFiles,
        edit: bool,
        stop_tasks: bool,
    ) -> Result<(), Error> {
        let chat = self.chat_ref(id).ok_or(Error::NotAChat(id))?;
        if let Some(why) = chat.unsettled(stop_tasks) {
            return Err(Error::Other(why));
        }
        let shown = chat.node().turns.len();
        if at >= shown {
            return Err(Error::Other(format!("There's no turn {}.", at + 1)));
        }
        // The files first: a turn that can't be undone stops the rewind,
        // what was undone before it kept, and redoable.
        let undone = self
            .reverse_turns(id, at, files, false)
            .map_err(Error::Other)?;
        let chat = self.chat(id).ok_or(Error::NotAChat(id))?;
        let actions = chat.rewind(at, edit, stop_tasks).map_err(Error::Other)?;
        if let Some(undone) = undone {
            chat.notify(undone);
        }
        self.act(id, actions);
        self.after_cut(id);
        Ok(())
    }

    /// Answers [`ChatAction::SwitchBranch`](clackatoo_protocol::ChatAction::SwitchBranch).
    pub(super) fn switch_chat_branch(
        &mut self,
        id: TerminalId,
        at: usize,
        to: u16,
        files: ForkFiles,
        stop_tasks: bool,
    ) -> Result<(), Error> {
        let chat = self.chat_ref(id).ok_or(Error::NotAChat(id))?;
        if let Some(why) = chat.unsettled(stop_tasks) {
            return Err(Error::Other(why));
        }
        if chat.switch_target(at, to).map_err(Error::Other)?.is_none() {
            return Ok(());
        }
        // What the branch shown changed after the divergence is undone
        // first, newest first; what the branch switched to changed, that
        // was undone as it was left, is made again after, oldest first.
        let undone = self
            .reverse_turns(id, at, files, false)
            .map_err(Error::Other)?;
        let chat = self.chat(id).ok_or(Error::NotAChat(id))?;
        let actions = chat
            .switch_branch(at, to, stop_tasks)
            .map_err(Error::Other)?;
        self.act(id, actions);
        let redone = self.reverse_turns(id, at, files, true);
        let said = match (undone, redone) {
            (_, Err(why)) => Some(why),
            (Some(undone), Ok(Some(redone))) => Some(format!("{undone} {redone}")),
            (undone, Ok(redone)) => undone.or(redone),
        };
        if let (Some(said), Some(chat)) = (said, self.chat(id)) {
            chat.notify(said);
        }
        self.after_cut(id);
        Ok(())
    }

    /// Undoes, or with `redo` redoes, what chat `id`'s turns from `from`
    /// on changed in files, as `files` says: newest first to undo, oldest
    /// first to redo, and only those undone, to redo. Stops at the first
    /// that can't be, saying why. Returns what to say of what was done.
    fn reverse_turns(
        &mut self,
        id: TerminalId,
        from: usize,
        files: ForkFiles,
        redo: bool,
    ) -> Result<Option<String>, String> {
        let force = match files {
            ForkFiles::Keep => return Ok(None),
            ForkFiles::Undo => false,
            ForkFiles::UndoForce => true,
        };
        self.settle_all_pending();
        let shown = self.chat_ref(id).map_or(0, |chat| chat.node().turns.len());
        let mut order: Vec<usize> = (from..shown).collect();
        if !redo {
            order.reverse();
        }
        let mut done = 0;
        for index in order {
            let chat = self.chat_ref(id).ok_or("the chat closed")?;
            let undone = (chat.node().turns.get(index))
                .and_then(|turn| turn.files)
                .is_some_and(|files| files.state != TurnFilesState::Made);
            if redo && !undone {
                continue;
            }
            let uid = chat.turn_uid(index);
            let (said, how) = self.reverse_turn_said(id, index, redo, force);
            let dropped = self.agent_files.log.trim(id);
            self.refresh_files(id, dropped.into_iter().chain(uid));
            match how {
                Reversal::Done => done += 1,
                Reversal::Nothing => {}
                // Forced, a turn none of whose files can be is left as it is.
                Reversal::Refused if force => {}
                Reversal::Refused => {
                    let what = if redo { "switched" } else { "rewound" };
                    return Err(format!("{said} Nothing was {what} past it."));
                }
            }
        }
        let turns = if done == 1 { "turn" } else { "turns" };
        let verb = if redo { "Redid" } else { "Undid" };
        Ok((done > 0).then(|| format!("{verb} what {done} {turns} changed in files.")))
    }

    /// The turns chat `id` shows changed from where its branches part:
    /// what was kept of turns let go is forgotten, and windows hear the
    /// turns of its agent's hunks again, by their indexes now.
    fn after_cut(&mut self, id: TerminalId) {
        let forgotten = self.chat(id).map(crate::chat::HostedChat::take_forgotten);
        if let Some(forgotten) = forgotten.filter(|uids| !uids.is_empty()) {
            self.agent_files.log.forget_turns(id, &forgotten);
        }
        let buffers: Vec<_> = (self.buffers.iter())
            .filter(|(_, slot)| (slot.agents.all().iter()).any(|hunk| hunk.chat == id.0))
            .map(|(&buffer, _)| buffer)
            .collect();
        for buffer in buffers {
            self.tell_agents(buffer);
        }
        self.chat_reviews_stale(id);
        // Subagents of the branch shown now may have finished meanwhile,
        // held while it wasn't.
        let children: Vec<TerminalId> = (self.chat_ref(id).into_iter())
            .flat_map(|chat| chat.node().subagents.iter().map(|card| card.chat))
            .collect();
        self.recheck_finished(children);
        self.collect_damage(id);
        self.keep_soon();
    }

    /// `listed`, the sessions an agent kept, without those that are
    /// branches of chats' conversations, not shown, or forked from them:
    /// the start page lists a chat once, with its branches inside it.
    pub(super) fn without_branches(&self, mut listed: Vec<Listed>) -> Vec<Listed> {
        let mut hidden: HashSet<String> = (self.terminals.values())
            .filter_map(|slot| slot.terminal.chat_ref())
            .flat_map(crate::chat::HostedChat::branch_sessions)
            .collect();
        if hidden.is_empty() {
            return listed;
        }
        // Forks of those, as one an upgrade cut off making may be, and
        // theirs, a few deep.
        for _ in 0..4 {
            let more: Vec<String> = (listed.iter())
                .filter(|row| {
                    row.forked_from
                        .as_ref()
                        .is_some_and(|from| hidden.contains(from))
                })
                .map(|row| row.session.clone())
                .filter(|session| !hidden.contains(session))
                .collect();
            if more.is_empty() {
                break;
            }
            hidden.extend(more);
        }
        // A chat's own session is listed as ever, forked or not.
        for slot in self.terminals.values() {
            if let Some(session) = slot.terminal.chat_ref().and_then(|chat| chat.session()) {
                hidden.remove(session);
            }
        }
        listed.retain(|row| !hidden.contains(&row.session));
        listed
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{BufferUpdate, ChatAction, ForkFiles, TURN_NOT_SHOWN};

    use super::*;
    use crate::server::turn_undo::tests::World;

    fn rewind(at: u32) -> ChatAction {
        ChatAction::Rewind {
            at,
            files: ForkFiles::Keep,
            edit: false,
            stop_tasks: false,
        }
    }

    fn hunk_turns(world: &mut World, id: clackatoo_protocol::BufferId) -> Vec<u32> {
        let Some(BufferUpdate::Agents { hunks, .. }) = world.daemon.agents_update(id) else {
            panic!("no agent hunks");
        };
        hunks.iter().map(|hunk| hunk.turn).collect()
    }

    #[test]
    fn a_rewind_tells_windows_its_turns_hunks_are_on_another_branch() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "a\n").unwrap();
        let id = world.open("a.txt");
        world.end_turn();
        world.next_turn();
        world.write("a.txt", "A\n");
        world.end_turn();
        assert_eq!(hunk_turns(&mut world, id), [1]);
        let chat = world.chat;
        world.daemon.chat_act(chat, rewind(1)).unwrap();
        assert_eq!(hunk_turns(&mut world, id), [TURN_NOT_SHOWN]);
        let back = ChatAction::SwitchBranch {
            at: 1,
            to: 1,
            files: ForkFiles::Keep,
            stop_tasks: false,
        };
        world.daemon.chat_act(chat, back).unwrap();
        assert_eq!(hunk_turns(&mut world, id), [1]);
        // Its undo is by index again, the uid it was made in.
        assert_eq!(world.undo(1, false, false), "Undid turn 2: a.txt.");
    }

    #[test]
    fn the_start_page_lists_no_branch_session_of_a_chat_nor_forks_of_them() {
        let mut world = World::new();
        world.end_turn();
        let chat = world.chat;
        world.daemon.chat_act(chat, rewind(0)).unwrap();
        let hosted = world.daemon.chat(chat).unwrap();
        hosted.act(ChatAction::Send("again".into()));
        // The chat's session now is another; s1 is the branch left's.
        let hosted = world.daemon.chat(chat).unwrap();
        hosted.session_closed();
        hosted.connected(crate::agent::Opened {
            session: "s2".into(),
            settings: None,
        });
        let row = |session: &str, from: Option<&str>| Listed {
            session: session.to_owned(),
            cwd: "/".to_owned(),
            title: String::new(),
            updated: None,
            forked_from: from.map(str::to_owned),
        };
        let listed = vec![
            row("s1", None),
            row("orphan", Some("s1")),
            row("s2", Some("s1")),
            row("other", None),
        ];
        let shown: Vec<String> = (world.daemon.without_branches(listed).into_iter())
            .map(|row| row.session)
            .collect();
        assert_eq!(shown, ["s2", "other"]);
    }

    fn rewind_undoing(at: u32, files: ForkFiles) -> ChatAction {
        ChatAction::Rewind {
            at,
            files,
            edit: false,
            stop_tasks: false,
        }
    }

    fn switch(at: u32, to: u16, files: ForkFiles) -> ChatAction {
        ChatAction::SwitchBranch {
            at,
            to,
            files,
            stop_tasks: false,
        }
    }

    /// A world whose chat changed a.txt in its first turn and b.txt and
    /// c.txt in its next two, each from a line to its capital.
    fn three_turns() -> World {
        let mut world = World::new();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(world.path(name), "x\n").unwrap();
        }
        world.write("a.txt", "A\n");
        world.end_turn();
        for name in ["b.txt", "c.txt"] {
            world.next_turn();
            world.write(name, "X\n");
            world.end_turn();
        }
        world
    }

    #[test]
    fn a_rewind_undoes_the_files_of_the_turns_it_cuts_and_a_switch_back_redoes_them() {
        let mut world = three_turns();
        let chat = world.chat;
        world
            .daemon
            .chat_act(chat, rewind_undoing(1, ForkFiles::Undo))
            .unwrap();
        assert_eq!(world.disk("a.txt").as_deref(), Some("A\n"));
        assert_eq!(world.disk("b.txt").as_deref(), Some("x\n"));
        assert_eq!(world.disk("c.txt").as_deref(), Some("x\n"));
        let notice = world.chat_node().notice.unwrap_or_default();
        assert_eq!(notice, "Undid what 2 turns changed in files.");
        world
            .daemon
            .chat_act(chat, switch(1, 1, ForkFiles::Undo))
            .unwrap();
        assert_eq!(world.chat_node().turns.len(), 3);
        assert_eq!(world.disk("b.txt").as_deref(), Some("X\n"));
        assert_eq!(world.disk("c.txt").as_deref(), Some("X\n"));
    }

    #[test]
    fn a_turn_that_cant_be_undone_stops_the_rewind_unless_forced() {
        let mut world = three_turns();
        let chat = world.chat;
        // Yours since, on the line turn 2 changed.
        std::fs::write(world.path("b.txt"), "yours\n").unwrap();
        let refused = world
            .daemon
            .chat_act(chat, rewind_undoing(1, ForkFiles::Undo));
        let refused = refused.unwrap_err().to_string();
        assert!(refused.contains("Nothing was rewound past it"), "{refused}");
        // The newest was undone, and stays so; nothing was cut.
        assert_eq!(world.disk("c.txt").as_deref(), Some("x\n"));
        assert_eq!(world.chat_node().turns.len(), 3);
        world
            .daemon
            .chat_act(chat, rewind_undoing(1, ForkFiles::UndoForce))
            .unwrap();
        assert_eq!(world.chat_node().turns.len(), 1);
        assert_eq!(world.disk("b.txt").as_deref(), Some("yours\n"));
    }
}
