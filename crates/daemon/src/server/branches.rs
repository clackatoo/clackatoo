//! Rewinding a chat's conversation and switching between its branches,
//! as the server does it (CLACKA-198): the chat keeps the branches (see
//! `chat/branches.rs`); the server forgets what it kept of turns let go,
//! tells windows the turns of agents' hunks again, as their indexes may
//! have changed, closes the sessions branches leave, and keeps branches'
//! sessions off the start page.

use std::collections::HashSet;

use clackatoo_protocol::{Error, ForkFiles, TerminalId};

use super::Daemon;
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
        refuse_files(files)?;
        let chat = self.chat(id).ok_or(Error::NotAChat(id))?;
        let actions = chat.rewind(at, edit, stop_tasks).map_err(Error::Other)?;
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
        refuse_files(files)?;
        let chat = self.chat(id).ok_or(Error::NotAChat(id))?;
        let actions = chat
            .switch_branch(at, to, stop_tasks)
            .map_err(Error::Other)?;
        self.act(id, actions);
        self.after_cut(id);
        Ok(())
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

/// Undoing the files a rewind or a switch takes out of the conversation
/// isn't done yet: refused, rather than leaving them as they are unasked.
fn refuse_files(files: ForkFiles) -> Result<(), Error> {
    match files {
        ForkFiles::Keep => Ok(()),
        ForkFiles::Undo | ForkFiles::UndoForce => Err(Error::Other(
            "Undoing the files of a rewind isn't done yet: rewind keeping them, and undo turns \
             with u."
                .to_owned(),
        )),
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
        // Undoing the files isn't done yet, so it's refused, not ignored.
        let undo = ChatAction::Rewind {
            at: 0,
            files: ForkFiles::Undo,
            edit: false,
            stop_tasks: false,
        };
        assert!(world.daemon.chat_act(chat, undo).is_err());
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
}
