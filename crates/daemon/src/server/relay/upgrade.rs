//! Federation across a local exec. Connections reopen; identities and edit
//! receipts survive. Socket tokens, queued terminal keys and request tickets
//! never survive: they belong to connections which have ended.
use std::io;

use clackatoo_protocol::{BufferId, Machine, MachineId, Passed, WorkspaceId, frame, wire};
use serde::{Deserialize, Serialize};

use super::{editors::FollowedBuffer, mirror};
use crate::handover::{HandoverError, relay};
use crate::remote::Remote;
use crate::server::Daemon;

/// This is a file-format version, not the current protocol. Keep decoding
/// protocol 125 for these views even after raising the connection floor.
/// Its fixture guards this contract. New view fields require a new format.
const VIEW_WIRE: u32 = 125;

#[derive(Debug, Serialize, Deserialize)]
struct View {
    machine: Option<Machine>,
    passed: Vec<Passed>,
    mirror: mirror::Snapshot,
    relays: bool,
}
impl wire::Downgrade for View {}

impl Daemon {
    pub(in crate::server) fn hand_over_federation(&self) -> io::Result<relay::Federation> {
        let peers = self
            .links
            .list
            .iter()
            .map(|linked| {
                let view = View {
                    machine: linked.machine.clone(),
                    passed: linked.passed.clone(),
                    mirror: (&linked.relayed.mirror).into(),
                    relays: linked.relayed.relays,
                };
                let view = frame::encode(VIEW_WIRE, &view).map_err(io::Error::other)?;
                Ok(relay::Peer {
                    host: linked.remote.host.clone(),
                    command: linked.remote.command.clone(),
                    asked: linked.asked,
                    unshared: linked.unshared,
                    expected: linked.machine_id().map(|id| id.0),
                    wire: linked.wire,
                    view,
                    buffers: linked
                        .relayed
                        .buffers
                        .iter()
                        .map(|(&local, followed)| followed.hand_over(local))
                        .collect(),
                })
            })
            .collect::<io::Result<_>>()?;
        Ok(relay::Federation {
            window: self.links.window,
            changes: self.links.changes,
            next_board: self.links.next_board,
            active: self.links.active.map(|id| id.0),
            peers,
            typed_parked: self
                .links
                .typed_parked
                .iter()
                .map(|(machine, key)| (machine.0, *key))
                .collect(),
        })
    }

    pub(in crate::server) fn resume_federation(
        &mut self,
        kept: &relay::Federation,
    ) -> Result<(), HandoverError> {
        if self.config.links.is_none() {
            return Ok(());
        }
        self.links.typed_parked = kept
            .typed_parked
            .iter()
            .map(|(machine, key)| (MachineId(*machine), *key))
            .collect();
        self.links.window = kept.window;
        self.links.changes = kept.changes;
        self.links.next_board = kept.next_board;
        self.links.active = kept.active.map(WorkspaceId);
        // Restore every peer before starting any connection, including
        // those reached through another. Their IDs must exist even while
        // that intermediary is still reconnecting.
        for peer in &kept.peers {
            let mut decoder = frame::FrameDecoder::new();
            decoder.push(&peer.view);
            let view: View = decoder
                .decode(VIEW_WIRE)
                .map_err(io::Error::other)?
                .ok_or(HandoverError::Damaged("missing federation view"))?;
            let mut linked = self.links.new_link(
                Remote {
                    host: peer.host.clone(),
                    command: peer.command.clone(),
                },
                None,
                peer.asked,
            );
            linked.unshared = peer.unshared;
            linked.expected = peer.expected.map(MachineId);
            linked.machine = view.machine;
            linked.passed = view.passed;
            linked.wire = peer.wire;
            linked.relayed.relays = view.relays;
            linked.relayed.mirror = view.mirror.into();
            linked.relayed.buffers = peer
                .buffers
                .iter()
                .map(|b| (BufferId(b.local), FollowedBuffer::resume(b)))
                .collect();
            self.links.list.push(linked);
        }
        for at in 0..self.links.list.len() {
            if self.links.list[at].asked {
                self.set_direct(at);
                self.try_link(at);
            }
        }
        self.sync_chained();
        Ok(())
    }
}

impl Daemon {
    /// A planned exec must not cut off terminal keys already accepted
    /// here. Let the normal event loop receive their replies; never
    /// replay shell input after reconnect or wait in a blocking loop.
    pub(in crate::server) fn relay_keys_hold_upgrade(
        &self,
        until: std::time::Instant,
        now: std::time::Instant,
    ) -> Result<bool, String> {
        let waiting = self.sources.iter().any(|(_, source)| {
            matches!(source, crate::server::Source::Reached(reached)
                if reached.upstream.expecting.iter().any(|e| matches!(e, super::Expect::Keys)))
        });
        if waiting && now >= until {
            Err("remote terminal input is still awaiting acknowledgement; try upgrading once the connection answers".into())
        } else {
            Ok(waiting)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::Reading;
    use crate::server::relay::tests::{client, heard, linked, say};
    use clackatoo_protocol::{ClientMessage, Request, Response, ServerMessage, TerminalId};
    use std::time::{Duration, Instant};

    #[test]
    fn upgrade_waits_for_remote_terminal_input_but_never_replays_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        let (client, _stream) = client(&mut daemon);
        let local = TerminalId(
            daemon.links.list[0]
                .relayed
                .mirror
                .terminal_numbers
                .local(7)
                .unwrap(),
        );
        assert!(matches!(
            daemon.relay_keys(client, 0, local, TerminalId(7), b"hello".to_vec()),
            Response::Done
        ));
        assert!(
            matches!(&heard(&mut there)[..], [ClientMessage::Request(Request::SendInput { bytes, .. })] if bytes == b"hello")
        );
        let now = Instant::now();
        assert_eq!(
            daemon.relay_keys_hold_upgrade(now + Duration::from_secs(1), now),
            Ok(true)
        );
        assert!(daemon.relay_keys_hold_upgrade(now, now).is_err());
        say(&mut there, &ServerMessage::Response(Response::Done));
        daemon.read_reached(link, Reading::Look);
        assert_eq!(daemon.relay_keys_hold_upgrade(now, now), Ok(false));
        assert!(heard(&mut there).is_empty());
    }

    #[test]
    fn frozen_view_keeps_layout_and_both_directions_of_its_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let (daemon, _link, _there) = linked(dir.path());
        let saved = daemon.hand_over_federation().unwrap();
        let mut decoder = frame::FrameDecoder::new();
        decoder.push(&saved.peers[0].view);
        let view: View = decoder.decode(VIEW_WIRE).unwrap().unwrap();
        let mirror: mirror::Mirror = view.mirror.into();
        assert_eq!(
            mirror.workspaces,
            daemon.links.list[0].relayed.mirror.workspaces
        );
        let local = mirror.terminal_numbers.local(7).unwrap();
        assert_eq!(mirror.terminal_numbers.remote(local), Some(7));
        assert_eq!(mirror.run(), Some(1));
    }
    #[test]
    #[ignore = "writes tests/fixtures/federation-view-v1.bin once"]
    fn write_federation_view_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let (daemon, _link, _there) = linked(dir.path());
        let kept = daemon.hand_over_federation().unwrap();
        std::fs::write(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/federation-view-v1.bin"
            ),
            &kept.peers[0].view,
        )
        .unwrap();
    }

    #[test]
    fn the_frozen_federation_view_remains_readable() {
        let mut decoder = frame::FrameDecoder::new();
        decoder.push(include_bytes!(
            "../../../tests/fixtures/federation-view-v1.bin"
        ));
        let view: View = decoder.decode(VIEW_WIRE).unwrap().unwrap();
        assert_eq!(view.machine.unwrap().name, "desk");
        let mirror: mirror::Mirror = view.mirror.into();
        assert_eq!(mirror.workspaces[0].name, "work");
        let local = mirror.terminal_numbers.local(7).unwrap();
        assert_eq!(mirror.terminal_numbers.remote(local), Some(7));
    }
}
