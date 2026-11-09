//! Managed operations reuse background PTYs, including their upgrade survival.
//! A caller-chosen identity is reserved before typing. A lost response is never
//! grounds for executing it twice. Only observed shell exits imply completion.

use clackatoo_protocol::{Activity, ActivityState, AgentTools, Sender, TerminalId};
use clackatoo_term::Progress;

use super::{Commanded, Daemon, Source};
use crate::chat::{Message, now};

/// The most activities running at once, per chat and in all.
const MAX_RUNNING: usize = 8;
const MOST_RUNNING: usize = 64;

/// The most finished activities kept, newest first, per chat and in all:
/// those older are let go, with their ids, as each new one starts.
const KEPT_PER_CHAT: usize = 16;
const KEPT: usize = 64;

use crate::saved::Operation as Record;

impl Daemon {
    pub(super) fn start_activity(
        &mut self,
        chat: TerminalId,
        id: &str,
        title: String,
        command: String,
        cwd: Option<String>,
    ) -> Result<String, String> {
        let dir = self.chat_dir(chat)?;
        let cwd = cwd.map_or_else(
            || std::path::PathBuf::from(&dir),
            |cwd| {
                super::super::buffers::resolve(&cwd, std::path::Path::new(&dir), &self.config.home)
            },
        );
        let cwd = cwd.to_string_lossy().into_owned();
        if let Some(record) = self
            .tools
            .activities
            .iter()
            .find(|r| r.activity.owner == chat && r.activity.id == id)
        {
            if record.activity.command != command || record.activity.cwd != cwd {
                return Err("This operation id already belongs to a different request. Inspect it; do not reuse it.".into());
            }
            return self.read_activities(chat, Some(id));
        }
        self.forget_old_activities();
        if let Some(why) = (self.too_many_running(chat)).or_else(|| self.too_many_kept(chat)) {
            return Err(why);
        }
        // Unmarked shells cannot provide a reliable lifecycle.
        if !crate::shell::marks(&self.config.shell, self.config.shell_integration.as_deref()) {
            return Err(
                "Managed activities require shell integration with command exit marks.".into(),
            );
        }
        if self
            .tools
            .activities
            .iter()
            .map(|r| r.activity.command.len() + r.activity.cwd.len() + r.activity.title.len())
            .sum::<usize>()
            + command.len()
            + cwd.len()
            + title.len()
            > 512 * 1024
        {
            return Err("The retained operation journal is full; no command was started.".into());
        }
        if command.len() > 8192 {
            return Err(
                "Managed commands are limited to 8 KiB; put longer commands in a script.".into(),
            );
        }
        let turn = self.chat_ref(chat).ok_or("The chat closed")?.working_on().0;
        let (pane, terminal) = self.background_terminal(chat, &command, Some(&cwd))?;
        let activity = Activity {
            id: id.into(),
            owner: chat,
            // Its uid, which stays with the turn; the chat shows it on
            // the turn's index (see `publish_activities`).
            turn: turn.0,
            title,
            command,
            cwd,
            pane: Some(pane),
            started: now(),
            ended: None,
            state: ActivityState::Starting,
            result: String::new(),
        };
        self.tools.activities.push(Record {
            activity,
            terminal,
            expect: None,
            renumbered: 0,
            notified: false,
            waiting_for: None,
        });
        self.publish_activities(chat);
        match self.persist_activity_intent() {
            Ok(serial) => {
                self.tools
                    .activities
                    .last_mut()
                    .expect("just reserved")
                    .waiting_for = Some(serial);
            }
            Err(why) => {
                let record = self.tools.activities.last_mut().expect("just reserved");
                record.activity.state = ActivityState::Refused;
                record.activity.result.clone_from(&why);
                record.activity.ended = Some(now());
                self.publish_activities(chat);
                return Err(why);
            }
        }
        self.read_activities(chat, Some(id))
    }

    /// Why the chat `chat` may start no more activities now, if it may
    /// not: as many run already, of its own or in all. Those finished
    /// don't count: only so many are kept (see
    /// [`Self::forget_old_activities`]).
    fn too_many_running(&self, chat: TerminalId) -> Option<String> {
        let running = |r: &&Record| r.activity.state.active();
        let all = self.tools.activities.iter().filter(running).count();
        let own = (self.tools.activities.iter())
            .filter(|r| r.activity.owner == chat)
            .filter(running)
            .count();
        (own >= MAX_RUNNING || all >= MOST_RUNNING).then(|| {
            format!(
                "Activity limit reached ({MAX_RUNNING} running per chat, {MOST_RUNNING} in all). \
                 Wait for or cancel existing work before starting more."
            )
        })
    }

    /// Why the chat `chat` may start no more activities now, if it may
    /// not: more finished ones are kept than may be, of its own or in all,
    /// and none of those past the caps may be let go (see
    /// [`Self::forget_old_activities`]).
    fn too_many_kept(&self, chat: TerminalId) -> Option<String> {
        let finished = |r: &&Record| !r.activity.state.active();
        let all = self.tools.activities.iter().filter(finished).count();
        let own = (self.tools.activities.iter())
            .filter(|r| r.activity.owner == chat)
            .filter(finished)
            .count();
        (own > KEPT_PER_CHAT || all > KEPT).then(|| {
            format!(
                "Activity limit reached: {KEPT_PER_CHAT} finished activities per chat and {KEPT} \
                 in all are kept, and those past it have outcomes not yet seen, or unknown, which \
                 are never let go. Inspect them with read_activities before starting more; the \
                 user may close chats whose activities are no longer needed."
            )
        })
    }

    /// Lets go of finished activities past the newest [`KEPT_PER_CHAT`]
    /// of each chat's and [`KEPT`] in all, oldest first, with their ids:
    /// one reused after that starts afresh. Never one running, nor one
    /// whose agent hasn't been told how it ended, nor one whose outcome
    /// isn't known, which its agent must see before anything is redone
    /// (see [`Self::too_many_kept`]).
    pub(in crate::server) fn forget_old_activities(&mut self) {
        let mut kept_of = std::collections::HashMap::new();
        let mut kept = 0;
        let mut keep = vec![true; self.tools.activities.len()];
        let mut owners = std::collections::BTreeSet::new();
        // Newest first: what's protected is counted, and kept, as it
        // comes; what's past the caps of the rest goes.
        for (at, record) in self.tools.activities.iter().enumerate().rev() {
            if record.activity.state.active() {
                continue;
            }
            let of = kept_of.entry(record.activity.owner).or_insert(0);
            if (*of < KEPT_PER_CHAT && kept < KEPT) || protected(record) {
                *of += 1;
                kept += 1;
            } else {
                keep[at] = false;
                owners.insert(record.activity.owner);
            }
        }
        if owners.is_empty() {
            return;
        }
        let mut keep = keep.into_iter();
        (self.tools.activities).retain(|_| keep.next().unwrap_or(true));
        for owner in owners {
            self.publish_activities(owner);
        }
    }

    /// The chat `chat` has closed: its activities go, as nothing can read
    /// them now. What they run in the background goes on, as the user's.
    pub(in crate::server) fn forget_activities_of(&mut self, chat: TerminalId) {
        let had = self.tools.activities.len();
        (self.tools.activities).retain(|r| r.activity.owner != chat);
        if self.tools.activities.len() != had {
            self.keep_soon();
        }
    }

    pub(in crate::server) fn activities_written(&mut self, serial: u64) {
        let ready: Vec<_> = self
            .tools
            .activities
            .iter_mut()
            .filter_map(|r| {
                if r.waiting_for.is_some_and(|s| s <= serial)
                    && r.activity.state == ActivityState::Starting
                {
                    r.waiting_for = None;
                    Some((
                        r.terminal,
                        r.activity.owner,
                        r.activity.command.clone(),
                        r.activity.title.clone(),
                    ))
                } else {
                    None
                }
            })
            .collect();
        for (terminal, chat, command, title) in ready {
            if self
                .chat_ref(chat)
                .is_some_and(|c| c.tools() != AgentTools::Off && !c.limits().no_commands())
            {
                self.type_at_first_prompt(terminal, command, Some(chat), Some(title));
            } else if let Some(r) = self
                .tools
                .activities
                .iter_mut()
                .find(|r| r.terminal == terminal)
            {
                r.activity.state = ActivityState::Refused;
                r.activity.result = "Permission was revoked before the command was sent.".into();
                r.activity.ended = Some(now());
                self.publish_activities(chat);
            }
        }
    }

    pub(in crate::server) fn restore_cold_activities(
        &mut self,
        records: Vec<Record>,
        chats: &std::collections::HashMap<u32, (TerminalId, bool)>,
    ) {
        let mut owners = std::collections::BTreeSet::new();
        for mut r in records.into_iter().take(128) {
            let Some(&(owner, _)) = chats.get(&r.activity.owner.0) else {
                continue;
            };
            r.activity.owner = owner;
            r.activity.pane = None;
            // Saved by its turn's index: kept by the uid that turn gets
            // as the chat's turns are told again.
            let index = usize::try_from(r.activity.turn).unwrap_or(usize::MAX);
            let uid = (self.chat_ref(owner)).and_then(|chat| chat.turn_uid_to_come(index));
            let Some(uid) = uid else {
                continue;
            };
            r.activity.turn = uid.0;
            if r.activity.state.active() {
                r.activity.state = ActivityState::Unknown;
                r.activity.result = "The daemon restarted. The previous process outcome is unknown; this command was not rerun.".into();
                r.activity.ended = Some(now());
            }
            r.notified = true; // Cold restore never creates an autonomous new turn.
            r.waiting_for = None;
            owners.insert(owner);
            self.tools.activities.push(r);
        }
        // An older build kept them all.
        self.forget_old_activities();
        for owner in owners {
            self.publish_activities(owner);
        }
    }

    pub(super) fn read_activities(
        &self,
        chat: TerminalId,
        id: Option<&str>,
    ) -> Result<String, String> {
        let hosted = self.chat_ref(chat);
        let records: Vec<Activity> = self
            .tools
            .activities
            .iter()
            .filter(|r| r.activity.owner == chat && id.is_none_or(|id| r.activity.id == id))
            // On the turn's index, as the agent counts turns: those of
            // turns on other branches aren't its.
            .filter_map(|r| {
                let mut activity = r.activity.clone();
                activity.turn = shown_turn(hosted?, activity.turn)?;
                Some(activity)
            })
            .collect();
        if id.is_some() && records.is_empty() {
            return Err("No operation with that id belongs to this chat.".into());
        }
        serde_json::to_string(&records).map_err(|err| err.to_string())
    }

    pub(in crate::server) fn cancel_activity(
        &mut self,
        chat: TerminalId,
        id: &str,
    ) -> Result<String, String> {
        let at = self
            .tools
            .activities
            .iter()
            .position(|r| r.activity.owner == chat && r.activity.id == id)
            .ok_or("No operation with that id belongs to this chat.")?;
        if !self.tools.activities[at].activity.state.active()
            || self.tools.activities[at].activity.state == ActivityState::Cancelling
        {
            return self.read_activities(chat, Some(id));
        }
        let terminal = self.tools.activities[at].terminal;
        if self.tools.activities[at].activity.state == ActivityState::Starting {
            self.tools.activities[at].waiting_for = None;
            self.tools.activities[at].activity.state = ActivityState::Refused;
            self.tools.activities[at].activity.result =
                "Cancelled before the command was sent.".into();
            self.tools.activities[at].activity.ended = Some(now());
            if let Some(commanded) = self.tools.commanded.get_mut(&terminal) {
                commanded.typing = None;
                commanded.failed = Some("Cancelled before the command was sent.".into());
            }
            self.publish_activities(chat);
        } else {
            self.type_into(terminal, b"\x03", false)?;
            self.tools.activities[at].activity.state = ActivityState::Cancelling;
            self.publish_activities(chat);
        }
        self.read_activities(chat, Some(id))
    }

    pub(super) fn activity_started(&mut self, terminal: TerminalId) {
        let commanded = self.tools.commanded.get(&terminal);
        let Some(record) = self
            .tools
            .activities
            .iter_mut()
            .find(|r| r.terminal == terminal && r.activity.state.active())
        else {
            return;
        };
        record.activity.state = ActivityState::Running;
        record.expect = commanded.and_then(|c| c.expect);
        record.renumbered = commanded.map_or(0, |c| c.renumbered);
        let owner = record.activity.owner;
        self.publish_activities(owner);
    }

    pub(super) fn observe_activity(&mut self, terminal: TerminalId, ended: bool) {
        let Some(at) = self
            .tools
            .activities
            .iter()
            .position(|r| r.terminal == terminal && r.activity.state.active())
        else {
            return;
        };
        let record = &self.tools.activities[at];
        let commanded = self.tools.commanded.get(&terminal);
        let refused = commanded.and_then(|c| c.failed.clone());
        let grid = self
            .terminals
            .get(&terminal)
            .and_then(|s| s.terminal.grid());
        let progress = grid
            .filter(|g| g.block_renumberings() == record.renumbered)
            .and_then(|g| record.expect.and_then(|block| g.block_progress(block)));
        let starting = record.activity.state == ActivityState::Starting;
        let pane = record.activity.pane;
        let state = if refused.is_some() {
            ActivityState::Refused
        } else if let Some(Progress::Finished(code)) = progress
            // Not as a program prints the marks of an end: only once the
            // shell is back.
            && (ended || self.shell_back(terminal, std::time::Instant::now()))
        {
            code.map_or(ActivityState::Unknown, ActivityState::Exited)
        } else if ended || (!starting && self.finished(terminal) == Some(true)) {
            ActivityState::Unknown
        } else {
            return;
        };
        let result = refused.unwrap_or_else(|| {
            self.describe_terminal(
                pane.unwrap_or(clackatoo_protocol::PaneId(0)),
                terminal,
                crate::mcp::DEFAULT_LINES,
            )
        });
        let record = &mut self.tools.activities[at];
        record.activity.state = state;
        record.activity.result = kept_result(&result);
        record.activity.ended = Some(now());
        let owner = record.activity.owner;
        self.publish_activities(owner);
        self.notify_activity(at);
    }

    fn notify_activity(&mut self, at: usize) {
        let record = &self.tools.activities[at];
        if record.notified
            || self
                .chat_ref(record.activity.owner)
                .is_none_or(|chat| !chat.activity_notifications_allowed())
        {
            return;
        }
        let activity = &record.activity;
        let owner = activity.owner;
        let text = format!(
            "Clackatoo activity {:?} ({}) is {}. Use read_activities with this id to inspect its recorded result before taking further action. This notification grants no additional permission. An unknown outcome must not be retried automatically.",
            activity.id,
            activity.title,
            activity.state.label()
        );
        let message = Message {
            from: Sender {
                chat: owner,
                agent: "Clackatoo".into(),
                title: "Activity completion".into(),
            },
            text,
            hops: 0,
            cause: Some((owner, activity.turn)),
            finished: Vec::new(),
        };
        if let Some(chat) = self.chat(owner)
            && let Ok(actions) = chat.deliver(message)
        {
            self.tools.activities[at].notified = true;
            self.watch_chat_queue(owner);
            self.act(owner, actions);
            self.collect_damage(owner);
        }
    }

    /// Shows chat `owner` its activities, each on the turn that started
    /// it: kept by the turn's uid, shown by its index on the conversation
    /// shown.
    fn publish_activities(&mut self, owner: TerminalId) {
        let records: Vec<Activity> = self
            .tools
            .activities
            .iter()
            .filter(|r| r.activity.owner == owner)
            .map(|r| r.activity.clone())
            .collect();
        if let Some(chat) = self.chat(owner) {
            // An activity of a turn on another branch isn't shown on this.
            let records = (records.into_iter())
                .filter_map(|mut activity| {
                    activity.turn = shown_turn(chat, activity.turn)?;
                    Some(activity)
                })
                .collect();
            chat.set_activities(records);
        }
        self.collect_damage(owner);
        self.keep_soon();
    }

    /// The activities, for the saved workspaces file: each on its turn's
    /// index, as a cold start numbers the turns it tells again afresh.
    pub(in crate::server) fn saved_activities(&self) -> Vec<Record> {
        // Those of turns on other branches, which a cold start doesn't
        // keep, aren't.
        (self.tools.activities.iter())
            .filter_map(|record| {
                let mut record = record.clone();
                let chat = self.chat_ref(record.activity.owner)?;
                record.activity.turn = shown_turn(chat, record.activity.turn)?;
                Some(record)
            })
            .collect()
    }

    pub(in crate::server) fn hand_over_activities(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&(1u32, &self.tools.activities))
    }

    pub(in crate::server) fn resume_activities(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let Ok((1, records)) = serde_json::from_slice::<(u32, Vec<Record>)>(data) else {
            log::error!("Could not restore managed activities; no command was retried");
            return;
        };
        self.tools.activities = records;
        let mut owners = std::collections::BTreeSet::new();
        for record in &mut self.tools.activities {
            owners.insert(record.activity.owner);
            if record.activity.state == ActivityState::Starting {
                record.activity.state = ActivityState::Unknown;
                record.activity.result = "Upgrade interrupted startup before a confirmed send. Inspect the terminal before retrying.".into();
                record.activity.ended = Some(now());
            } else if record.activity.state.active() {
                self.tools.commanded.insert(
                    record.terminal,
                    Commanded {
                        owner: Some(record.activity.owner),
                        name: Some(record.activity.title.clone()),
                        expect: record.expect,
                        renumbered: record.renumbered,
                        ..Commanded::default()
                    },
                );
            }
        }
        // An older build kept them all.
        self.forget_old_activities();
        for owner in owners {
            self.publish_activities(owner);
        }
        let terminals: Vec<_> = self
            .tools
            .activities
            .iter()
            .filter(|r| r.activity.state.active())
            .map(|r| r.terminal)
            .collect();
        for terminal in terminals {
            self.observe_activity(terminal, !self.terminals.contains_key(&terminal));
        }
    }

    pub(in crate::server) fn harness_capabilities(&self, chat: TerminalId) -> String {
        let own = self.chat_ref(chat);
        let tools = own.map_or(AgentTools::Off, crate::chat::HostedChat::tools);
        let connections: Vec<_> = self
            .sources
            .iter()
            .filter_map(|(_, source)| match source {
                Source::Tools(c) if c.chat == chat && !c.finished => Some(c.listed),
                _ => None,
            })
            .collect();
        let native = self.tools.native.get(&chat).map_or_else(
            || "Off — no window granted".into(),
            |(_, access)| format!("{access:?} — a window is connected"),
        );
        let activities = if !self.activity_storage_enabled() {
            "Unavailable: durable workspace storage is disabled".into()
        } else if !crate::shell::marks(&self.config.shell, self.config.shell_integration.as_deref())
        {
            "Unavailable: shell integration is missing".into()
        } else if tools == AgentTools::Off {
            "Unavailable: harness tools are Off".into()
        } else if own.is_none_or(|c| c.limits().no_commands()) {
            "Read-only: starting and cancelling jobs is refused".into()
        } else {
            format!(
                "Available; {MAX_RUNNING} running / last {KEPT_PER_CHAT} finished kept per chat"
            )
        };
        let rows = vec![
            ("Harness tools", format!("{tools:?}; individual calls still enforce project and command permissions")),
            ("Command restrictions", if own.is_none_or(|c| c.limits().no_commands()) { "Read-only: commands refused".into() } else { "Commands permitted subject to Tools policy".into() }),
            ("Browser service", if self.config.browser.is_some() { "Available".into() } else { "Not configured".into() }),
            ("Browser permission", if own.is_some_and(crate::chat::HostedChat::browser) { "Enabled; first page still asks".into() } else { "Off — enable in permissions".into() }),
            ("Native window", native),
            ("Managed activities", activities),
            ("Tool connection", if connections.is_empty() { "No live MCP connection".into() } else { "Connected".into() }),
            ("Catalogue discovery", if connections.contains(&true) { "Requested by the provider connection".into() } else { "Not observed on a live connection".into() }),
            ("Model acknowledgment", "Unconfirmed — this provider supplies no acknowledgment that the model saw its tools".into()),
        ];
        serde_json::to_string(&rows).unwrap_or_default()
    }
}

/// Whether a finished activity is never let go: its agent hasn't been
/// told how it ended, or how it ended isn't known.
fn protected(record: &Record) -> bool {
    !record.notified || record.activity.state == ActivityState::Unknown
}

/// Where turn `uid` of `chat` is on the conversation shown, as an
/// activity's card says: `None` if it isn't, being on another branch.
fn shown_turn(chat: &crate::chat::HostedChat, uid: u32) -> Option<u32> {
    let index = chat.turn_index_to_come(crate::chat::TurnUid(uid))?;
    u32::try_from(index).ok()
}

/// The most of an activity's result kept, in bytes.
const MOST_RESULT: usize = 4096;

/// What's kept of `result`, an activity's terminal described: at most
/// [`MOST_RESULT`] bytes, from its end, which holds the newest output and
/// how the command ended, after `…` if it's cut.
fn kept_result(result: &str) -> String {
    if result.len() <= MOST_RESULT {
        return result.to_owned();
    }
    let mut start = result.len() - (MOST_RESULT - '…'.len_utf8());
    while !result.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &result[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_result_keeps_its_end_with_how_the_command_ended() {
        use std::fmt::Write as _;

        assert_eq!(
            kept_result("$ make\nok\n[finished]"),
            "$ make\nok\n[finished]"
        );
        let mut result = "$ make\n".to_owned();
        for line in 0..2000 {
            let _ = writeln!(result, "é {line}");
        }
        result.push_str("[exited with status 2]");
        let kept = kept_result(&result);
        assert!(kept.len() <= MOST_RESULT, "{}", kept.len());
        assert!(kept.starts_with('…'), "{kept}");
        assert!(kept.ends_with("é 1999\n[exited with status 2]"), "{kept}");
    }

    const JOURNAL: &str = r#"[1,[{"activity":{"id":"build","owner":42,"turn":2,"title":"Build","command":"make","cwd":"/src","pane":7,"started":100,"ended":null,"state":"Running","result":""},"terminal":8,"expect":3,"renumbered":0,"notified":false}]]"#;

    #[test]
    fn version_one_operation_shape_is_frozen_and_pending_writes_are_not_restored() {
        let (version, mut records): (u32, Vec<Record>) = serde_json::from_str(JOURNAL).unwrap();
        assert_eq!(version, 1);
        records[0].waiting_for = Some(19);
        assert_eq!(
            serde_json::to_string(&(version, &records)).unwrap(),
            JOURNAL
        );
        let (_, restored): (u32, Vec<Record>) = serde_json::from_str(JOURNAL).unwrap();
        assert_eq!(restored[0].waiting_for, None);
        assert_eq!(restored[0].expect, Some(3));
    }

    #[test]
    fn cold_recovery_remaps_owner_discards_stale_pane_and_never_starts_work() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let (_, mut records): (u32, Vec<Record>) = serde_json::from_str(JOURNAL).unwrap();
        // Saved on the chat's turn 3, before its turns are told again.
        records[0].activity.turn = 3;
        let before = daemon.terminals.len();
        daemon.restore_cold_activities(
            records,
            &std::collections::HashMap::from([(42, (chat, true))]),
        );
        let r = &daemon.tools.activities[0];
        assert_eq!(r.activity.owner, chat);
        assert_eq!(r.activity.pane, None);
        assert_eq!(r.activity.state, ActivityState::Unknown);
        assert!(r.notified);
        assert!(daemon.tools.commanded.is_empty());
        assert_eq!(daemon.terminals.len(), before);
        // Kept by the uid turn 3 gets as it's told again, never the index
        // (turns are numbered from 1000 in tests), and shown on turn 3.
        let hosted = daemon.chat_ref(chat).unwrap();
        let uid = hosted.turn_uid_to_come(3).unwrap();
        assert_ne!(uid.0, 3);
        assert_eq!(r.activity.turn, uid.0);
        assert_eq!(hosted.node().activities[0].turn, 3);
        assert_eq!(daemon.saved_activities()[0].activity.turn, 3);
    }

    /// The notice that an activity ended, to a chat the user stopped, is
    /// held for them as other agents' messages are, and begins once they
    /// write; it isn't lost (CLACKA-220).
    #[test]
    fn an_activitys_notice_to_a_stopped_chat_waits_for_the_user() {
        use agent_client_protocol_schema::v1::StopReason;
        use clackatoo_protocol::ChatAction;
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let hosted = daemon.chat(chat).unwrap();
        drop(hosted.act(ChatAction::Stop));
        hosted.answered(Ok(StopReason::Cancelled));
        let (_, mut records): (u32, Vec<Record>) = serde_json::from_str(JOURNAL).unwrap();
        records[0].activity.owner = chat;
        records[0].activity.state = ActivityState::Exited(0);
        daemon.tools.activities = records;
        daemon.notify_activity(0);
        assert!(daemon.tools.activities[0].notified);
        let hosted = daemon.chat(chat).unwrap();
        assert_eq!(hosted.relations().inbox.len(), 1, "held");
        assert_eq!(hosted.node().turns.len(), 1, "nothing begun");
        drop(hosted.act(ChatAction::Send("go on".to_owned())));
        hosted.answered(Ok(StopReason::EndTurn));
        assert!(!hosted.deliver_next().is_empty(), "begun after the user's");
    }

    /// A finished activity whose outcome isn't known, or whose agent
    /// hasn't heard how it ended, is never let go, whichever chat's it is,
    /// nor through an upgrade from a build that kept them all: others go
    /// in its place, oldest first, and with only those past the caps, no
    /// more start (CLACKA-220).
    #[test]
    fn activities_not_yet_seen_are_never_let_go() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let (_, records): (u32, Vec<Record>) = serde_json::from_str(JOURNAL).unwrap();
        let record = |owner: TerminalId, n: usize, state: ActivityState, notified: bool| {
            let mut record = records[0].clone();
            record.activity.owner = owner;
            record.activity.id = format!("op-{owner}-{n}");
            record.activity.state = state;
            record.notified = notified;
            record
        };
        let b = TerminalId(77);
        let mut all = Vec::new();
        // The oldest: a's, not yet seen; then b's, seen, newer.
        for n in 0..KEPT_PER_CHAT + 4 {
            let (state, notified) = if n % 2 == 0 {
                (ActivityState::Unknown, true)
            } else {
                (ActivityState::Exited(0), false)
            };
            all.push(record(a, n, state, notified));
        }
        for owner in 80..84 {
            for n in 0..KEPT_PER_CHAT {
                all.push(record(TerminalId(owner), n, ActivityState::Exited(0), true));
            }
        }
        for n in 0..4 {
            all.push(record(b, n, ActivityState::Exited(1), true));
        }
        // As an upgrade from a build that kept them all brings them.
        daemon.resume_activities(&serde_json::to_vec(&(1, &all)).unwrap());
        let of = |daemon: &Daemon, owner: TerminalId| {
            (daemon.tools.activities.iter())
                .filter(|r| r.activity.owner == owner)
                .count()
        };
        assert_eq!(of(&daemon, a), KEPT_PER_CHAT + 4, "none of a's let go");
        assert_eq!(of(&daemon, b), 4, "the newest stay");
        let finished = daemon.tools.activities.len();
        assert_eq!(
            finished,
            KEPT + KEPT_PER_CHAT + 4,
            "only what's protected past the cap"
        );
        // a's are past its own cap, and none may go: it starts no more.
        let refused = daemon.too_many_kept(a).unwrap();
        assert!(refused.contains("never let go"), "{refused}");
        // The cap in all is past too, by a's alone: so is every chat's.
        assert!(daemon.too_many_kept(b).is_some());
        daemon.forget_activities_of(a);
        assert!(daemon.too_many_kept(b).is_none());
    }

    /// Finished activities don't fill the caps for good: only the newest
    /// are kept, per chat and in all, those running always, and a chat
    /// that closes takes its own with it (CLACKA-220).
    #[test]
    fn only_so_many_finished_activities_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, a) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let (_, records): (u32, Vec<Record>) = serde_json::from_str(JOURNAL).unwrap();
        let record = |owner: TerminalId, n: usize, state: ActivityState| {
            let mut record = records[0].clone();
            record.activity.owner = owner;
            record.activity.id = format!("op-{owner}-{n}");
            record.activity.state = state;
            record.notified = true;
            record
        };
        let b = TerminalId(77);
        for n in 0..100 {
            let owner = if n % 2 == 0 { a } else { b };
            (daemon.tools.activities).push(record(owner, n, ActivityState::Exited(0)));
        }
        (daemon.tools.activities).push(record(a, 100, ActivityState::Running));
        assert!(
            daemon.too_many_running(a).is_none(),
            "finished ones don't count"
        );
        daemon.forget_old_activities();
        let of = |daemon: &Daemon, owner: TerminalId| {
            (daemon.tools.activities.iter())
                .filter(|r| r.activity.owner == owner)
                .map(|r| r.activity.id.clone())
                .collect::<Vec<_>>()
        };
        let kept = of(&daemon, a);
        assert_eq!(kept.len(), KEPT_PER_CHAT + 1, "{kept:?}");
        assert_eq!(kept.first().unwrap(), &format!("op-{a}-68"), "the newest");
        assert!(kept.contains(&format!("op-{a}-100")), "running");
        assert_eq!(of(&daemon, b).len(), KEPT_PER_CHAT);
        // Seven more chats' newest fill what's kept in all.
        for owner in 80..88 {
            for n in 0..KEPT_PER_CHAT {
                let owner = TerminalId(owner);
                (daemon.tools.activities).push(record(owner, n, ActivityState::Exited(0)));
            }
        }
        daemon.forget_old_activities();
        let finished = (daemon.tools.activities.iter())
            .filter(|r| !r.activity.state.active())
            .count();
        assert_eq!(finished, KEPT);
        // As many running as may be, in a chat, hold the next.
        for n in 0..MAX_RUNNING {
            (daemon.tools.activities).push(record(b, 200 + n, ActivityState::Running));
        }
        assert!(daemon.too_many_running(b).is_some());
        daemon.forget_activities_of(b);
        assert!(of(&daemon, b).is_empty());
        assert!(daemon.too_many_running(b).is_none());
    }

    #[test]
    fn upgrade_does_not_replay_an_unconfirmed_start_or_missing_process() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = crate::server::agent_files::tests::daemon_with_chat(dir.path());
        let (_, mut records): (u32, Vec<Record>) = serde_json::from_str(JOURNAL).unwrap();
        records[0].activity.owner = chat;
        records[0].activity.state = ActivityState::Starting;
        records[0].waiting_for = Some(4);
        let starting = serde_json::to_vec(&(1, &records)).unwrap();
        daemon.resume_activities(&starting);
        assert_eq!(
            daemon.tools.activities[0].activity.state,
            ActivityState::Unknown
        );
        assert!(daemon.tools.commanded.is_empty());
        records[0].activity.state = ActivityState::Running;
        daemon.resume_activities(&serde_json::to_vec(&(1, &records)).unwrap());
        assert_eq!(
            daemon.tools.activities[0].activity.state,
            ActivityState::Unknown
        );
        assert!(daemon.tools.commanded.values().all(|c| c.typing.is_none()));
    }
}
