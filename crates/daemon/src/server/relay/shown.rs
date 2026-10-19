//! Another machine's boards, reviews and chats' pages, shown through this
//! daemon (CLACKA-117, phase 3).
//!
//! Each is followed there once, however many clients here follow it, and
//! what the daemon there sends of it goes to each, with every number in it
//! renumbered as this daemon numbers what it shows of that machine: the
//! board or review, the chats that wrote or made what's in it, the buffers
//! open, and the agent hunks a review lists, which keeping or rejecting
//! names. A review's actions name no review, only hunks and chats, which
//! say which machine they're on. A chat's page sends a picture a few
//! times a second while watched: each client is sent only the latest it
//! hasn't been, once it has read the last, as a page of this daemon's
//! own is.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use clackatoo_protocol::{
    BoardAuthor, BoardEntry, BoardId, BoardLink, BoardSummary, BoardUpdate, BrowserUpdate,
    ClientMessage, Error, Event, Hunks, ReviewAction, ReviewId, ReviewList, TerminalId,
};
use mio::Token;

use super::editors::Out;
use super::mirror::{Fresh, Mirror, shown_name};
use super::unpack::{Need, Unpacked};
use super::{Counters, Expect, Renumbered, Route, Then};
use crate::server::{Daemon, Source};

/// One of another daemon's reviews, followed by clients of this one.
#[derive(Debug, Default)]
pub(in crate::server) struct FollowedReview {
    viewers: BTreeSet<Token>,
    /// The agent hunks it last listed, by the daemon there's numbers.
    pub(super) hunks: Vec<u64>,
}

impl Daemon {
    /// The connection whose workspaces show this daemon's board `local`,
    /// and its number there.
    pub(in crate::server) fn relayed_board(&self, local: BoardId) -> Option<(usize, BoardId)> {
        (self.links.list.iter().enumerate()).find_map(|(at, linked)| {
            let remote = linked.relayed.mirror.board_numbers.remote(local.0)?;
            Some((at, BoardId(remote)))
        })
    }

    /// As [`relayed_board`](Self::relayed_board), for a review.
    pub(in crate::server) fn relayed_review(&self, local: ReviewId) -> Option<(usize, ReviewId)> {
        (self.links.list.iter().enumerate()).find_map(|(at, linked)| {
            let remote = linked.relayed.mirror.review_numbers.remote(local.0)?;
            Some((at, ReviewId(remote)))
        })
    }

    /// Notes that the client at `token` follows this daemon's board
    /// `local`, of the connection at `at`, or no longer does, if not
    /// `follows`: returns whether the daemon there is to be told, as it
    /// starts or stops being followed here. A client attaching again is
    /// sent it whole again.
    pub(super) fn follow_board(
        &mut self,
        token: Token,
        at: usize,
        local: BoardId,
        follows: bool,
    ) -> bool {
        let boards = &mut self.links.list[at].relayed.boards;
        let viewers = boards.entry(local).or_default();
        if follows {
            viewers.insert(token);
            return true;
        }
        viewers.remove(&token);
        let last = viewers.is_empty();
        if last {
            boards.remove(&local);
        }
        last
    }

    /// As [`follow_board`](Self::follow_board), for a review.
    pub(super) fn follow_review(
        &mut self,
        token: Token,
        at: usize,
        local: ReviewId,
        follows: bool,
    ) -> bool {
        let reviews = &mut self.links.list[at].relayed.reviews;
        let followed = reviews.entry(local).or_default();
        if follows {
            followed.viewers.insert(token);
            return true;
        }
        followed.viewers.remove(&token);
        let last = followed.viewers.is_empty();
        if last {
            reviews.remove(&local);
            self.keep_relayed_hunks(at);
        }
        last
    }

    /// As [`follow_board`](Self::follow_board), for a chat's page.
    pub(super) fn follow_browser(
        &mut self,
        token: Token,
        at: usize,
        local: TerminalId,
        follows: bool,
    ) -> bool {
        let browsers = &mut self.links.list[at].relayed.browsers;
        let viewers = browsers.entry(local).or_default();
        if follows {
            viewers.insert(token);
            return true;
        }
        viewers.remove(&token);
        if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
            client.browser_frames.remove(&local);
        }
        let last = viewers.is_empty();
        if last {
            browsers.remove(&local);
        }
        last
    }

    /// `action`, a review's, renumbered for the daemon on the machine the
    /// hunks or chats it names are on, if they're another machine's, with
    /// that machine's connection: or why it can't be done.
    pub(super) fn relayed_review_action(
        &self,
        action: &ReviewAction,
    ) -> Option<(usize, Renumbered)> {
        let chat = |local: TerminalId| self.relayed_terminal(local);
        let renumbered = |action: ReviewAction| {
            Ok((clackatoo_protocol::Request::Review(action), Then::Reviewed))
        };
        let hunks = |hunks: &Hunks| -> Option<(usize, Result<Hunks, Error>)> {
            match hunks {
                Hunks::Ids(ids) => {
                    let at = (self.links.list.iter()).position(|linked| {
                        ids.first().is_some_and(|&id| {
                            linked.relayed.mirror.hunk_numbers.remote(id).is_some()
                        })
                    })?;
                    let numbers = &self.links.list[at].relayed.mirror.hunk_numbers;
                    let remote: Option<Vec<u64>> =
                        ids.iter().map(|&id| numbers.remote(id)).collect();
                    Some((
                        at,
                        remote.map(Hunks::Ids).ok_or_else(|| {
                            Error::Review("those changes aren't all on one machine".into())
                        }),
                    ))
                }
                &Hunks::Turn { chat: local, turn } => {
                    chat(local).map(|(at, chat)| (at, Ok(Hunks::Turn { chat, turn })))
                }
                &Hunks::Chat(local) => chat(local).map(|(at, chat)| (at, Ok(Hunks::Chat(chat)))),
            }
        };
        let (at, action) = match action {
            ReviewAction::Keep(which) => {
                let (at, which) = hunks(which)?;
                (at, which.map(ReviewAction::Keep))
            }
            ReviewAction::Reject {
                hunks: which,
                force,
            } => {
                let (at, which) = hunks(which)?;
                let force = *force;
                (at, which.map(|hunks| ReviewAction::Reject { hunks, force }))
            }
            ReviewAction::Comment {
                chat: local,
                place,
                text,
            } => {
                let (at, chat) = chat(*local)?;
                let mut place = place.clone();
                // A place in a buffer this daemon numbers, else by its line.
                place.at = place.at.and_then(|(buffer, version, offset)| {
                    let numbers = &self.links.list[at].relayed.mirror.buffer_numbers;
                    let remote = numbers.remote(buffer.0)?;
                    Some((clackatoo_protocol::BufferId(remote), version, offset))
                });
                let text = text.clone();
                (at, Ok(ReviewAction::Comment { chat, place, text }))
            }
            &ReviewAction::Uncomment { chat: local, id } => {
                let (at, chat) = chat(local)?;
                (at, Ok(ReviewAction::Uncomment { chat, id }))
            }
            &ReviewAction::Send(local) => {
                let (at, chat) = chat(local)?;
                (at, Ok(ReviewAction::Send(chat)))
            }
            &ReviewAction::Refresh(local) => {
                let (at, review) = self.relayed_review(local)?;
                (at, Ok(ReviewAction::Refresh(review)))
            }
        };
        let relays = self.links.list[at].relayed.relays;
        Some(match action {
            Ok(_) if !relays => (at, Err(self.too_old(at, "reviewing"))),
            Ok(action) => (at, renumbered(action)),
            Err(err) => (at, Err(err)),
        })
    }

    /// What's done with a frame from the daemon reached at `token`, whose
    /// first numbers say it's a change to one of its boards, reviews or
    /// chats' pages, the kind of event numbered `kind`, about its number
    /// `remote`, if it's followed here.
    pub(super) fn shown_route(
        &self,
        token: Token,
        kind: u32,
        remote: u32,
        update: Option<u32>,
    ) -> Option<(Route, Need)> {
        let (at, wire) = self.reached_at(token)?;
        let relayed = &self.links.list[at].relayed;
        let mirror = &relayed.mirror;
        match kind {
            BOARD => {
                let local = BoardId(mirror.board_numbers.local(remote)?);
                relayed
                    .boards
                    .contains_key(&local)
                    .then_some((Route::Board { local }, Need::Decode))
            }
            REVIEW => {
                let local = ReviewId(mirror.review_numbers.local(remote)?);
                relayed
                    .reviews
                    .contains_key(&local)
                    .then_some((Route::Review { local }, Need::Decode))
            }
            BROWSER => {
                let local = TerminalId(mirror.terminal_numbers.local(remote)?);
                let viewers = relayed.browsers.get(&local)?;
                // A picture's bytes are taken out; what the page is, is
                // passed on as it came.
                let need = if update == Some(FRAME) {
                    Need::Decode
                } else {
                    self.pass_on_to(viewers.iter().copied(), wire)
                };
                Some((Route::Browser { local }, need))
            }
            _ => None,
        }
    }

    /// Passes on a change to this daemon's board `local`, of the
    /// connection at `at`, renumbered, to each client following it.
    pub(super) fn relay_board(&mut self, at: usize, local: BoardId, update: BoardUpdate) {
        let (linked, mut fresh) = self.numbering(at);
        if !fresh.have_room() {
            self.out_of_numbers(at);
            return;
        }
        let mirror = &mut linked.relayed.mirror;
        let update = match update {
            BoardUpdate::Snapshot { summary, entries } => BoardUpdate::Snapshot {
                summary: board_summary(mirror, &mut fresh, summary),
                entries: board_entries(mirror, &mut fresh, entries),
            },
            BoardUpdate::Changed { summary, entries } => BoardUpdate::Changed {
                summary: board_summary(mirror, &mut fresh, summary),
                entries: board_entries(mirror, &mut fresh, entries),
            },
            BoardUpdate::Refused(why) => BoardUpdate::Refused(shown_name(&why)),
            BoardUpdate::Gone(why) => BoardUpdate::Gone(shown_name(&why)),
        };
        let viewers: Vec<Token> = (linked.relayed.boards.get(&local))
            .map(|viewers| viewers.iter().copied().collect())
            .unwrap_or_default();
        for token in viewers {
            let event = Event::Board {
                board: local,
                update: update.clone(),
            };
            self.send_event(token, event);
        }
    }

    /// Passes on a review's new list, of this daemon's review `local` of
    /// the connection at `at`, renumbered, to each client following it.
    pub(super) fn relay_review(&mut self, at: usize, local: ReviewId, mut list: ReviewList) {
        let (linked, mut fresh) = self.numbering(at);
        if !fresh.have_room() {
            self.out_of_numbers(at);
            return;
        }
        let mirror = &mut linked.relayed.mirror;
        let mut chat = |chat: &mut TerminalId| {
            chat.0 = mirror
                .terminal_numbers
                .local_or_fresh(chat.0, || fresh.terminal());
        };
        if let Some(of) = &mut list.chat {
            chat(of);
        }
        for named in &mut list.chats {
            chat(&mut named.chat);
        }
        let mut told = Vec::new();
        for file in &mut list.files {
            for hunk in &mut file.hunks {
                if let Some(by) = &mut hunk.by {
                    chat(&mut by.chat);
                }
            }
        }
        let mut hunks = 0;
        for file in &mut list.files {
            let room = super::mirror::MOST_HUNKS.saturating_sub(hunks);
            file.hunks.truncate(room);
            hunks += file.hunks.len();
            // A buffer this daemon doesn't number is none open, as far as
            // its clients go.
            file.buffer = (file.buffer)
                .and_then(|buffer| mirror.buffer_numbers.local(buffer.0))
                .map(clackatoo_protocol::BufferId);
            for hunk in &mut file.hunks {
                for id in &mut hunk.agents {
                    told.push(*id);
                    *id = mirror.hunk_numbers.local_or_fresh(*id, || fresh.hunk());
                }
            }
        }
        let viewers: Vec<Token> = match linked.relayed.reviews.get_mut(&local) {
            Some(followed) => {
                followed.hunks = told;
                followed.viewers.iter().copied().collect()
            }
            None => Vec::new(),
        };
        self.keep_relayed_hunks(at);
        for token in viewers {
            let event = Event::Review {
                review: local,
                list: list.clone(),
            };
            self.send_event(token, event);
        }
    }

    /// Passes on a change to this daemon's chat `local`'s page, of the
    /// connection at `at`, as `unpacked` holds it, to each client watching
    /// it: what the page is, as it came, and the latest picture of it,
    /// each client sent it once it has read the last.
    pub(super) fn relay_browser(&mut self, at: usize, local: TerminalId, unpacked: Unpacked) {
        let viewers: Vec<Token> = (self.links.list[at].relayed.browsers.get(&local))
            .map(|viewers| viewers.iter().copied().collect())
            .unwrap_or_default();
        match unpacked {
            Unpacked::Decoded(Ok(clackatoo_protocol::ServerMessage::Event(Event::Browser {
                update: BrowserUpdate::Frame(frame),
                ..
            }))) => {
                let jpeg = Arc::new(frame.jpeg);
                for token in viewers {
                    if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
                        client.browser_frames.insert(local, Arc::clone(&jpeg));
                        self.browsing.frames_unsent = true;
                    }
                }
                self.send_browser_frames();
            }
            Unpacked::Decoded(_) => {}
            unpacked @ Unpacked::PassedOn(_) => {
                for token in viewers {
                    if let Some(frame) =
                        self.frame_for(at, token, &unpacked, &[(2, u64::from(local.0))])
                    {
                        self.send_out(token, Out::Frame(frame));
                    }
                }
            }
        }
    }

    /// The connection at `at` has been made: the boards and chats' pages
    /// followed through it are attached there again. Reviews too.
    pub(super) fn shown_linked(&mut self, at: usize) {
        use clackatoo_protocol::Request;
        let relayed = &self.links.list[at].relayed;
        let mirror = &relayed.mirror;
        let mut asks = Vec::new();
        for local in relayed.boards.keys() {
            if let Some(remote) = mirror.board_numbers.remote(local.0) {
                asks.push(Request::AttachBoard {
                    board: BoardId(remote),
                });
            }
        }
        for local in relayed.reviews.keys() {
            if let Some(remote) = mirror.review_numbers.remote(local.0) {
                asks.push(Request::AttachReview {
                    review: ReviewId(remote),
                });
            }
        }
        for (local, size) in &relayed.browser_sizes {
            if relayed.browsers.contains_key(local)
                && let Some(remote) = mirror.terminal_numbers.remote(local.0)
            {
                asks.push(Request::AttachBrowser {
                    chat: TerminalId(remote),
                    width: size[0],
                    height: size[1],
                });
            }
        }
        for ask in asks {
            let _ = self.send_upstream(at, &ClientMessage::Request(ask), Expect::Nothing);
        }
    }

    /// Forgets the client at `token`, gone: it follows none of other
    /// daemons' boards, reviews or chats' pages any more.
    pub(super) fn forget_shown_client(&mut self, token: Token) {
        use clackatoo_protocol::Request;
        for at in 0..self.links.list.len() {
            let relayed = &self.links.list[at].relayed;
            let boards: Vec<BoardId> = (relayed.boards.iter())
                .filter(|(_, viewers)| viewers.contains(&token))
                .map(|(&local, _)| local)
                .collect();
            let reviews: Vec<ReviewId> = (relayed.reviews.iter())
                .filter(|(_, followed)| followed.viewers.contains(&token))
                .map(|(&local, _)| local)
                .collect();
            let browsers: Vec<TerminalId> = (relayed.browsers.iter())
                .filter(|(_, viewers)| viewers.contains(&token))
                .map(|(&local, _)| local)
                .collect();
            let mut asks = Vec::new();
            for local in boards {
                if self.follow_board(token, at, local, false)
                    && let Some((_, board)) = self.relayed_board(local)
                {
                    asks.push(Request::DetachBoard { board });
                }
            }
            for local in reviews {
                if self.follow_review(token, at, local, false)
                    && let Some((_, review)) = self.relayed_review(local)
                {
                    asks.push(Request::DetachReview { review });
                }
            }
            for local in browsers {
                if self.follow_browser(token, at, local, false)
                    && let Some((_, chat)) = self.relayed_terminal(local)
                {
                    asks.push(Request::DetachBrowser { chat });
                }
            }
            for ask in asks {
                let _ = self.send_upstream(at, &ClientMessage::Request(ask), Expect::Nothing);
            }
        }
    }
}

/// The number of `Event::Board` among the events.
pub(super) const BOARD: u32 = 10;

/// The number of `Event::Review` among the events.
pub(super) const REVIEW: u32 = 11;

/// The number of `Event::Browser` among the events.
pub(super) const BROWSER: u32 = 12;

/// The number of `BrowserUpdate::Frame` among a page's updates.
const FRAME: u32 = 1;

/// `summary`, of a board of another daemon's, renumbered by `mirror`, new
/// numbers from `fresh`, and made safe to show.
fn board_summary(
    mirror: &mut Mirror,
    fresh: &mut Counters<'_>,
    mut summary: BoardSummary,
) -> BoardSummary {
    summary.id.0 = mirror
        .board_numbers
        .local_or_fresh(summary.id.0, || fresh.board());
    summary.name = shown_name(&summary.name);
    summary
}

/// `entries`, of a board of another daemon's, with the chats they name
/// renumbered by `mirror`, new numbers from `fresh`.
fn board_entries(
    mirror: &mut Mirror,
    fresh: &mut Counters<'_>,
    mut entries: Vec<BoardEntry>,
) -> Vec<BoardEntry> {
    let mut author = |by: &mut BoardAuthor| {
        if let BoardAuthor::Chat { chat, .. } = by {
            chat.0 = mirror
                .terminal_numbers
                .local_or_fresh(chat.0, || fresh.terminal());
        }
    };
    for entry in &mut entries {
        author(&mut entry.by);
        if let Some((_, by)) = &mut entry.released {
            author(by);
        }
        for (_, by, _) in &mut entry.edits {
            author(by);
        }
    }
    for link in entries.iter_mut().flat_map(|entry| &mut entry.links) {
        if let BoardLink::Turn { chat, .. } = link {
            chat.0 = mirror
                .terminal_numbers
                .local_or_fresh(chat.0, || fresh.terminal());
        }
    }
    entries
}

/// What's kept of each client following another daemon's things, by this
/// daemon's numbers.
pub(in crate::server) type Viewers<T> = BTreeMap<T, BTreeSet<Token>>;

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use clackatoo_protocol::{
        BrowserFrame, BrowserStatus, Content, Layout, Made, Pane, PaneId, Request, Response,
        ReviewFile, ReviewHunk, ServerMessage, Tab, TabId, Workspace, WorkspaceId, Workspaces,
    };

    use super::super::Routed;
    use super::super::tests::{client, desk, heard, linked, read, say};
    use super::*;
    use crate::server::Reading;

    /// A daemon connected to desk, as [`linked`] makes it, which shows a
    /// workspace with desk's review 3 and board 4 beside its terminal 7,
    /// a chat: this daemon's numbers for the review, the board and the
    /// chat.
    fn with_review(
        dir: &std::path::Path,
    ) -> (Daemon, Token, UnixStream, (ReviewId, BoardId, TerminalId)) {
        let (mut daemon, link, mut there) = linked(dir);
        let pane = |id, content| {
            Layout::Pane(Pane {
                id: PaneId(id),
                content,
                home: None,
                key: None,
            })
        };
        let workspace = Workspace {
            id: WorkspaceId(1),
            name: "work".into(),
            tabs: vec![Tab {
                id: TabId(1),
                layout: Layout::Split {
                    axis: clackatoo_protocol::Axis::LeftToRight,
                    parts: vec![
                        pane(1, Content::Terminal(TerminalId(7))),
                        pane(2, Content::Review(ReviewId(3))),
                        pane(3, Content::Board(BoardId(4))),
                    ],
                    shares: vec![1, 1, 1],
                },
                focused: PaneId(1),
                name: None,
            }],
            active_tab: TabId(1),
            home: None,
            lent: None,
        };
        say(
            &mut there,
            &ServerMessage::Event(Event::Workspaces(Workspaces {
                list: vec![workspace],
                machine: Some(desk()),
                ..Workspaces::default()
            })),
        );
        daemon.read_reached(link, Reading::Look);
        heard(&mut there);
        let mirror = &daemon.links.list[0].relayed.mirror;
        let review = ReviewId(mirror.review_numbers.local(3).unwrap());
        let board = BoardId(mirror.board_numbers.local(4).unwrap());
        let chat = TerminalId(mirror.terminal_numbers.local(7).unwrap());
        (daemon, link, there, (review, board, chat))
    }

    /// The events `stream` has been sent.
    fn events(stream: &mut UnixStream) -> Vec<Event> {
        (read::<ServerMessage>(stream).into_iter())
            .filter_map(|message| match message {
                ServerMessage::Event(event) => Some(event),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_reviews_list_is_renumbered_and_keeping_its_hunks_goes_there() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, (review, _, chat)) = with_review(dir.path());
        let (window, mut to_window) = client(&mut daemon);
        let attach = Request::AttachReview { review };
        assert!(matches!(
            daemon.route_request(window, attach),
            Routed::Waiting
        ));
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Request(Request::AttachReview {
                review: ReviewId(3)
            })]
        );
        let list = ReviewList {
            chat: Some(TerminalId(7)),
            chat_name: "fix it".into(),
            files: vec![ReviewFile {
                path: "/src/main.rs".into(),
                shown: "src/main.rs".into(),
                // Open there, in a buffer no pane here shows.
                buffer: Some(clackatoo_protocol::BufferId(5)),
                hunks: vec![ReviewHunk {
                    line: 1,
                    before: Vec::new(),
                    base: "a\n".into(),
                    text: "b\n".into(),
                    after: Vec::new(),
                    agents: vec![100],
                    by: Some(Made {
                        chat: TerminalId(7),
                        turn: 0,
                    }),
                    reviewed: false,
                }],
            }],
            ..ReviewList::default()
        };
        say(&mut there, &ServerMessage::Response(Response::Done));
        say(
            &mut there,
            &ServerMessage::Event(Event::Review {
                review: ReviewId(3),
                list,
            }),
        );
        daemon.read_reached(link, Reading::Look);
        let [
            Event::Review {
                review: shown,
                list,
            },
        ] = &events(&mut to_window)[..]
        else {
            panic!("no list");
        };
        assert_eq!(*shown, review);
        assert_eq!(list.chat, Some(chat), "the chat as numbered here");
        let file = &list.files[0];
        assert_eq!(file.buffer, None, "no buffer numbered here");
        let hunk = &file.hunks[0];
        assert_eq!(hunk.by.map(|by| by.chat), Some(chat));
        let [kept] = hunk.agents[..] else {
            panic!("{:?}", hunk.agents);
        };
        assert_ne!(kept, 100, "numbered here");

        // Kept here, it's kept there, under its number there.
        let keep = Request::Review(ReviewAction::Keep(Hunks::Ids(vec![kept])));
        assert!(matches!(
            daemon.route_request(window, keep),
            Routed::Waiting
        ));
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Request(Request::Review(ReviewAction::Keep(
                Hunks::Ids(vec![100])
            )))]
        );
        say(
            &mut there,
            &ServerMessage::Response(Response::Reviewed("Kept 1 change".into())),
        );
        daemon.read_reached(link, Reading::Look);
        let answered: Vec<Response> = (read::<ServerMessage>(&mut to_window).into_iter())
            .filter_map(|message| match message {
                ServerMessage::Response(response) => Some(response),
                _ => None,
            })
            .collect();
        assert_eq!(answered, [Response::Reviewed("Kept 1 change".into())]);
        // A hunk numbered nowhere is this daemon's own.
        let own = Request::Review(ReviewAction::Keep(Hunks::Ids(vec![kept + 1000])));
        assert!(matches!(daemon.route_request(window, own), Routed::Here(_)));
    }

    #[test]
    fn a_boards_chats_and_number_are_this_daemons() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, (_, board, chat)) = with_review(dir.path());
        let (window, mut to_window) = client(&mut daemon);
        daemon.route_request(window, Request::AttachBoard { board });
        heard(&mut there);
        let summary = BoardSummary {
            id: BoardId(4),
            name: "plan".into(),
            purpose: String::new(),
            scope: clackatoo_protocol::BoardScope::Global,
            archived: false,
            created: 0,
            entries: 1,
        };
        let entry = BoardEntry {
            id: 1,
            kind: clackatoo_protocol::PostKind::Note,
            by: BoardAuthor::Chat {
                chat: TerminalId(7),
                agent: "claude".into(),
                title: "fix it".into(),
            },
            at: 0,
            text: "done".into(),
            links: vec![BoardLink::Turn {
                chat: TerminalId(7),
                turn: 2,
            }],
            subject: None,
            expires: None,
            released: None,
            edits: Vec::new(),
        };
        say(&mut there, &ServerMessage::Response(Response::Done));
        say(
            &mut there,
            &ServerMessage::Event(Event::Board {
                board: BoardId(4),
                update: BoardUpdate::Snapshot {
                    summary,
                    entries: vec![entry],
                },
            }),
        );
        daemon.read_reached(link, Reading::Look);
        let [
            Event::Board {
                board: shown,
                update: BoardUpdate::Snapshot { summary, entries },
            },
        ] = &events(&mut to_window)[..]
        else {
            panic!("no board");
        };
        assert_eq!((*shown, summary.id), (board, board));
        assert!(matches!(&entries[0].by, BoardAuthor::Chat { chat: by, .. } if *by == chat));
        assert!(matches!(entries[0].links[0], BoardLink::Turn { chat: by, .. } if by == chat));
    }

    #[test]
    fn a_chats_page_is_watched_through_this_daemon_its_pictures_the_latest_only() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there, (_, _, chat)) = with_review(dir.path());
        let (window, mut to_window) = client(&mut daemon);
        let attach = Request::AttachBrowser {
            chat,
            width: 800,
            height: 600,
        };
        daemon.route_request(window, attach);
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Request(Request::AttachBrowser {
                chat: TerminalId(7),
                width: 800,
                height: 600
            })]
        );
        let browser = |update| {
            ServerMessage::Event(Event::Browser {
                chat: TerminalId(7),
                update,
            })
        };
        let status = BrowserStatus {
            url: "https://example.com".into(),
            ..BrowserStatus::default()
        };
        let picture = |byte| {
            BrowserUpdate::Frame(BrowserFrame {
                jpeg: vec![byte; 8],
            })
        };
        say(&mut there, &ServerMessage::Response(Response::Done));
        say(&mut there, &browser(BrowserUpdate::Status(status.clone())));
        say(&mut there, &browser(picture(1)));
        say(&mut there, &browser(picture(2)));
        daemon.read_reached(link, Reading::Look);
        daemon.send_browser_frames();
        let shown: Vec<(TerminalId, BrowserUpdate)> = (events(&mut to_window).into_iter())
            .filter_map(|event| match event {
                Event::Browser { chat, update } => Some((chat, update)),
                _ => None,
            })
            .collect();
        assert_eq!(shown[0], (chat, BrowserUpdate::Status(status)));
        assert!(
            shown[1..]
                .iter()
                .all(|(by, update)| *by == chat && matches!(update, BrowserUpdate::Frame(_))),
            "{shown:?}"
        );
        assert_eq!(
            shown.last().map(|(_, update)| update.clone()),
            Some(picture(2)),
            "the latest"
        );
        // Let go of, it's let go of there.
        daemon.route_request(window, Request::DetachBrowser { chat });
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Request(Request::DetachBrowser {
                chat: TerminalId(7)
            })]
        );
    }
}
