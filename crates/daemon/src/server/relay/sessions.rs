//! An agent's past sessions on another machine, for the start page of a
//! pane there (CLACKA-163): the sessions the agent kept that a new chat can
//! go on with.
//!
//! A window names the machine whose agent it asks
//! ([`ClientMessage::ListSessions`]' `on`). For another machine this daemon
//! is connected to, the daemon there is asked, as any client asks it, of
//! its own agent and directory: under a number of the connection's, the
//! window's given back with its answer. What it lists is untrusted, as
//! anything from another daemon: no more sessions than a listing here
//! keeps, those with an ID no agent gives, or in another directory than
//! asked, left out, titles made safe to show and cut short, and the chat
//! that has one open by this daemon's number for it, or none.
//!
//! A chat going on with one is made as any new chat on that machine is:
//! its `resume` passes on unchanged.

use std::collections::BTreeMap;
use std::path::Path;

use clackatoo_protocol::{AgentSession, ClientMessage, Error, Event, MachineId, TerminalId};
use mio::Token;

use super::Expect;
use super::mirror::shown_name;
use crate::server::Daemon;

/// The most sessions of another machine's agent taken in: as many as a
/// listing here keeps.
const MOST_LISTED: usize = crate::agent::MOST_LISTED;

/// The longest ID of a session taken in, in bytes, as from an agent here.
const MOST_ID: usize = 256;

/// The most characters of a session's title kept.
const MOST_TITLE: usize = 200;

/// The longest time a session says it last changed, in bytes.
const MOST_UPDATED: usize = 64;

/// The longest reason another daemon gives for listing none, in
/// characters.
const LONGEST_REASON: usize = 1024;

/// The listings asked of one other daemon, by the connection's numbers:
/// the directory each asked about.
#[derive(Debug, Default)]
pub(in crate::server) struct Listings {
    pub(super) numbers: super::editors::Asked,
    cwds: BTreeMap<u64, String>,
}

impl Daemon {
    /// Asks the daemon on machine `on` for the sessions its agent called
    /// `agent` kept in `cwd`, for the client at `token`, which numbered it
    /// `id`, if `on` is another machine than this one. Returns whether it
    /// was: the answer, or why there's none, comes as an
    /// [`Event::Sessions`].
    pub(super) fn route_list_sessions(
        &mut self,
        token: Token,
        id: u64,
        (agent, cwd): (String, String),
        on: Option<MachineId>,
    ) -> bool {
        let Some(machine) = on.filter(|&machine| machine != self.machine.id) else {
            return false;
        };
        if let Err(why) = self.list_sessions_there(token, id, (agent, cwd), machine) {
            let why = match why {
                Error::Other(why) => why,
                why => why.to_string(),
            };
            let sessions = Err(why);
            self.send_event(token, Event::Sessions { id, sessions });
        }
        true
    }

    /// As [`route_list_sessions`](Self::route_list_sessions), for machine
    /// `machine`, another: or why it can't be asked.
    fn list_sessions_there(
        &mut self,
        token: Token,
        id: u64,
        (agent, cwd): (String, String),
        machine: MachineId,
    ) -> Result<(), Error> {
        let Some(at) = self.link_to(machine) else {
            return Err(Error::Other(format!(
                "‘{}’ isn't connected: :connect it first",
                self.machine_named(machine)
            )));
        };
        self.can_ask(at)?;
        let listings = &mut self.links.list[at].relayed.sessions;
        let number = listings.numbers.number(token, id);
        // Only those still waiting for an answer are kept.
        let numbers = &listings.numbers;
        listings
            .cwds
            .retain(|&kept, _| numbers.client(kept).is_some());
        listings.cwds.insert(number, cwd.clone());
        // The daemon there is asked of its own agent: so any daemon, of
        // any version, answers it.
        let list = ClientMessage::ListSessions {
            id: number,
            agent,
            cwd,
            on: None,
        };
        self.send_upstream(at, &list, Expect::Nothing)
    }

    /// Takes in `sessions`, what the daemon of the connection at `at`
    /// listed for its number `number`: passed on, made safe, to the client
    /// that asked, under its own number.
    pub(super) fn take_relayed_sessions(
        &mut self,
        at: usize,
        number: u64,
        sessions: Result<Vec<AgentSession>, String>,
    ) {
        let linked = &mut self.links.list[at];
        let listings = &mut linked.relayed.sessions;
        let Some((token, id)) = listings.numbers.client(number) else {
            return;
        };
        listings.numbers.forget(number);
        let Some(cwd) = listings.cwds.remove(&number) else {
            return;
        };
        let mirror = &linked.relayed.mirror;
        let host = &linked.remote.host;
        let sessions = match sessions {
            Ok(sessions) => Ok((sessions.into_iter())
                .filter_map(|session| {
                    let shown = !session.session.is_empty()
                        && session.session.len() <= MOST_ID
                        && !session.session.contains(char::is_control)
                        && Path::new(&session.cwd) == Path::new(&cwd);
                    shown.then(|| AgentSession {
                        title: shown_name(&session.title)
                            .chars()
                            .take(MOST_TITLE)
                            .collect(),
                        updated: (session.updated).filter(|updated| {
                            updated.len() <= MOST_UPDATED && !updated.contains(char::is_control)
                        }),
                        chat: (session.chat)
                            .and_then(|chat| mirror.terminal_numbers.local(chat.0))
                            .map(TerminalId),
                        session: session.session,
                        cwd: cwd.clone(),
                    })
                })
                .take(MOST_LISTED)
                .collect()),
            Err(why) => {
                let why: String = shown_name(&why).chars().take(LONGEST_REASON).collect();
                Err(format!("{why} (on {host})"))
            }
        };
        self.send_event(token, Event::Sessions { id, sessions });
    }

    /// Forgets the client at `token`, gone: what other daemons list for it
    /// goes nowhere.
    pub(super) fn forget_sessions_client(&mut self, token: Token) {
        for linked in &mut self.links.list {
            let listings = &mut linked.relayed.sessions;
            listings.numbers.forget_client(token);
            let numbers = &listings.numbers;
            listings
                .cwds
                .retain(|&kept, _| numbers.client(kept).is_some());
        }
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::ServerMessage;

    use super::super::tests::{client, desk, heard, linked, read, say};
    use super::*;
    use crate::server::Reading;

    /// The sessions told the client reading `window`, by their numbers.
    fn told(
        window: &mut std::os::unix::net::UnixStream,
    ) -> Vec<(u64, Result<Vec<AgentSession>, String>)> {
        (read::<ServerMessage>(window).into_iter())
            .filter_map(|message| match message {
                ServerMessage::Event(Event::Sessions { id, sessions }) => Some((id, sessions)),
                _ => None,
            })
            .collect()
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one answer after another, each short")]
    fn another_machines_sessions_are_asked_by_its_number_and_made_safe() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        let (window, mut from_daemon) = client(&mut daemon);
        let message = ClientMessage::ListSessions {
            id: 5,
            agent: "claude".into(),
            cwd: "/src".into(),
            on: Some(desk().id),
        };
        assert!(daemon.route_message(window, message).is_none());
        let [
            ClientMessage::ListSessions {
                id: number,
                agent,
                cwd,
                on,
            },
        ] = &heard(&mut there)[..]
        else {
            panic!("not asked there");
        };
        // Asked there of its own agent, under the connection's number.
        assert_eq!(
            (agent.as_str(), cwd.as_str(), *on),
            ("claude", "/src", None)
        );
        let session = |id: &str, cwd: &str, title: &str| AgentSession {
            session: id.into(),
            title: title.into(),
            cwd: cwd.into(),
            updated: Some("2026-10-13T20:54:20Z".into()),
            chat: None,
        };
        let mut sessions = vec![
            AgentSession {
                chat: Some(TerminalId(7)),
                ..session("s1", "/src", "Fix\nthe \u{202e}parser")
            },
            AgentSession {
                chat: Some(TerminalId(99)),
                updated: Some("x".repeat(100)),
                ..session("s2", "/src/", &"t".repeat(500))
            },
            // Elsewhere, and IDs no agent gives, are left out.
            session("s3", "/etc", ""),
            session("", "/src", ""),
            session("bad\nid", "/src", ""),
            session(&"x".repeat(300), "/src", ""),
        ];
        sessions.extend((0..300).map(|n| session(&format!("more-{n}"), "/src", "")));
        let answer = Event::Sessions {
            id: *number,
            sessions: Ok(sessions),
        };
        say(&mut there, &ServerMessage::Event(answer));
        daemon.read_reached(link, Reading::Last);
        let [(5, Ok(listed))] = &told(&mut from_daemon)[..] else {
            panic!("not told");
        };
        assert_eq!(listed.len(), MOST_LISTED);
        assert_eq!(listed[0].title, "Fix the parser");
        // The chat it has open, by this daemon's number; one not shown
        // here, none.
        let shown = daemon.shown_workspaces();
        let local = shown
            .list
            .iter()
            .find(|w| w.home.is_some())
            .unwrap()
            .terminals()[0];
        assert_eq!(listed[0].chat, Some(local));
        assert_eq!(listed[1].chat, None);
        assert_eq!(listed[1].title.chars().count(), MOST_TITLE);
        assert_eq!(listed[1].updated, None);
        assert_eq!(listed[1].cwd, "/src");
        assert!(
            listed
                .iter()
                .all(|session| !session.session.starts_with('s')
                    || ["s1", "s2"].contains(&session.session.as_str()))
        );

        // Answered once: a second answer goes nowhere. And why there are
        // none says where.
        say(
            &mut there,
            &ServerMessage::Event(Event::Sessions {
                id: *number,
                sessions: Ok(Vec::new()),
            }),
        );
        daemon.read_reached(link, Reading::Last);
        assert!(told(&mut from_daemon).is_empty());
        let again = ClientMessage::ListSessions {
            id: 6,
            agent: "claude".into(),
            cwd: "/src".into(),
            on: Some(desk().id),
        };
        assert!(daemon.route_message(window, again).is_none());
        let [ClientMessage::ListSessions { id: number, .. }] = &heard(&mut there)[..] else {
            panic!("not asked there");
        };
        say(
            &mut there,
            &ServerMessage::Event(Event::Sessions {
                id: *number,
                sessions: Err("not\u{1b}logged in".into()),
            }),
        );
        daemon.read_reached(link, Reading::Last);
        assert_eq!(
            told(&mut from_daemon),
            [(6, Err("notlogged in (on desk)".to_owned()))]
        );
    }
}
