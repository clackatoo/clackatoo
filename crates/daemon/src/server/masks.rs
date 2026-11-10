//! Lines a program marked to be hidden (`OSC 46373`, see
//! [`clackatoo_term::regions`]), masked in everything a client is sent of
//! a terminal: its view, its history, the text it copies, and where its
//! searches go. So every client, whatever its protocol, a window on
//! another machine, a daemon relaying and the phone alike, has only the
//! mask, and draws, copies and searches that (see
//! [`clackatoo_protocol::Masking`]).
//!
//! Lines kept from agents are masked too for a client agents may read
//! through: a window that granted a chat native access to itself, which
//! sees what it draws, and one that doesn't show the terminal, as
//! `clackatoo capture`, which an agent's command could run (see
//! [`HintRules::noagent`]).
//!
//! The user can see a run of hidden lines as it is, in one window, for a
//! while ([`Request::Unmask`]): it's sent again as it is, in a fresh
//! snapshot, and copies and searches see it, until it's masked again, by
//! the same ask, once [`HintRules::shown_for`] has gone by, or line by
//! line as anything else is printed there. Only a window that reaches
//! this daemon itself can ask: not a web device, nor one through a daemon
//! relaying, which would show the lines to every client it has, phones
//! too.
//!
//! # Through a daemon relaying
//!
//! A daemon relaying (`:connect`) of protocol 153 or later
//! ([`wire::RELAY_HINTS`]) is one of the user's own, so it's sent lines as
//! they are, with their hints ([`Event::HintedView`]), and masks them for
//! each of its own clients as this daemon masks its own: what its windows
//! may see, its phones, agents' commands and windows agents see may not,
//! and the daemons relaying for it in turn get the hints. It asks for the
//! lines it can't mask itself, copies, searches, history, captures and
//! images, masked as one client of its has them ([`Request::Masked`]),
//! and shows a window of its own a run of hidden lines itself, asking
//! this daemon only which lines they are ([`Response::HiddenRun`]). An
//! older one is sent every line with a hint masked, and can't show any.
//! Whatever it asks without saying how, it's answered as an older one.
//!
//! [`Request::Unmask`]: clackatoo_protocol::Request::Unmask
//! [`Request::Masked`]: clackatoo_protocol::Request::Masked
//! [`Response::HiddenRun`]: clackatoo_protocol::Response::HiddenRun
//! [`Event::HintedView`]: clackatoo_protocol::Event::HintedView
//! [`HintRules::shown_for`]: crate::HintRules::shown_for
//! [`HintRules::noagent`]: crate::HintRules::noagent

use std::time::{Duration, Instant};

use clackatoo_protocol::{
    Error, LineHints, Masking, Pictures, Request, Response, TerminalId, wire,
};
use clackatoo_term::{Hints, Row, Terminal};
use mio::Token;

use super::{Daemon, Source};

/// The hints of `rows`, each a screen's row with its line, of `terminal`,
/// of its cursor's line, and of the virtual placements in `pictures`,
/// for a daemon relaying that masks them itself: `None` if none has any.
pub(super) fn hints_of<'a>(
    rows: impl IntoIterator<Item = (u16, &'a Row)>,
    terminal: &Terminal,
    pictures: &Pictures,
) -> Option<LineHints> {
    let masking = |row: &Row| row.hints() & LineHints::MASKING;
    let lines: Vec<(u16, Hints, u64)> = (rows.into_iter())
        .filter(|(_, row)| !masking(row).is_empty())
        .map(|(line, row)| (line, masking(row), Masking::fingerprint(row)))
        .collect();
    let grid = terminal.grid();
    let cursor = usize::from(terminal.cursor().line);
    let cursor = (cursor < grid.height())
        .then(|| grid.row(cursor))
        .filter(|row| !masking(row).is_empty())
        .map(|row| (masking(row), Masking::fingerprint(row)));
    let virtuals: Vec<(u32, u32, Hints)> = (pictures.virtuals.iter())
        .map(|virtual_| {
            let hints = terminal.virtual_hints(virtual_) & LineHints::MASKING;
            (virtual_.id, virtual_.placement, hints)
        })
        .filter(|(_, _, hints)| !hints.is_empty())
        .collect();
    let hints = LineHints {
        cols: terminal.size().cols(),
        lines,
        cursor,
        virtuals,
    };
    (!hints.is_empty()).then_some(hints)
}

/// What [`Request::Unmask`](clackatoo_protocol::Request::Unmask) does to
/// a client's `masking`, which it shows lines as they are by until
/// `shown_until`, given `run`, the run of hidden lines it asked about and
/// what each holds, and `hints`, the hints of that line, if it asked
/// about one: shows it, for `shown_for`, or masks again what was shown,
/// or says why not. What's said is what it's answered with.
pub(super) fn show_run(
    masking: &mut Masking,
    shown_until: &mut Option<Instant>,
    run: Option<(std::ops::Range<u64>, Vec<u64>)>,
    hints: Hints,
    shown_for: Duration,
) -> Result<String, Error> {
    if !masking.hidden.contains(Hints::HIDE) {
        return Err(Error::Other(
            "nothing's hidden: daemon.hints.hide is `show`".into(),
        ));
    }
    if masking.hidden.contains(Hints::NOAGENT) && hints.contains(Hints::NOAGENT) {
        return Err(Error::Other(
            "that's kept from agents, and an agent can see this window".into(),
        ));
    }
    match run {
        // Asked again where it's shown, it's masked again.
        Some((run, prints)) if masking.shown != run => {
            let lines = run.end - run.start;
            masking.shown = run;
            masking.prints = prints;
            *shown_until = Some(Instant::now() + shown_for);
            let plural = if lines == 1 { "" } else { "s" };
            Ok(format!(
                "{lines} hidden line{plural} shown for {}s",
                shown_for.as_secs()
            ))
        }
        Some(_) | None if !masking.shown.is_empty() => {
            masking.shown = 0..0;
            masking.prints.clear();
            *shown_until = None;
            Ok("hidden again".to_owned())
        }
        _ => Err(Error::Other("nothing's hidden there".into())),
    }
}

/// The most lines shown as they are at once.
const MOST_SHOWN: u64 = 10_000;

impl Daemon {
    /// The hints that mask a line for the client at `token`: what's
    /// hidden, and what's kept from agents too if agents may read through
    /// it, as config says.
    pub(super) fn hidden_for(&self, token: Token, follows: bool) -> Hints {
        let rules = self.config.hints;
        let mut hidden = Hints::empty();
        if rules.hide {
            hidden |= Hints::HIDE;
        }
        // A window an agent sees, a client not showing the terminal, as
        // an agent's command could be, and a daemon relaying, whose
        // clients this daemon doesn't know, never get it as it is.
        let granted = (self.tools.native.values()).any(|&(window, _)| window == token);
        if rules.noagent && (granted || !follows || self.relays(token)) {
            hidden |= Hints::NOAGENT;
        }
        hidden
    }

    /// How lines are masked for the client at `token` in `terminal`: as
    /// its view of it is, or as for a client agents may read through, if
    /// it doesn't follow it. For a daemon relaying, as it asked for one
    /// of its clients ([`Request::Masked`]), or else as for one agents
    /// may read through.
    pub(super) fn masking_for(&self, token: Token, terminal: TerminalId) -> Masking {
        if self.relays(token) {
            return (self.links.masked_as.clone()).unwrap_or_else(|| Masking {
                hidden: self.hidden_for(token, false),
                ..Masking::NONE
            });
        }
        let followed = match self.sources.get(token.0) {
            Some(Source::Client(client)) => client.views.get(&terminal),
            _ => None,
        };
        followed.map_or_else(
            || Masking {
                hidden: self.hidden_for(token, false),
                ..Masking::NONE
            },
            |subscription| subscription.masking.clone(),
        )
    }

    /// Whether the client at `token` is a daemon relaying that masks lines
    /// for each of its own clients: one of protocol 153 or later.
    pub(super) fn masks_relayed(&self, token: Token) -> bool {
        self.relays(token)
            && self
                .client_wire(token)
                .is_some_and(|wire| wire >= wire::RELAY_HINTS)
    }

    /// How a view followed by the client at `token` is masked, and
    /// whether it's sent with its lines' hints, as to a daemon relaying
    /// that masks them itself, which is sent them as they are.
    pub(super) fn view_masking(&self, token: Token) -> (Masking, bool) {
        if self.masks_relayed(token) {
            return (Masking::NONE, true);
        }
        let masking = Masking {
            hidden: self.hidden_for(token, true),
            ..Masking::NONE
        };
        (masking, false)
    }

    /// Answers [`Request::Masked`] from the client at `token`: `request`,
    /// answered as for a client that has lines masked as `masking` says,
    /// only for a daemon relaying that masks lines itself.
    pub(super) fn respond_masked(
        &mut self,
        token: Token,
        masking: Masking,
        request: Request,
    ) -> Response {
        if !self.masks_relayed(token) {
            return Response::Error(Error::Other(
                "only a daemon relaying asks for lines masked as it says".into(),
            ));
        }
        if !matches!(
            request,
            Request::History { .. }
                | Request::Text { .. }
                | Request::BlockText { .. }
                | Request::Search { .. }
                | Request::Capture { .. }
                | Request::Image { .. }
        ) {
            return Response::Error(Error::Other(
                "only a terminal's lines are asked for masked".into(),
            ));
        }
        let before = self.links.masked_as.replace(masking);
        let response = self.respond(request, token);
        self.links.masked_as = before;
        response
    }

    /// Answers [`Request::Unmask`](clackatoo_protocol::Request::Unmask)
    /// from the client at `token`: shows it the run of masked lines of
    /// `terminal` around `line` as they are, or masks them again.
    pub(super) fn unmask(
        &mut self,
        token: Token,
        terminal: TerminalId,
        line: Option<u64>,
    ) -> Result<Response, Error> {
        if self.is_web_device(token) {
            return Err(Error::Other(
                "a device on the web app can't show what's hidden".into(),
            ));
        }
        if self.relays(token) && !self.masks_relayed(token) {
            return Err(Error::Other(
                "what's hidden is shown only in a window on its own machine, or one reaching it \
                 with --remote"
                    .into(),
            ));
        }
        // A daemon relaying shows its window the lines itself: it's told
        // which they are.
        if self.relays(token) {
            let grid = self.grid(terminal)?;
            let line = line.ok_or_else(|| Error::Other("nothing's hidden there".into()))?;
            let run = (grid.hidden_run(line))
                .ok_or_else(|| Error::Other("nothing's hidden there".into()))?;
            let end = run.end.min(run.start + MOST_SHOWN);
            let prints = (run.start..end)
                .map(|line| {
                    grid.document_line(line)
                        .map_or(0, |row| Masking::fingerprint(&row))
                })
                .collect();
            return Ok(Response::HiddenRun {
                lines: run.start..end,
                prints,
                hints: grid.line_hints(line) & LineHints::MASKING,
            });
        }
        let hidden = self.masking_for(token, terminal).hidden;
        let grid = self.grid(terminal)?;
        if hidden.contains(Hints::NOAGENT)
            && line.is_some_and(|line| grid.line_hints(line).contains(Hints::NOAGENT))
        {
            return Err(Error::Other(
                "that's kept from agents, and an agent can see this window".into(),
            ));
        }
        let run = line.and_then(|line| grid.hidden_run(line)).map(|run| {
            let end = run.end.min(run.start + MOST_SHOWN);
            let prints = (run.start..end)
                .map(|line| {
                    grid.document_line(line)
                        .map_or(0, |row| Masking::fingerprint(&row))
                })
                .collect::<Vec<u64>>();
            (run.start..end, prints)
        });
        let shown_for = self.config.hints.shown_for;
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return Err(Error::Other("only a window can show what's hidden".into()));
        };
        let subscription = (client.views.get_mut(&terminal))
            .ok_or_else(|| Error::Other(format!("terminal {terminal} isn't shown here")))?;
        // What's kept from agents was refused above, for a line asked
        // about.
        let said = show_run(
            &mut subscription.masking,
            &mut subscription.shown_until,
            run,
            Hints::empty(),
            shown_for,
        )?;
        subscription.forget_sent();
        self.views_pending = true;
        Ok(Response::Text(said))
    }

    /// When lines shown as they are are next masked again.
    pub(super) fn next_mask_due(&self) -> Option<Instant> {
        (self.sources.iter())
            .filter_map(|(_, source)| match source {
                Source::Client(client) => Some(client),
                _ => None,
            })
            .flat_map(|client| client.views.values())
            .filter_map(|subscription| subscription.shown_until)
            .chain(self.next_relayed_mask_due())
            .min()
    }

    /// Masks again the lines shown as they are whose time is up.
    pub(super) fn mask_again_if_due(&mut self) {
        let now = Instant::now();
        for (_, source) in &mut self.sources {
            let Source::Client(client) = source else {
                continue;
            };
            for subscription in client.views.values_mut() {
                if subscription.shown_until.is_some_and(|until| until <= now) {
                    subscription.masking.shown = 0..0;
                    subscription.masking.prints.clear();
                    subscription.shown_until = None;
                    subscription.forget_sent();
                    self.views_pending = true;
                }
            }
        }
        self.mask_relayed_again_if_due(now);
    }

    /// Masks what each window shows by what agents may read through it
    /// now, as it grants a chat native access to itself or takes it back,
    /// or starts relaying:
    /// what changed is sent afresh.
    pub(super) fn mask_by_grants(&mut self) {
        let tokens: Vec<usize> = (self.sources.iter())
            .filter(|(_, source)| matches!(source, Source::Client(_)))
            .map(|(key, _)| key)
            .collect();
        for key in tokens {
            let (masking, hinted) = self.view_masking(Token(key));
            let Some(Source::Client(client)) = self.sources.get_mut(key) else {
                continue;
            };
            for subscription in client.views.values_mut() {
                if subscription.masking.hidden != masking.hidden || subscription.hinted != hinted
                {
                    // What was shown is masked again too.
                    subscription.masking = masking.clone();
                    subscription.hinted = hinted;
                    subscription.shown_until = None;
                    subscription.forget_sent();
                    self.views_pending = true;
                }
            }
        }
        self.mask_relayed_by_grants();
    }
}

#[cfg(test)]
mod tests {

    use clackatoo_protocol::{NewTerminal, Node, Position, Request, Size, ViewUpdate};
    use clackatoo_term::hints::MASK;

    use super::*;
    use crate::server::{Client, Hosted, Link, PROTOCOL_VERSION};
    use clackatoo_protocol::wire;

    /// A window on this machine's socket, and the other end of it.
    fn window(daemon: &mut Daemon) -> (Token, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let stream = mio::net::UnixStream::from_std(ours);
        let token = Token(daemon.sources.vacant_key());
        daemon
            .sources
            .insert(Source::Client(Client::new(Link::Unix(stream))));
        (token, theirs)
    }

    /// A daemon with a terminal whose line 1 a program marked hidden,
    /// masked if `hide`, and a window following it.
    fn hidden(
        dir: &std::path::Path,
        hide: bool,
    ) -> (Daemon, TerminalId, Token, std::os::unix::net::UnixStream) {
        let (mut daemon, _) = crate::server::agent_files::tests::daemon_with_chat(dir);
        daemon.config.hints.hide = hide;
        let request = NewTerminal {
            size: Size::new(40, 6).unwrap(),
            command: Some(vec!["cat".into()]),
            cwd: None,
            record_to: None,
        };
        let terminal = daemon.new_terminal(&request).unwrap();
        feed(
            &mut daemon,
            terminal,
            b"one\r\n\x1b]46373;region;start;r1;hide\x07hunter2\r\n\x1b]46373;region;end;r1\x07two",
        );
        let (token, theirs) = window(&mut daemon);
        daemon.attach(token, terminal).unwrap();
        (daemon, terminal, token, theirs)
    }

    fn feed(daemon: &mut Daemon, terminal: TerminalId, output: &[u8]) {
        if let Some(Hosted::Pty(pty)) =
            (daemon.terminals.get_mut(&terminal)).map(|slot| &mut slot.terminal)
        {
            pty.emulator_mut().advance(output);
        }
    }

    /// The text of the lines the window is sent next, which must be a
    /// snapshot.
    fn snapshot(daemon: &mut Daemon, token: Token, terminal: TerminalId) -> Vec<String> {
        let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) else {
            panic!("no window");
        };
        let slot = &daemon.terminals[&terminal];
        let subscription = client.views.get_mut(&terminal).unwrap();
        let update = subscription.update_from(&slot.terminal, PROTOCOL_VERSION, None);
        let Some(ViewUpdate::Snapshot(view)) = update else {
            panic!("not a snapshot: {update:?}");
        };
        let Node::Grid(grid) = view.root else {
            panic!("not a grid");
        };
        grid.lines
            .iter()
            .map(clackatoo_protocol::Line::text)
            .collect()
    }

    fn text(daemon: &mut Daemon, token: Token, terminal: TerminalId) -> Response {
        let text = Request::Text {
            terminal,
            from: Position { line: 0, col: 0 },
            to: Position { line: 2, col: 9 },
            whole_lines: true,
        };
        daemon.respond(text, token)
    }

    fn found(daemon: &mut Daemon, token: Token, terminal: TerminalId) -> bool {
        let search = Request::Search {
            terminal,
            query: "hunter".into(),
            from: Position { line: 0, col: 0 },
            backward: false,
            exact: false,
            count: None,
        };
        !matches!(daemon.respond(search, token), Response::Found(None))
    }

    /// Lines marked hidden are masked in what a window is sent, copies
    /// and finds, until it asks to see them, for a while, or asks again.
    #[test]
    fn hidden_lines_are_masked_until_the_user_asks_to_see_them() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        let lines = snapshot(&mut daemon, token, terminal);
        assert_eq!(lines[..3], ["one", MASK, "two"]);
        let copied = format!("one\n{MASK}\ntwo");
        assert_eq!(
            text(&mut daemon, token, terminal),
            Response::Text(copied.clone())
        );
        assert!(!found(&mut daemon, token, terminal));
        // Asked where nothing's hidden, nothing's shown.
        let unmask = |line| Request::Unmask { terminal, line };
        let asked = daemon.respond(unmask(Some(0)), token);
        assert!(matches!(asked, Response::Error(_)), "{asked:?}");
        // Asked to see it, it's sent as it is, for a while.
        let asked = daemon.respond(unmask(Some(1)), token);
        assert_eq!(asked, Response::Text("1 hidden line shown for 30s".into()));
        assert_eq!(snapshot(&mut daemon, token, terminal)[1], "hunter2");
        let shown = "one\nhunter2\ntwo".to_owned();
        assert_eq!(text(&mut daemon, token, terminal), Response::Text(shown));
        assert!(found(&mut daemon, token, terminal));
        // Masked again once the time's up.
        assert!(daemon.next_mask_due().is_some());
        if let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) {
            client.views.get_mut(&terminal).unwrap().shown_until = Some(Instant::now());
        }
        daemon.mask_again_if_due();
        assert_eq!(snapshot(&mut daemon, token, terminal)[1], MASK);
        assert_eq!(text(&mut daemon, token, terminal), Response::Text(copied));
        assert_eq!(daemon.next_mask_due(), None);
        // Or at once, asked again.
        daemon.respond(unmask(Some(1)), token);
        snapshot(&mut daemon, token, terminal);
        let asked = daemon.respond(unmask(Some(1)), token);
        assert_eq!(asked, Response::Text("hidden again".into()));
        assert_eq!(snapshot(&mut daemon, token, terminal)[1], MASK);
    }

    /// Masked lines that scrolled into history are fetched masked, and
    /// once asked to see them, as they are.
    #[test]
    fn history_is_fetched_masked() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        feed(&mut daemon, terminal, b"\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8");
        let fetch = || Request::History {
            terminal,
            end: 3,
            count: 3,
        };
        let Response::History(lines) = daemon.respond(fetch(), token) else {
            panic!("no history");
        };
        let texts: Vec<String> = (lines.lines.iter()).map(|line| line.line.text()).collect();
        assert_eq!(texts, ["one", MASK, "two"]);
        let unmask = Request::Unmask {
            terminal,
            line: Some(1),
        };
        daemon.respond(unmask, token);
        let Response::History(lines) = daemon.respond(fetch(), token) else {
            panic!("no history");
        };
        assert_eq!(lines.lines[1].line.text(), "hunter2");
    }

    /// With config set to show them, nothing's masked, and there's nothing
    /// to ask to see.
    #[test]
    fn config_can_show_what_programs_mark_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), false);
        assert_eq!(snapshot(&mut daemon, token, terminal)[1], "hunter2");
        assert!(found(&mut daemon, token, terminal));
        let unmask = Request::Unmask {
            terminal,
            line: Some(1),
        };
        let asked = daemon.respond(unmask, token);
        assert!(matches!(asked, Response::Error(_)), "{asked:?}");
    }

    /// A phone is sent the mask, and can't ask to see what's under it.
    #[test]
    fn a_web_device_cant_ask_to_see_what_is_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, _, _theirs) = hidden(dir.path(), true);
        let (phone, _reader) = crate::server::tests::web_reader(&mut daemon);
        daemon.attach(phone, terminal).unwrap();
        assert_eq!(snapshot(&mut daemon, phone, terminal)[1], MASK);
        let unmask = Request::Unmask {
            terminal,
            line: Some(1),
        };
        let asked = daemon.respond(unmask, phone);
        assert!(matches!(asked, Response::Error(_)), "{asked:?}");
    }

    /// A line shown as it is, printed on afresh, is masked again: what's
    /// shown is what was there, not its number.
    #[test]
    fn a_line_printed_afresh_where_one_was_shown_is_masked_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        let unmask = Request::Unmask {
            terminal,
            line: Some(1),
        };
        daemon.respond(unmask, token);
        assert_eq!(snapshot(&mut daemon, token, terminal)[1], "hunter2");
        // Another program prints another secret on the same line.
        feed(
            &mut daemon,
            terminal,
            b"\x1b[2H\x1b]46373;region;start;r2;hide\x07\x1b[2Kswordfish\x1b]46373;region;end;r2\x07",
        );
        if let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) {
            client.views.get_mut(&terminal).unwrap().forget_sent();
        }
        assert_eq!(snapshot(&mut daemon, token, terminal)[1], MASK);
        let copied = format!("one\n{MASK}\ntwo");
        assert_eq!(text(&mut daemon, token, terminal), Response::Text(copied));
    }

    /// What's kept from agents is masked for a client that doesn't show
    /// the terminal, as `clackatoo capture` run by an agent's command, and
    /// for a window an agent has been given control of; a window showing
    /// it otherwise has it.
    #[test]
    fn whats_kept_from_agents_is_masked_where_agents_may_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        feed(
            &mut daemon,
            terminal,
            b"\r\n\x1b]46373;region;start;p;private\x07token\x1b]46373;region;end;p\x07",
        );
        let all = || Request::Text {
            terminal,
            from: Position { line: 0, col: 0 },
            to: Position { line: 3, col: 9 },
            whole_lines: true,
        };
        let shown = format!("one\n{MASK}\ntwo\ntoken");
        assert_eq!(daemon.respond(all(), token), Response::Text(shown.clone()));
        let (stranger, _theirs) = window(&mut daemon);
        let masked = format!("one\n{MASK}\ntwo\n{MASK}");
        assert_eq!(
            daemon.respond(all(), stranger),
            Response::Text(masked.clone())
        );
        let Response::View(view) = daemon.respond(Request::Capture { terminal }, stranger) else {
            panic!("no view");
        };
        let lines: Vec<String> = view
            .grid()
            .unwrap()
            .lines
            .iter()
            .map(clackatoo_protocol::Line::text)
            .collect();
        assert_eq!(lines[3], MASK);
        // A window an agent drives.
        let chat = TerminalId(4242);
        daemon
            .tools
            .native
            .insert(chat, (token, clackatoo_protocol::NativeAccess::Control));
        daemon.mask_by_grants();
        assert_eq!(snapshot(&mut daemon, token, terminal)[3], MASK);
        assert_eq!(daemon.respond(all(), token), Response::Text(masked));
        daemon.tools.native.remove(&chat);
        daemon.mask_by_grants();
        assert_eq!(snapshot(&mut daemon, token, terminal)[3], "token");
        assert_eq!(daemon.respond(all(), token), Response::Text(shown));
    }

    /// Makes the client at `token` a daemon relaying, of protocol `wire`.
    fn relaying(daemon: &mut Daemon, token: Token, wire: u32) {
        if let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) {
            client.relays = true;
            client.wire = wire;
        }
    }

    /// A daemon relaying older than 153 can't ask to see what's hidden, as
    /// it would show it to every client it has.
    #[test]
    fn an_older_daemon_relaying_cant_ask_to_see_what_is_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        relaying(&mut daemon, token, wire::SECRET_REGIONS);
        let unmask = Request::Unmask {
            terminal,
            line: Some(1),
        };
        let asked = daemon.respond(unmask, token);
        assert!(matches!(asked, Response::Error(_)), "{asked:?}");
    }

    /// The cursor on a hidden line is hidden, at its start.
    #[test]
    fn the_cursor_on_a_hidden_line_says_nothing_of_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        feed(&mut daemon, terminal, b"\x1b[2;6H");
        let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) else {
            panic!("no window");
        };
        let slot = &daemon.terminals[&terminal];
        let subscription = client.views.get_mut(&terminal).unwrap();
        let update = subscription.update_from(&slot.terminal, PROTOCOL_VERSION, None);
        let Some(ViewUpdate::Snapshot(view)) = update else {
            panic!("not a snapshot");
        };
        let cursor = view.grid().unwrap().state.cursor;
        assert_eq!((cursor.line, cursor.col, cursor.visible), (1, 0, false));
    }

    /// In a window an agent sees, what the user asks to see shows what's
    /// only hidden, never what's kept from agents too; and a grant made
    /// after masks again what was shown.
    #[test]
    fn a_window_an_agent_sees_never_shows_whats_kept_from_agents() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        feed(
            &mut daemon,
            terminal,
            b"\r\n\x1b]46373;region;start;s;secret\x07token\x1b]46373;region;end;s\x07",
        );
        let unmask = |line| Request::Unmask {
            terminal,
            line: Some(line),
        };
        // Shown, then granted: masked again.
        daemon.respond(unmask(1), token);
        assert_eq!(snapshot(&mut daemon, token, terminal)[1], "hunter2");
        let chat = TerminalId(4242);
        let control = clackatoo_protocol::NativeAccess::Control;
        daemon.tools.native.insert(chat, (token, control));
        daemon.mask_by_grants();
        assert_eq!(snapshot(&mut daemon, token, terminal)[1], MASK);
        // Granted, the user can see what's only hidden, but not a secret.
        daemon.respond(unmask(1), token);
        let lines = snapshot(&mut daemon, token, terminal);
        assert_eq!((lines[1].as_str(), lines[3].as_str()), ("hunter2", MASK));
        let asked = daemon.respond(unmask(3), token);
        assert!(matches!(asked, Response::Error(_)), "{asked:?}");
        let all = Request::Text {
            terminal,
            from: Position { line: 3, col: 0 },
            to: Position { line: 3, col: 9 },
            whole_lines: true,
        };
        assert_eq!(daemon.respond(all, token), Response::Text(MASK.to_owned()));
    }

    /// A daemon relaying older than 153, following a terminal for its
    /// windows, is sent what's kept from agents masked: it never leaves
    /// this machine as it is.
    #[test]
    fn whats_kept_from_agents_never_reaches_an_older_daemon_relaying() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, _, _theirs) = hidden(dir.path(), true);
        feed(
            &mut daemon,
            terminal,
            b"\r\n\x1b]46373;region;start;p;private\x07token\x1b]46373;region;end;p\x07",
        );
        let (relay, _theirs) = window(&mut daemon);
        relaying(&mut daemon, relay, wire::SECRET_REGIONS);
        daemon.attach(relay, terminal).unwrap();
        assert_eq!(snapshot(&mut daemon, relay, terminal)[3], MASK);
        let all = Request::Text {
            terminal,
            from: Position { line: 3, col: 0 },
            to: Position { line: 3, col: 9 },
            whole_lines: true,
        };
        assert_eq!(daemon.respond(all, relay), Response::Text(MASK.to_owned()));
    }

    /// Shows `bytes` in `terminal`, keeping each image it asks for under
    /// `key`, as the store would.
    fn show_image(daemon: &mut Daemon, terminal: TerminalId, bytes: &[u8], key: u8) {
        let Some(Hosted::Pty(pty)) =
            (daemon.terminals.get_mut(&terminal)).map(|slot| &mut slot.terminal)
        else {
            panic!("no terminal");
        };
        let emulator = pty.emulator_mut();
        let mut rest = bytes;
        loop {
            let used = emulator.advance_until_waiting(rest);
            rest = &rest[used..];
            for request in emulator.take_image_requests() {
                let kept = clackatoo_term::ImageKept {
                    key: [key; 16],
                    width: 20,
                    height: 20,
                };
                emulator.image_kept(request.token, Ok(kept));
            }
            if rest.is_empty() && !emulator.waiting_for_image() {
                return;
            }
        }
    }

    /// A picture on a hidden line is in no snapshot, patch or history
    /// fetch, and its image isn't sent: until the line is shown as it is,
    /// when its picture is too, while it's shown.
    #[test]
    fn a_picture_on_a_hidden_line_is_sent_only_while_shown() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        feed(&mut daemon, terminal, b"\r\n");
        show_image(
            &mut daemon,
            terminal,
            b"\x1b_Ga=T,f=100,i=7;iVBORw0K\x1b\\\r\n",
            1,
        );
        show_image(
            &mut daemon,
            terminal,
            b"\x1b]46373;region;start;s;hide\x07\x1b_Ga=T,f=100,i=8;iVBORw0K\x1b\\\x1b]46373;region;end;s\x07",
            2,
        );
        let images = |daemon: &mut Daemon| {
            let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) else {
                panic!("no window");
            };
            let slot = &daemon.terminals[&terminal];
            let subscription = client.views.get_mut(&terminal).unwrap();
            let update = subscription.update_from(&slot.terminal, PROTOCOL_VERSION, None);
            let Some(ViewUpdate::Snapshot(view)) = update else {
                panic!("not a snapshot");
            };
            let grid = view.grid().unwrap().clone();
            let mut keys: Vec<u8> = (grid.pictures.pictures.iter())
                .map(|(_, picture)| picture.image[0])
                .collect();
            keys.sort_unstable();
            keys
        };
        assert_eq!(images(&mut daemon), [1]);
        let image = |key| clackatoo_protocol::ImageId([key; 16]);
        let fetch = |key| Request::Image {
            from: clackatoo_protocol::ImageFrom::Terminal(terminal),
            image: image(key),
            fit: None,
            at: 0,
        };
        assert_eq!(
            daemon.respond(fetch(2), token),
            Response::Image(clackatoo_protocol::ImageAnswer::Missing)
        );
        assert!(daemon.image_masked(token, terminal, image(2)));
        assert!(!daemon.image_masked(token, terminal, image(1)));
        // Shown as it is, its picture is too, and its image may be sent.
        let unmask = Request::Unmask {
            terminal,
            line: Some(4),
        };
        let asked = daemon.respond(unmask, token);
        assert!(matches!(asked, Response::Text(_)), "{asked:?}");
        assert_eq!(images(&mut daemon), [1, 2]);
        assert!(!daemon.image_masked(token, terminal, image(2)));
        // Only to the window that asked.
        let (other, _theirs) = window(&mut daemon);
        daemon.attach(other, terminal).unwrap();
        assert!(daemon.image_masked(other, terminal, image(2)));
        // In history too.
        feed(&mut daemon, terminal, &b"\r\n".repeat(12));
        let history = || Request::History {
            terminal,
            end: 6,
            count: 6,
        };
        let keys = |daemon: &mut Daemon| {
            let Response::History(lines) = daemon.respond(history(), token) else {
                panic!("no history");
            };
            let mut keys: Vec<u8> = (lines.pictures.pictures.iter())
                .map(|(_, picture)| picture.image[0])
                .collect();
            keys.sort_unstable();
            keys
        };
        assert_eq!(keys(&mut daemon), [1, 2]);
        assert!(!daemon.image_masked(token, terminal, image(2)));
        // Masked again once the time's up: refused again.
        if let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) {
            client.views.get_mut(&terminal).unwrap().shown_until = Some(Instant::now());
        }
        daemon.mask_again_if_due();
        assert_eq!(keys(&mut daemon), [1]);
        assert!(daemon.image_masked(token, terminal, image(2)));
        assert_eq!(
            daemon.respond(fetch(2), token),
            Response::Image(clackatoo_protocol::ImageAnswer::Missing)
        );
    }

    /// A picture on a line kept from agents isn't shown in a window an
    /// agent sees, whatever the user asks to see there.
    #[test]
    fn a_picture_kept_from_agents_stays_masked_in_a_window_an_agent_sees() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        feed(&mut daemon, terminal, b"\r\n");
        show_image(
            &mut daemon,
            terminal,
            b"\x1b]46373;region;start;s;secret\x07\x1b_Ga=T,f=100,i=8;iVBORw0K\x1b\\\x1b]46373;region;end;s\x07",
            2,
        );
        let chat = TerminalId(4242);
        let control = clackatoo_protocol::NativeAccess::Control;
        daemon.tools.native.insert(chat, (token, control));
        daemon.mask_by_grants();
        let image = clackatoo_protocol::ImageId([2; 16]);
        assert!(daemon.image_masked(token, terminal, image));
        let asked = daemon.respond(
            Request::Unmask {
                terminal,
                line: Some(3),
            },
            token,
        );
        assert!(matches!(asked, Response::Error(_)), "{asked:?}");
        assert!(daemon.image_masked(token, terminal, image));
    }

    /// A client older than 153 that starts relaying has what it followed
    /// before masked as an older daemon relaying's is; a newer one is sent
    /// it as it is, with its hints, to mask for each of its own clients.
    #[test]
    fn a_client_that_starts_relaying_is_masked_as_its_protocol_says() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        feed(
            &mut daemon,
            terminal,
            b"\r\n\x1b]46373;region;start;p;private\x07token\x1b]46373;region;end;p\x07",
        );
        assert_eq!(snapshot(&mut daemon, token, terminal)[3], "token");
        if let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) {
            client.wire = wire::SECRET_REGIONS;
        }
        assert_eq!(daemon.respond(Request::Relay, token), Response::Done);
        assert_eq!(snapshot(&mut daemon, token, terminal)[3], MASK);
        let (newer, _theirs) = window(&mut daemon);
        daemon.attach(newer, terminal).unwrap();
        assert_eq!(daemon.respond(Request::Relay, newer), Response::Done);
        let (lines, hints) = hinted_snapshot(&mut daemon, newer, terminal);
        assert_eq!(lines[1..4], ["hunter2", "two", "token"]);
        let hinted: Vec<(u16, Hints)> = (hints.lines.iter())
            .map(|&(line, hints, _)| (line, hints))
            .collect();
        assert_eq!(hinted, [(1, Hints::HIDE), (3, Hints::NOAGENT)]);
    }

    /// The lines of the next update the client at `token` is sent of
    /// `terminal`, a snapshot, as they are, and their hints, as for a
    /// daemon relaying that masks them itself.
    fn hinted_snapshot(
        daemon: &mut Daemon,
        token: Token,
        terminal: TerminalId,
    ) -> (Vec<String>, LineHints) {
        let lines = snapshot(daemon, token, terminal);
        let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) else {
            panic!("no client");
        };
        let hints = (client.views.get_mut(&terminal).unwrap().hints.take()).expect("no hints");
        (lines, hints)
    }

    /// A daemon relaying of protocol 153 or later is sent lines as they
    /// are, with their hints; asks for copies, searches and history masked
    /// as one of its clients has them, and is answered so; is told which
    /// lines a run of hidden ones is, to show them itself; and anything it
    /// asks without saying how is masked as for an older one.
    #[test]
    fn a_daemon_relaying_masks_lines_itself() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, _, _theirs) = hidden(dir.path(), true);
        feed(
            &mut daemon,
            terminal,
            b"\r\n\x1b]46373;region;start;p;private\x07token\x1b]46373;region;end;p\x07",
        );
        let (relay, _theirs) = window(&mut daemon);
        relaying(&mut daemon, relay, PROTOCOL_VERSION);
        daemon.attach(relay, terminal).unwrap();
        let (lines, hints) = hinted_snapshot(&mut daemon, relay, terminal);
        assert_eq!(lines[1..4], ["hunter2", "two", "token"]);
        assert_eq!(hints.lines.len(), 2);
        let all = || Request::Text {
            terminal,
            from: Position { line: 0, col: 0 },
            to: Position { line: 3, col: 9 },
            whole_lines: true,
        };
        // Asked plainly: masked as for an older daemon relaying.
        let masked = format!("one\n{MASK}\ntwo\n{MASK}");
        assert_eq!(daemon.respond(all(), relay), Response::Text(masked));
        // Asked for a window there: what's kept from agents isn't masked.
        let masked_for = |shown, prints| Request::Masked {
            masking: Masking {
                hidden: Hints::HIDE,
                shown,
                prints,
            },
            request: Box::new(all()),
        };
        let for_window = format!("one\n{MASK}\ntwo\ntoken");
        assert_eq!(
            daemon.respond(masked_for(0..0, Vec::new()), relay),
            Response::Text(for_window)
        );
        // Which lines the run is, to show them to that window.
        let unmask = Request::Unmask {
            terminal,
            line: Some(1),
        };
        let Response::HiddenRun {
            lines,
            prints,
            hints,
        } = daemon.respond(unmask, relay)
        else {
            panic!("no run");
        };
        assert_eq!((lines.clone(), hints), (1..2, Hints::HIDE));
        let shown = "one\nhunter2\ntwo\ntoken".to_owned();
        assert_eq!(
            daemon.respond(masked_for(lines, prints), relay),
            Response::Text(shown)
        );
        // Nothing but a terminal's lines is asked masked, and only by a
        // daemon relaying.
        let odd = Request::Masked {
            masking: Masking::NONE,
            request: Box::new(Request::ListTerminals),
        };
        assert!(matches!(daemon.respond(odd, relay), Response::Error(_)));
        let (stranger, _theirs) = window(&mut daemon);
        let asked = daemon.respond(masked_for(0..0, Vec::new()), stranger);
        assert!(matches!(asked, Response::Error(_)), "{asked:?}");
    }

    /// A virtual placement made in a region isn't sent to a client that
    /// has the region masked, nor is its image.
    #[test]
    fn a_virtual_placement_made_in_a_region_is_never_sent() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        show_image(
            &mut daemon,
            terminal,
            b"\x1b_Ga=T,U=1,f=100,i=9,c=1,r=1,q=2;AAAA\x1b\\",
            1,
        );
        show_image(
            &mut daemon,
            terminal,
            b"\x1b]46373;region;start;s;hide\x07\x1b_Ga=T,U=1,f=100,i=10,c=1,r=1,q=2;AAAA\x1b\\\x1b]46373;region;end;s\x07",
            2,
        );
        let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) else {
            panic!("no window");
        };
        let slot = &daemon.terminals[&terminal];
        let subscription = client.views.get_mut(&terminal).unwrap();
        let update = subscription.update_from(&slot.terminal, PROTOCOL_VERSION, None);
        let Some(ViewUpdate::Snapshot(view)) = update else {
            panic!("not a snapshot");
        };
        let ids: Vec<u32> = (view.grid().unwrap().pictures.virtuals.iter())
            .map(|virtual_| virtual_.id)
            .collect();
        assert_eq!(ids, [9]);
        let fetch = Request::Image {
            from: clackatoo_protocol::ImageFrom::Terminal(terminal),
            image: clackatoo_protocol::ImageId([2; 16]),
            fit: None,
            at: 0,
        };
        assert_eq!(
            daemon.respond(fetch, token),
            Response::Image(clackatoo_protocol::ImageAnswer::Missing)
        );
    }
}
