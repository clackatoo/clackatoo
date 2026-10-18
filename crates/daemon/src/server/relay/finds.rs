//! The finder, for a pane on another machine, through this daemon
//! (CLACKA-117, phase 3).
//!
//! A search names the pane whose project it looks in, so one for a pane
//! on another machine is asked of the daemon there, which walks, greps
//! and reads its own files, off its loop, and sends what it finds a batch
//! at a time. Each search is numbered afresh for the connection, and what
//! it finds passed on to the client that asked, under its own number, as
//! it came. The daemon there sends as fast as the connection takes it, so
//! a client slow to read what it's sent has its search stopped there,
//! rather than this daemon hold a backlog of it: it's told what came is
//! all it gets.

use clackatoo_protocol::frame;
use clackatoo_protocol::{ClientMessage, Done, Event, Found, PaneId, Search};
use mio::Token;

use super::editors::Out;
use super::mirror::Fresh;
use super::unpack::{Need, Unpacked};
use super::{Expect, Route};
use crate::server::{Daemon, Source};

/// How much may be waiting to be written to a client before what a search
/// on another machine finds for it stops the search: far more than it
/// takes to fill a finder, and as much as an answer waits behind.
const MOST_WAITING: usize = 4 * 1024 * 1024;

/// The number of `Found::Boards` among what's found.
const BOARDS: u64 = 4;

/// The number of `Found::Done` among what's found.
const DONE: u64 = 6;

impl Daemon {
    /// Asks the daemon there for the search the client at `token`
    /// numbered `id`, in the project of `pane`, if that's on another
    /// machine. Returns whether it was.
    pub(super) fn route_find(
        &mut self,
        token: Token,
        id: u64,
        pane: Option<PaneId>,
        search: &Search,
    ) -> bool {
        let Some(found) = pane.and_then(|pane| self.pane_elsewhere(pane)) else {
            return false;
        };
        let refused = match found {
            Err(why) => Some(why),
            Ok((at, remote)) => self.find_there(token, id, at, remote, search),
        };
        if let Some(why) = refused {
            let why = match why {
                clackatoo_protocol::Error::Other(why) => why,
                why => why.to_string(),
            };
            let found = Found::Done(Done::Failed(why));
            self.send_event(token, Event::Found { id, found });
        }
        true
    }

    /// Asks the daemon of the connection at `at` for the search the client
    /// at `token` numbered `id`, in the project of its pane `remote`: or
    /// says why it can't be.
    fn find_there(
        &mut self,
        token: Token,
        id: u64,
        at: usize,
        remote: PaneId,
        search: &Search,
    ) -> Option<clackatoo_protocol::Error> {
        if self.links.list[at].relayed.relays {
            // Asked again under the same number, it's the same search,
            // which the daemon there stops before starting it again.
            let number = self.links.list[at].relayed.finds.number(token, id);
            let find = ClientMessage::Find {
                id: number,
                pane: Some(remote),
                search: search.clone(),
            };
            self.send_upstream(at, &find, Expect::Nothing).err()
        } else {
            Some(self.too_old(at, "the finder"))
        }
    }

    /// Stops the search the client at `token` numbered `id`, if it's
    /// another machine's. Returns whether it was.
    pub(super) fn route_stop_finding(&mut self, token: Token, id: u64) -> bool {
        let Some((at, number)) = (self.links.list.iter().enumerate())
            .find_map(|(at, linked)| Some((at, linked.relayed.finds.numbered(token, id)?)))
        else {
            return false;
        };
        self.links.list[at].relayed.finds.forget(number);
        let stop = ClientMessage::StopFinding { id: number };
        let _ = self.send_upstream(at, &stop, Expect::Nothing);
        true
    }

    /// What's done with a frame from the daemon reached at `token`,
    /// `payload`, if it's what a search asked of it found: passed on, as
    /// it came, to the client that asked, but for the boards, which are
    /// renumbered.
    pub(super) fn found_route(&self, token: Token, payload: &[u8]) -> Option<(Route, Need)> {
        let (at, wire) = self.reached_at(token)?;
        let [_, _, number, kind] = frame::leading::<4>(payload)?;
        let (asker, _) = self.links.list[at].relayed.finds.client(number)?;
        let need = if kind == BOARDS {
            Need::Decode
        } else {
            self.pass_on_to([asker], wire)
        };
        Some((Route::Found { number, kind }, need))
    }

    /// Passes on what the search the connection at `at` numbered `number`
    /// found, of the kind numbered `kind`, as `unpacked` holds it, to the
    /// client that asked: unless it has too much waiting already, when the
    /// search is stopped, and the client told it's cut short.
    pub(super) fn relay_found(&mut self, at: usize, number: u64, kind: u64, unpacked: Unpacked) {
        let Some((token, id)) = self.links.list[at].relayed.finds.client(number) else {
            return;
        };
        let waiting = match self.sources.get(token.0) {
            Some(Source::Client(client)) => client.outgoing.len(),
            _ => return,
        };
        if waiting > MOST_WAITING {
            log::info!("a search on another machine found more than its client reads: stopped");
            self.route_stop_finding(token, id);
            let found = Found::Done(Done::CutShort);
            self.send_event(token, Event::Found { id, found });
            return;
        }
        if kind == DONE {
            self.links.list[at].relayed.finds.forget(number);
        }
        match unpacked {
            Unpacked::Decoded(Ok(clackatoo_protocol::ServerMessage::Event(Event::Found {
                found: Found::Boards(mut boards),
                ..
            }))) => {
                boards.truncate(super::mirror::MOST_CHATS);
                let (linked, mut fresh) = self.numbering(at);
                if !fresh.have_room() {
                    self.out_of_numbers(at);
                    return;
                }
                let numbers = &mut linked.relayed.mirror.board_numbers;
                for board in &mut boards {
                    board.id.0 = numbers.local_or_fresh(board.id.0, || fresh.board());
                    board.name = super::mirror::shown_name(&board.name);
                }
                let found = Found::Boards(boards);
                self.send_event(token, Event::Found { id, found });
            }
            Unpacked::Decoded(_) => {}
            unpacked @ Unpacked::PassedOn(_) => {
                if let Some(frame) = self.frame_for(at, token, &unpacked, &[(2, id)]) {
                    self.send_out(token, Out::Frame(frame));
                }
            }
        }
    }

    /// Forgets the client at `token`, gone: its searches on other machines
    /// are stopped.
    pub(super) fn forget_finds_client(&mut self, token: Token) {
        for at in 0..self.links.list.len() {
            let finds = &mut self.links.list[at].relayed.finds;
            let numbers: Vec<u64> = finds.numbers_of(token);
            finds.forget_client(token);
            for number in numbers {
                let stop = ClientMessage::StopFinding { id: number };
                let _ = self.send_upstream(at, &stop, Expect::Nothing);
            }
        }
    }
}
