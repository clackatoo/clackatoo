//! Managed operations reuse background PTYs, including their upgrade survival.
//! A caller-chosen identity is reserved before typing. A lost response is never
//! grounds for executing it twice. Only observed shell exits imply completion.

use clackatoo_protocol::{Activity, ActivityState, AgentTools, Sender, TerminalId};
use clackatoo_term::Progress;

use super::{Commanded, Daemon, Source};
use crate::chat::{Message, now};

const MAX_PER_CHAT: usize = 64;
const MAX_RUNNING: usize = 8;

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
        let own: Vec<_> = self
            .tools
            .activities
            .iter()
            .filter(|r| r.activity.owner == chat)
            .collect();
        if self.tools.activities.len() >= 128
            || own.len() >= MAX_PER_CHAT
            || own.iter().filter(|r| r.activity.state.active()).count() >= MAX_RUNNING
        {
            return Err("Activity limit reached (64 retained, 8 running per chat). Inspect existing work before starting more.".into());
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
            .map(|r| {
                // On the turn's index, as the agent counts turns.
                let mut activity = r.activity.clone();
                if let Some(hosted) = hosted {
                    activity.turn = shown_turn(hosted, activity.turn);
                }
                activity
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
        let state = if refused.is_some() {
            ActivityState::Refused
        } else if let Some(Progress::Finished(code)) =
            grid.and_then(|g| record.expect.and_then(|block| g.block_progress(block)))
            && grid.is_some_and(|g| g.block_renumberings() == record.renumbered)
        {
            code.map_or(ActivityState::Unknown, ActivityState::Exited)
        } else if ended
            || (record.activity.state != ActivityState::Starting
                && self.finished(terminal) == Some(true))
        {
            ActivityState::Unknown
        } else {
            return;
        };
        let result = refused.unwrap_or_else(|| {
            self.describe_terminal(
                record
                    .activity
                    .pane
                    .unwrap_or(clackatoo_protocol::PaneId(0)),
                terminal,
                crate::mcp::DEFAULT_LINES,
            )
        });
        let record = &mut self.tools.activities[at];
        record.activity.state = state;
        let mut end = result.len().min(4096);
        while !result.is_char_boundary(end) {
            end -= 1;
        }
        record.activity.result = result[..end].into();
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
            let records = (records.into_iter())
                .map(|mut activity| {
                    activity.turn = shown_turn(chat, activity.turn);
                    activity
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
        let mut records = self.tools.activities.clone();
        for record in &mut records {
            if let Some(chat) = self.chat_ref(record.activity.owner) {
                record.activity.turn = shown_turn(chat, record.activity.turn);
            }
        }
        records
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
            format!("Available; {MAX_RUNNING} running / {MAX_PER_CHAT} retained per chat")
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

/// Where turn `uid` of `chat` is on the conversation shown, as an
/// activity's card says; its last turn if it isn't there.
fn shown_turn(chat: &crate::chat::HostedChat, uid: u32) -> u32 {
    let index = (chat.turn_index(crate::chat::TurnUid(uid)))
        .unwrap_or_else(|| chat.turns().saturating_sub(1));
    u32::try_from(index).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (_, records): (u32, Vec<Record>) = serde_json::from_str(JOURNAL).unwrap();
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
