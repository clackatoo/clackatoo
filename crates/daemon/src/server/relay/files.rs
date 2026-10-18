//! Another machine's file explorers and folders' listings, and the file
//! operations asked of them, through this daemon (CLACKA-117, phase 3).
//!
//! The daemon there lists its folders, watches them and moves its files:
//! nothing of its files is touched here. This daemon follows each of its
//! explorers there once, as one client, however many of its own follow
//! it. Which folders are open in an explorer is each client's own, so a
//! folder is opened there while any client here has it open, and closed
//! once none has; and a folder's listing goes only to the clients that
//! have it open, as the daemon there would send it. What else an explorer
//! says (its root, and what git says of its files) goes to every client
//! following it.
//!
//! A file operation names no explorer, only paths, so a client says which
//! pane it's asked from ([`ClientMessage::FileOp`]'s `pane`, protocol 92),
//! whose machine its paths are on: one on another machine is asked of the
//! daemon there, numbered afresh for the connection, and how it went
//! comes back to the client under its own number.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use clackatoo_protocol::frame::{self, Head};
use clackatoo_protocol::{
    ClientMessage, Event, FileOp, FileOpError, FilesAction, FilesId, PaneId, Request, Response,
};
use mio::Token;

use super::editors::Out;
use super::unpack::{Need, Unpacked};
use super::{Expect, Route, Routed, Then};
use crate::server::{Daemon, Source};

/// One of another daemon's file explorers, followed by clients of this
/// one.
#[derive(Debug, Default)]
pub(in crate::server) struct FollowedFiles {
    /// The clients following it, and the folders each has open in it.
    viewers: BTreeMap<Token, BTreeSet<String>>,
    /// It's attached there.
    upstream: bool,
}

impl FollowedFiles {
    /// Whether any client has `dir` open.
    fn open(&self, dir: &str) -> bool {
        self.viewers.values().any(|open| open.contains(dir))
    }
}

/// Whether `path` is `dir`, or inside it.
fn within(path: &str, dir: &str) -> bool {
    Path::new(path).starts_with(dir)
}

impl Daemon {
    /// The connection whose workspaces show this daemon's explorer
    /// `local`, and its number there.
    pub(in crate::server) fn relayed_files(&self, local: FilesId) -> Option<(usize, FilesId)> {
        (self.links.list.iter().enumerate()).find_map(|(at, linked)| {
            let remote = linked.relayed.mirror.files_numbers.remote(local.0)?;
            Some((at, FilesId(remote)))
        })
    }

    /// Has the client at `token` follow this daemon's explorer `local`, if
    /// it's another daemon's, as [`Request::AttachFiles`] asks.
    pub(super) fn route_attach_files(&mut self, token: Token, local: FilesId) -> Option<Routed> {
        let (at, remote) = self.relayed_files(local)?;
        let followed = self.links.list[at].relayed.files.entry(local).or_default();
        followed.viewers.entry(token).or_default();
        if !self.is_linked(at) {
            // Attached there once the connection's made again.
            return Some(Routed::Answered(Response::Done));
        }
        let attach = Request::AttachFiles { files: remote };
        let routed = self.wait_for(token, at, attach, Then::Pass);
        if matches!(routed, Routed::Waiting)
            && let Some(followed) = self.links.list[at].relayed.files.get_mut(&local)
        {
            followed.upstream = true;
        }
        Some(routed)
    }

    /// Stops the client at `token` following this daemon's explorer
    /// `local`, if it's another daemon's.
    pub(super) fn route_detach_files(&mut self, token: Token, local: FilesId) -> Option<Routed> {
        let (at, _) = self.relayed_files(local)?;
        self.unfollow_files(token, at, local);
        Some(Routed::Answered(Response::Done))
    }

    /// Stops the client at `token` following this daemon's explorer
    /// `local`, of the connection at `at`: the folders only it had open
    /// are closed there, and the explorer is let go of there once no
    /// client follows it.
    fn unfollow_files(&mut self, token: Token, at: usize, local: FilesId) {
        let relayed = &mut self.links.list[at].relayed;
        let Some(followed) = relayed.files.get_mut(&local) else {
            return;
        };
        let Some(open) = followed.viewers.remove(&token) else {
            return;
        };
        if followed.viewers.is_empty() {
            let upstream = followed.upstream;
            relayed.files.remove(&local);
            if upstream && let Some(remote) = relayed.mirror.files_numbers.remote(local.0) {
                let detach = Request::DetachFiles {
                    files: FilesId(remote),
                };
                let _ = self.send_upstream(at, &ClientMessage::Request(detach), Expect::Nothing);
            }
            return;
        }
        self.close_unopened(at, local, &open);
    }

    /// Closes there the folders of `closed`, of this daemon's explorer
    /// `local` of the connection at `at`, that no client here has open any
    /// more. Closing a folder there closes those inside it too, so any of
    /// them a client still has open is opened again.
    fn close_unopened(&mut self, at: usize, local: FilesId, closed: &BTreeSet<String>) {
        let relayed = &self.links.list[at].relayed;
        let Some(remote) = relayed.mirror.files_numbers.remote(local.0) else {
            return;
        };
        let Some(followed) = relayed.files.get(&local) else {
            return;
        };
        if !followed.upstream {
            return;
        }
        let unopened: Vec<&String> = (closed.iter()).filter(|dir| !followed.open(dir)).collect();
        // Only the outermost: closing one closes what's inside it.
        let outermost: Vec<String> = (unopened.iter())
            .filter(|dir| !(unopened.iter()).any(|other| other != *dir && within(dir, other)))
            .map(|dir| (*dir).clone())
            .collect();
        let again: BTreeSet<String> = (followed.viewers.values().flatten())
            .filter(|open| outermost.iter().any(|dir| within(open, dir)))
            .cloned()
            .collect();
        let files = FilesId(remote);
        let actions = (outermost.into_iter().map(FilesAction::Collapse))
            .chain(again.into_iter().map(FilesAction::Expand));
        for action in actions.collect::<Vec<_>>() {
            let message = ClientMessage::Files { files, action };
            let _ = self.send_upstream(at, &message, Expect::Nothing);
        }
    }

    /// Does what the client at `token` asked of this daemon's explorer
    /// `local`, if it's another daemon's: there. Returns whether it was.
    pub(super) fn route_files(
        &mut self,
        token: Token,
        local: FilesId,
        action: &FilesAction,
    ) -> bool {
        let Some((at, remote)) = self.relayed_files(local) else {
            return false;
        };
        let relayed = &mut self.links.list[at].relayed;
        let Some(open) =
            (relayed.files.get_mut(&local)).and_then(|followed| followed.viewers.get_mut(&token))
        else {
            log::debug!("{action:?} in explorer {local} from a client not following it");
            return true;
        };
        let files = remote;
        match action {
            // Opened again, it's listed again, for all that have it open.
            FilesAction::Expand(dir) => {
                open.insert(dir.clone());
                let expand = ClientMessage::Files {
                    files,
                    action: action.clone(),
                };
                let _ = self.send_upstream(at, &expand, Expect::Nothing);
            }
            FilesAction::Collapse(dir) => {
                let closed: BTreeSet<String> = (open.iter())
                    .filter(|open| within(open, dir))
                    .cloned()
                    .collect();
                open.retain(|open| !within(open, dir));
                self.close_unopened(at, local, &closed);
            }
            FilesAction::Root(_) | FilesAction::Refresh => {
                let message = ClientMessage::Files {
                    files,
                    action: action.clone(),
                };
                let _ = self.send_upstream(at, &message, Expect::Nothing);
            }
        }
        true
    }

    /// What's done with a frame from the daemon reached at `token`, whose
    /// first numbers are `head`, if it's a change to one of its explorers
    /// followed here: passed on as it came, renumbered.
    pub(super) fn files_route(&self, token: Token, head: &Head) -> Option<(Route, Need)> {
        let (at, wire) = self.reached_at(token)?;
        let relayed = &self.links.list[at].relayed;
        let remote = head.get(2)?;
        let local = FilesId(relayed.mirror.files_numbers.local(remote)?);
        let followed = relayed.files.get(&local)?;
        let need = self.pass_on_to(followed.viewers.keys().copied(), wire);
        Some((Route::Files { local }, need))
    }

    /// Passes on a change to this daemon's explorer `local`, of the
    /// connection at `at`, as `unpacked` holds it: a folder's listing to
    /// the clients that have it open, and the rest to all.
    pub(super) fn relay_files_frame(&mut self, at: usize, local: FilesId, unpacked: &Unpacked) {
        let wire = self.link_wire(at);
        let Some(came) = unpacked.for_wire(wire).and_then(|frame| frame.get(4..)) else {
            return;
        };
        let dir = frame::files_dir(came).map(str::to_owned);
        let Some(followed) = self.links.list[at].relayed.files.get(&local) else {
            return;
        };
        let to: Vec<Token> = (followed.viewers.iter())
            .filter(|(_, open)| dir.as_ref().is_none_or(|dir| open.contains(dir)))
            .map(|(&token, _)| token)
            .collect();
        for token in to {
            if let Some(frame) = self.frame_for(at, token, unpacked, &[(2, u64::from(local.0))]) {
                self.send_out(token, Out::Frame(frame));
            }
        }
    }

    /// Asks the daemon there to do the file operation `op` the client at
    /// `token` numbered `id`, asked from `pane`, if that's one of another
    /// daemon's: numbered afresh, how it went coming back under the
    /// client's own number. Returns whether it was.
    pub(super) fn route_file_op(
        &mut self,
        token: Token,
        id: u64,
        op: &FileOp,
        pane: Option<PaneId>,
    ) -> bool {
        let Some((at, remote)) = pane.and_then(|pane| self.relayed_pane(pane)) else {
            return false;
        };
        let refused = if self.links.list[at].relayed.relays {
            let number = self.links.list[at].relayed.ops.number(token, id);
            let asked = ClientMessage::FileOp {
                id: number,
                op: op.clone(),
                pane: Some(remote),
            };
            self.send_upstream(at, &asked, Expect::Nothing).err()
        } else {
            Some(self.too_old(at, "a file operation"))
        };
        if let Some(why) = refused {
            let failed = FileOpError::Failed(match why {
                clackatoo_protocol::Error::Other(why) => why,
                why => why.to_string(),
            });
            self.send_event(
                token,
                Event::FileOp {
                    id,
                    outcome: Err(failed),
                },
            );
        }
        true
    }

    /// What's done with a frame from the daemon reached at `token`,
    /// `payload`, if it says how a file operation asked of it went: passed
    /// on, as it came, to the client that asked.
    pub(super) fn file_op_route(&self, token: Token, payload: &[u8]) -> Option<(Route, Need)> {
        let (at, wire) = self.reached_at(token)?;
        let [_, _, number] = frame::leading::<3>(payload)?;
        let (asker, _) = self.links.list[at].relayed.ops.client(number)?;
        Some((Route::FileOp { number }, self.pass_on_to([asker], wire)))
    }

    /// Passes on how the file operation the connection at `at` numbered
    /// `number` went, as `unpacked` holds it, to the client that asked.
    pub(super) fn relay_file_op_frame(&mut self, at: usize, number: u64, unpacked: &Unpacked) {
        let ops = &mut self.links.list[at].relayed.ops;
        let Some((token, id)) = ops.client(number) else {
            return;
        };
        ops.forget(number);
        if let Some(frame) = self.frame_for(at, token, unpacked, &[(2, id)]) {
            self.send_out(token, Out::Frame(frame));
        }
    }

    /// The connection at `at` has been made: the explorers followed
    /// through it are attached there again, with the folders open here
    /// opened there.
    pub(super) fn files_linked(&mut self, at: usize) {
        let relayed = &mut self.links.list[at].relayed;
        let mut messages = Vec::new();
        for (local, followed) in &mut relayed.files {
            let Some(remote) = relayed.mirror.files_numbers.remote(local.0) else {
                continue;
            };
            let files = FilesId(remote);
            messages.push(ClientMessage::Request(Request::AttachFiles { files }));
            let open: BTreeSet<&String> = followed.viewers.values().flatten().collect();
            messages.extend(open.into_iter().map(|dir| ClientMessage::Files {
                files,
                action: FilesAction::Expand(dir.clone()),
            }));
            followed.upstream = true;
        }
        for message in messages {
            let _ = self.send_upstream(at, &message, Expect::Nothing);
        }
    }

    /// Forgets the client at `token`, gone: it follows none of other
    /// daemons' explorers any more, nor waits to hear of file operations.
    pub(super) fn forget_files_client(&mut self, token: Token) {
        for at in 0..self.links.list.len() {
            let followed: Vec<FilesId> = (self.links.list[at].relayed.files.iter())
                .filter(|(_, followed)| followed.viewers.contains_key(&token))
                .map(|(&local, _)| local)
                .collect();
            for local in followed {
                self.unfollow_files(token, at, local);
            }
            self.links.list[at].relayed.ops.forget_client(token);
        }
    }

    /// The connection reached at `token`, and the protocol version it runs
    /// at.
    pub(super) fn reached_at(&self, token: Token) -> Option<(usize, u32)> {
        let Some(Source::Reached(reached)) = self.sources.get(token.0) else {
            return None;
        };
        Some((self.links.find(&reached.host)?, reached.wire))
    }

    /// How a frame from a connection at protocol `wire` is unpacked to be
    /// passed on to the clients at `tokens`: for the version each speaks,
    /// and the connection's own.
    pub(super) fn pass_on_to(&self, tokens: impl IntoIterator<Item = Token>, wire: u32) -> Need {
        let mut wires: Vec<u32> = (tokens.into_iter())
            .filter_map(|token| self.client_wire(token))
            .chain([wire])
            .collect();
        wires.sort_unstable();
        wires.dedup();
        Need::PassOn {
            terminal: None,
            wires,
        }
    }
}
