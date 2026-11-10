//! Lines another machine's program marked private, masked here for each
//! client following its terminal, as this daemon masks its own
//! (CLACKA-242; see `server/masks.rs`).
//!
//! A daemon of protocol 153 or later ([`wire::RELAY_HINTS`]) sends this
//! one, a machine of the same user's, a terminal's lines as they are, with
//! their hints ([`Event::HintedView`]), whenever a change to its view has
//! any. Each client following it here is sent it masked as it would be
//! were the terminal this daemon's own ([`hinted_frame`]): a window has
//! what's hidden masked, a phone, a window an agent sees and a client not
//! showing the terminal what's kept from agents too, and a daemon relaying
//! for this one is passed the hints on, if it masks lines itself, or sent
//! every line with a hint masked. What a client asks of the lines that
//! this daemon can't mask itself, a copy, a search, history, a capture or
//! an image, is asked there masked as that client has them
//! ([`Request::Masked`]).
//!
//! A window here asks to see a run of hidden lines here: the daemon there
//! says which lines they are ([`Response::HiddenRun`]), and they're shown
//! to that window alone, as they are, from the next snapshot, which
//! following the terminal there again brings; no other client here sees
//! them, nor do the daemons relaying for this one. They're masked again as
//! a window's own are.
//!
//! The daemon there keeps whatever's never to be written to disk: nothing
//! of another machine's terminal is kept on this one's.
//!
//! [`Response::HiddenRun`]: clackatoo_protocol::Response::HiddenRun

use std::time::Instant;

use clackatoo_protocol::{
    ClientMessage, Error, Event, LineHints, Masking, Request, Response, ServerMessage, TerminalId,
    ViewUpdate, frame, wire,
};
use clackatoo_term::Hints;
use mio::Token;

use super::{Daemon, Expect, Routed, Then, Viewer, local_error};
use crate::server::masks::show_run;

/// The most lines shown as they are at once, as for this daemon's own
/// terminals.
const MOST_SHOWN: u64 = 10_000;

/// `update`, a change to this daemon's terminal `local`'s view whose
/// lines `hints` are the hints of, as `viewer`, which speaks `wire` and is
/// a daemon relaying if `relays`, is to be sent it, encoded: with the
/// hints, to a daemon relaying that masks lines itself, and otherwise
/// masked as `viewer` has them. `newer`: from a daemon whose lines a
/// client older than protocol 147 has to have made again.
pub(super) fn hinted_frame(
    viewer: &mut Viewer,
    (wire, relays): (u32, bool),
    local: TerminalId,
    update: &ViewUpdate,
    hints: &LineHints,
    newer: bool,
) -> Option<Vec<u8>> {
    let mut update = viewer.history_update(update);
    let event = if relays && wire >= wire::RELAY_HINTS {
        Event::HintedView {
            terminal: local,
            update,
            hints: hints.clone(),
        }
    } else {
        hints.mask(&mut update, &viewer.masking);
        if newer && wire < wire::TERMINAL_STANDARDS_2 {
            update.lay_out_as_before();
        }
        Event::View {
            terminal: local,
            update,
        }
    };
    wire::keeping_pixels(relays, || frame::encode(wire, &ServerMessage::Event(event))).ok()
}

impl Daemon {
    /// The hints that mask a line of another machine's terminal for the
    /// client at `token`, following it if `follows`: as for this daemon's
    /// own, and what's kept from agents for a phone too, as before it was
    /// sent lines as they are.
    pub(in crate::server) fn relayed_hidden(&self, token: Token, follows: bool) -> Hints {
        let mut hidden = self.hidden_for(token, follows);
        if self.config.hints.noagent && self.is_web_device(token) {
            hidden |= Hints::NOAGENT;
        }
        hidden
    }

    /// How the lines of this daemon's terminal `local`, of the connection
    /// at `at`, are masked for the client at `token`: as a daemon relaying
    /// asked for one of its own, or as the client's view of it is, or else
    /// as for one agents may read through.
    fn relayed_masking(&self, token: Token, at: usize, local: TerminalId) -> Masking {
        if let Some(masking) = &self.links.masked_as {
            return masking.clone();
        }
        let viewer = (self.links.list[at].relayed.followed.get(&local))
            .and_then(|followed| followed.viewers.iter().find(|viewer| viewer.token == token));
        match viewer {
            Some(viewer) => Masking {
                hidden: self.relayed_hidden(token, true),
                ..viewer.masking.clone()
            },
            None => Masking {
                hidden: self.relayed_hidden(token, false),
                ..Masking::NONE
            },
        }
    }

    /// `request`, about the lines of this daemon's terminal `local`, of
    /// the connection at `at`, as it's asked of the daemon there for the
    /// client at `token`: masked as that client has them, if the daemon
    /// there sends this one lines as they are. An older one masks them
    /// as for a daemon relaying, whatever's asked.
    pub(super) fn masked_there(
        &self,
        token: Token,
        at: usize,
        local: TerminalId,
        request: Request,
    ) -> Request {
        if self.link_wire(at) < wire::RELAY_HINTS {
            return request;
        }
        Request::Masked {
            masking: self.relayed_masking(token, at, local),
            request: Box::new(request),
        }
    }

    /// Answers [`Request::Unmask`] from the client at `token` about this
    /// daemon's terminal `local`, `remote` on the connection at `at`: a
    /// window here is shown the run of hidden lines around `line` here,
    /// once the daemon there says which they are, or has them masked
    /// again at once; a daemon relaying that masks lines itself has it
    /// asked there for it.
    pub(super) fn relay_unmask(
        &mut self,
        token: Token,
        (at, local, remote): (usize, TerminalId, TerminalId),
        line: Option<u64>,
    ) -> Routed {
        let refused = |why: String| Routed::Answered(Response::Error(Error::Other(why)));
        if let Err(err) = self.refuse_unshared(token, at) {
            return Routed::Answered(Response::Error(err));
        }
        if self.is_web_device(token) {
            return refused("a device on the web app can't show what's hidden".into());
        }
        if self.relays(token) && !self.masks_relayed(token) {
            return refused(
                "what's hidden is shown only in a window on its own machine, or one reaching it \
                 with --remote, or a newer one through :connect"
                    .into(),
            );
        }
        if self.link_wire(at) < wire::RELAY_HINTS {
            let host = &self.links.list[at].remote.host;
            return refused(format!(
                "{host}'s clackatoo is too old to show what's hidden through this machine: \
                 update it there"
            ));
        }
        let unmask = Request::Unmask {
            terminal: remote,
            line,
        };
        if self.relays(token) {
            return self.wait_for(token, at, unmask, Then::Pass);
        }
        let shown_for = self.config.hints.shown_for;
        let Some(viewer) = self.relayed_viewer(at, local, token) else {
            return refused(format!("terminal {local} isn't shown here"));
        };
        if line.is_some() {
            return self.wait_for(token, at, unmask, Then::Unmasked(local));
        }
        // Masked again at once, here.
        let said = show_run(
            &mut viewer.masking,
            &mut viewer.shown_until,
            None,
            Hints::empty(),
            shown_for,
        );
        match said {
            Ok(said) => {
                self.follow_afresh(at, local, token);
                Routed::Answered(Response::Text(said))
            }
            Err(err) => Routed::Answered(Response::Error(err)),
        }
    }

    /// The answer for the client at `token`, which asked to see hidden
    /// lines of this daemon's terminal `local`, the daemon of the
    /// connection at `at` answering with `response`: the lines it says are
    /// shown to that client as they are, from a snapshot.
    pub(super) fn unmasked_answer(
        &mut self,
        at: usize,
        token: Token,
        local: TerminalId,
        response: Response,
    ) -> Response {
        let host = self.links.list[at].remote.host.clone();
        let (lines, prints, hints) = match response {
            Response::HiddenRun {
                lines,
                prints,
                hints,
            } if (lines.end).checked_sub(lines.start) == Some(prints.len() as u64)
                && prints.len() as u64 <= MOST_SHOWN =>
            {
                (lines, prints, hints)
            }
            Response::Error(err) => {
                let mirror = &self.links.list[at].relayed.mirror;
                return Response::Error(local_error(mirror, &host, err));
            }
            response => {
                log::warn!("{host} answered with what wasn't asked: {response:?}");
                return Response::Error(Error::Other(format!(
                    "{host} answered with what wasn't asked"
                )));
            }
        };
        let shown_for = self.config.hints.shown_for;
        let Some(viewer) = self.relayed_viewer(at, local, token) else {
            return Response::Error(Error::Other(format!(
                "terminal {local} isn't shown here"
            )));
        };
        let run = Some((lines, prints));
        match show_run(&mut viewer.masking, &mut viewer.shown_until, run, hints, shown_for) {
            Ok(said) => {
                self.follow_afresh(at, local, token);
                Response::Text(said)
            }
            Err(err) => Response::Error(err),
        }
    }

    /// The client at `token` following this daemon's terminal `local`, of
    /// the connection at `at`, if it does.
    fn relayed_viewer(&mut self, at: usize, local: TerminalId, token: Token) -> Option<&mut Viewer> {
        let followed = self.links.list.get_mut(at)?.relayed.followed.get_mut(&local)?;
        followed.viewers.iter_mut().find(|viewer| viewer.token == token)
    }

    /// Has the client at `token` sent a snapshot of this daemon's terminal
    /// `local`, of the connection at `at`, before anything else, as its
    /// lines are masked otherwise now: following it there again sends one
    /// to every client here.
    fn follow_afresh(&mut self, at: usize, local: TerminalId, token: Token) {
        let Some(viewer) = self.relayed_viewer(at, local, token) else {
            return;
        };
        viewer.snapshot_due = true;
        let upstream = (self.links.list[at].relayed.followed.get(&local))
            .is_some_and(|followed| followed.upstream);
        if upstream
            && self.is_linked(at)
            && let Some(remote) = (self.links.list[at].relayed.mirror.terminal_numbers).remote(local.0)
        {
            let attach = ClientMessage::Request(self.follow_request(at, local, TerminalId(remote)));
            let _ = self.send_upstream(at, &attach, Expect::Nothing);
        }
    }

    /// Masks what each client following another machine's terminal is
    /// sent by what agents may read through it now: what changed is sent
    /// afresh, and what was shown is masked again.
    pub(in crate::server) fn mask_relayed_by_grants(&mut self) {
        let mut changed = Vec::new();
        for (at, linked) in self.links.list.iter().enumerate() {
            for (&local, followed) in &linked.relayed.followed {
                for viewer in &followed.viewers {
                    let hidden = self.relayed_hidden(viewer.token, true);
                    if viewer.masking.hidden != hidden {
                        changed.push((at, local, viewer.token, hidden));
                    }
                }
            }
        }
        for (at, local, token, hidden) in changed {
            if let Some(viewer) = self.relayed_viewer(at, local, token) {
                viewer.masking = Masking {
                    hidden,
                    ..Masking::NONE
                };
                viewer.shown_until = None;
                self.follow_afresh(at, local, token);
            }
        }
    }

    /// When lines of another machine's terminal shown as they are to a
    /// client here are next masked again.
    pub(in crate::server) fn next_relayed_mask_due(&self) -> Option<Instant> {
        (self.links.list.iter())
            .flat_map(|linked| linked.relayed.followed.values())
            .flat_map(|followed| &followed.viewers)
            .filter_map(|viewer| viewer.shown_until)
            .min()
    }

    /// Masks again the lines of other machines' terminals shown as they
    /// are whose time is up, as of `now`.
    pub(in crate::server) fn mask_relayed_again_if_due(&mut self, now: Instant) {
        let mut due = Vec::new();
        for (at, linked) in self.links.list.iter().enumerate() {
            for (&local, followed) in &linked.relayed.followed {
                due.extend(
                    (followed.viewers.iter())
                        .filter(|viewer| viewer.shown_until.is_some_and(|until| until <= now))
                        .map(|viewer| (at, local, viewer.token)),
                );
            }
        }
        for (at, local, token) in due {
            if let Some(viewer) = self.relayed_viewer(at, local, token) {
                viewer.masking.shown = 0..0;
                viewer.masking.prints.clear();
                viewer.shown_until = None;
                self.follow_afresh(at, local, token);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use clackatoo_protocol::{
        ClientMessage, Event, Masking, NativeAccess, Position, Request, Response, ServerMessage,
        TerminalId, View, ViewUpdate, wire,
    };
    use clackatoo_term::hints::MASK;
    use clackatoo_term::{Config, Hints, Size, Terminal};
    use mio::Token;

    use super::super::Routed;
    use super::super::tests::{answers, client, heard, linked, read, say, terminal};
    use crate::server::Daemon;
    use crate::server::Reading;

    /// The terminal the daemon there numbers 7.
    const REMOTE: TerminalId = TerminalId(7);

    /// The daemon there's terminal: a line hidden, then one kept from
    /// agents, as a program marked them.
    fn remote_terminal() -> Terminal {
        let mut terminal = Terminal::new(Size::new(20, 5).unwrap(), &Config::default());
        terminal.advance(
            b"one\r\n\x1b]46373;region;start;h;hide\x07hunter2\x1b]46373;region;end;h\x07\r\n\
              two\r\n\x1b]46373;region;start;p;private\x07token\x1b]46373;region;end;p\x07\r\n",
        );
        terminal
    }

    /// The daemon there sends a snapshot of its terminal, as one of
    /// protocol 153 or later sends a daemon relaying: its lines as they
    /// are, with their hints.
    fn send_snapshot(daemon: &mut Daemon, link: Token, there: &mut UnixStream) {
        let terminal = remote_terminal();
        let view = View::capture_masked(&terminal, wire::RELAY_HINTS, &Masking::NONE);
        let pictures = view.grid().unwrap().pictures.clone();
        let rows = (0..).zip(terminal.grid().rows());
        let hints = crate::server::masks::hints_of(rows, &terminal, &pictures).unwrap();
        say(
            there,
            &ServerMessage::Event(Event::HintedView {
                terminal: REMOTE,
                update: ViewUpdate::Snapshot(view),
                hints,
            }),
        );
        daemon.read_reached(link, Reading::Look);
    }

    /// Has each client at `tokens` follow this daemon's terminal `local`,
    /// the daemon there answering what it's asked.
    fn attach(
        daemon: &mut Daemon,
        (link, there): (Token, &mut UnixStream),
        tokens: &[Token],
        local: TerminalId,
    ) {
        for &token in tokens {
            daemon.route_request(token, Request::Attach { terminal: local });
        }
        let asked = (heard(there).into_iter())
            .filter(|message| matches!(message, ClientMessage::Request(_)))
            .count();
        for _ in 0..asked {
            say(there, &ServerMessage::Response(Response::Done));
        }
        daemon.read_reached(link, Reading::Look);
    }

    /// The first lines of `view`.
    fn lines_of(view: &View) -> Vec<String> {
        (view.grid().unwrap().lines.iter())
            .map(clackatoo_protocol::Line::text)
            .take(4)
            .collect()
    }

    /// The first lines of the last snapshot `stream` was sent.
    fn shown(stream: &mut UnixStream) -> Vec<String> {
        let snapshots: Vec<View> = (read::<ServerMessage>(stream).into_iter())
            .filter_map(|message| match message {
                ServerMessage::Event(Event::View {
                    update: ViewUpdate::Snapshot(view),
                    ..
                }) => Some(view),
                _ => None,
            })
            .collect();
        lines_of(snapshots.last().expect("no snapshot"))
    }

    /// A copy of `local`'s first lines.
    fn copy(local: TerminalId) -> Request {
        Request::Text {
            terminal: local,
            from: Position { line: 0, col: 0 },
            to: Position { line: 3, col: 9 },
            whole_lines: true,
        }
    }

    /// How the daemon there is asked to mask what it was asked last.
    fn masked_as(there: &mut UnixStream) -> Masking {
        match heard(there).pop() {
            Some(ClientMessage::Request(Request::Masked { masking, .. })) => masking,
            other => panic!("not asked masked: {other:?}"),
        }
    }

    /// A window here following a terminal on another machine sees what's
    /// kept from agents, as one there would; a phone, a window an agent
    /// sees and a client not showing it have it masked; and what they ask
    /// of its lines is asked masked as each has them.
    #[test]
    fn each_client_here_has_another_machines_lines_masked_as_its_own_would_be() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        let (window, mut to_window) = client(&mut daemon);
        let (granted, mut to_granted) = client(&mut daemon);
        let (phone, _to_phone) = crate::server::tests::web_reader(&mut daemon);
        let local = terminal(&daemon);
        attach(&mut daemon, (link, &mut there), &[window, granted, phone], local);
        let chat = TerminalId(4242);
        (daemon.tools.native).insert(chat, (granted, NativeAccess::Control));
        daemon.mask_by_grants();
        heard(&mut there);
        send_snapshot(&mut daemon, link, &mut there);
        assert_eq!(shown(&mut to_window), ["one", MASK, "two", "token"]);
        assert_eq!(shown(&mut to_granted), ["one", MASK, "two", MASK]);
        // What they copy is asked masked as each has it.
        daemon.route_request(window, copy(local));
        assert_eq!(masked_as(&mut there).hidden, Hints::HIDE);
        for token in [granted, phone] {
            daemon.route_request(token, copy(local));
            assert_eq!(masked_as(&mut there).hidden, Hints::HIDE | Hints::NOAGENT);
        }
        // So is a capture by a client not showing it, as an agent's
        // command could ask.
        let (capture, _to_capture) = client(&mut daemon);
        daemon.route_request(capture, Request::Capture { terminal: local });
        assert_eq!(masked_as(&mut there).hidden, Hints::HIDE | Hints::NOAGENT);
    }

    /// `zv` in a window here shows it the run of hidden lines, once the
    /// daemon there says which they are: that window alone, from the next
    /// snapshot, and what it copies; until it's masked again.
    #[test]
    fn a_window_here_is_shown_hidden_lines_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        let (window, mut to_window) = client(&mut daemon);
        let (other, mut to_other) = client(&mut daemon);
        let local = terminal(&daemon);
        attach(&mut daemon, (link, &mut there), &[window, other], local);
        send_snapshot(&mut daemon, link, &mut there);
        assert_eq!(shown(&mut to_window)[1], MASK);
        // Asked which lines they are, there.
        let unmask = |line| Request::Unmask {
            terminal: local,
            line,
        };
        daemon.route_request(window, unmask(Some(1)));
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Request(Request::Unmask {
                terminal: REMOTE,
                line: Some(1),
            })]
        );
        let print = Masking::fingerprint(remote_terminal().grid().row(1));
        say(
            &mut there,
            &ServerMessage::Response(Response::HiddenRun {
                lines: 1..2,
                prints: vec![print],
                hints: Hints::HIDE,
            }),
        );
        daemon.read_reached(link, Reading::Look);
        assert_eq!(
            answers(&mut to_window),
            [Response::Text("1 hidden line shown for 30s".into())]
        );
        // Followed afresh there, for a snapshot.
        let attach = ClientMessage::Request(Request::Attach { terminal: REMOTE });
        assert!(heard(&mut there).contains(&attach));
        send_snapshot(&mut daemon, link, &mut there);
        assert_eq!(shown(&mut to_window)[1], "hunter2");
        assert_eq!(shown(&mut to_other)[1], MASK);
        // What it copies is asked shown so.
        daemon.route_request(window, copy(local));
        let masking = masked_as(&mut there);
        assert_eq!((masking.shown, masking.prints), (1..2, vec![print]));
        daemon.route_request(other, copy(local));
        assert_eq!(masked_as(&mut there).shown, 0..0);
        // Masked again once the time's up.
        assert!(daemon.next_mask_due().is_some());
        let later = std::time::Instant::now() + std::time::Duration::from_secs(31);
        daemon.mask_relayed_again_if_due(later);
        assert_eq!(daemon.next_mask_due(), None);
        assert!(heard(&mut there).contains(&attach));
        send_snapshot(&mut daemon, link, &mut there);
        assert_eq!(shown(&mut to_window)[1], MASK);
        // A phone can't ask.
        let (phone, _to_phone) = crate::server::tests::web_reader(&mut daemon);
        daemon.route_request(phone, Request::Attach { terminal: local });
        let asked = daemon.route_request(phone, unmask(Some(1)));
        assert!(
            matches!(asked, Routed::Answered(Response::Error(_))),
            "{asked:?}"
        );
    }

    /// Through a daemon there older than 153, which masks every line with
    /// a hint for this one, nothing's shown, and nothing's asked masked.
    #[test]
    fn through_an_older_daemon_nothing_is_shown() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        if let Some(crate::server::Source::Reached(reached)) = daemon.sources.get_mut(link.0) {
            reached.wire = wire::SECRET_REGIONS;
        }
        let (window, _to_window) = client(&mut daemon);
        let local = terminal(&daemon);
        attach(&mut daemon, (link, &mut there), &[window], local);
        let unmask = Request::Unmask {
            terminal: local,
            line: Some(1),
        };
        let asked = daemon.route_request(window, unmask);
        assert!(
            matches!(asked, Routed::Answered(Response::Error(_))),
            "{asked:?}"
        );
        daemon.route_request(window, copy(local));
        assert!(matches!(
            &heard(&mut there)[..],
            [ClientMessage::Request(Request::Text { .. })]
        ));
    }

    /// A daemon relaying for this one that masks lines itself is passed
    /// them as they are, with their hints; an older one is sent them
    /// masked.
    #[test]
    fn a_daemon_relaying_for_this_one_is_passed_the_hints_on() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, link, mut there) = linked(dir.path());
        let (newer, mut to_newer) = client(&mut daemon);
        let (older, mut to_older) = client(&mut daemon);
        let local = terminal(&daemon);
        for (token, wire) in [(newer, wire::RELAY_HINTS), (older, wire::SECRET_REGIONS)] {
            if let Some(crate::server::Source::Client(client)) = daemon.sources.get_mut(token.0) {
                client.wire = wire;
            }
            assert_eq!(daemon.respond(Request::Relay, token), Response::Done);
        }
        attach(&mut daemon, (link, &mut there), &[newer, older], local);
        send_snapshot(&mut daemon, link, &mut there);
        let hinted = (read::<ServerMessage>(&mut to_newer).into_iter()).find_map(|message| {
            match message {
                ServerMessage::Event(Event::HintedView {
                    terminal, hints, ..
                }) => Some((terminal, hints.lines.len())),
                _ => None,
            }
        });
        assert_eq!(hinted, Some((local, 2)));
        let mut decoder = clackatoo_protocol::frame::FrameDecoder::new();
        let mut buffer = vec![0; 65536];
        let n = std::io::Read::read(&mut to_older, &mut buffer).unwrap();
        decoder.push(&buffer[..n]);
        let masked = std::iter::from_fn(|| {
            (decoder.decode::<ServerMessage>(wire::SECRET_REGIONS)).unwrap()
        })
        .find_map(|message| match message {
            ServerMessage::Event(Event::View {
                update: ViewUpdate::Snapshot(view),
                ..
            }) => Some(lines_of(&view)),
            _ => None,
        });
        assert_eq!(masked.expect("no snapshot"), ["one", MASK, "two", MASK]);
        // What it asks for one of its own is asked so there.
        let asked = Request::Masked {
            masking: Masking {
                hidden: Hints::HIDE,
                ..Masking::NONE
            },
            request: Box::new(copy(local)),
        };
        daemon.route_request(newer, asked);
        assert_eq!(masked_as(&mut there).hidden, Hints::HIDE);
        // And its window's ask to see hidden lines is asked there.
        let unmask = Request::Unmask {
            terminal: local,
            line: Some(1),
        };
        daemon.route_request(newer, unmask);
        assert_eq!(
            heard(&mut there),
            [ClientMessage::Request(Request::Unmask {
                terminal: REMOTE,
                line: Some(1),
            })]
        );
    }
}
