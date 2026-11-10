//! Lines a program marked to be hidden (`OSC 3008`, see
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
//! [`Request::Unmask`]: clackatoo_protocol::Request::Unmask
//! [`HintRules::shown_for`]: crate::HintRules::shown_for
//! [`HintRules::noagent`]: crate::HintRules::noagent

use std::time::Instant;

use clackatoo_protocol::{Error, Masking, Response, TerminalId};
use clackatoo_term::Hints;
use mio::Token;

use super::{Daemon, Source};

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
    /// it doesn't follow it.
    pub(super) fn masking_for(&self, token: Token, terminal: TerminalId) -> Masking {
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
        if self.relays(token) {
            return Err(Error::Other(
                "what's hidden is shown only in a window on its own machine, or one reaching it \
                 with --remote"
                    .into(),
            ));
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
        let said = match run {
            _ if !subscription.masking.hidden.contains(Hints::HIDE) => {
                return Err(Error::Other(
                    "nothing's hidden: daemon.hints.hide is `show`".into(),
                ));
            }
            // Asked again where it's shown, it's masked again.
            Some((run, prints)) if subscription.masking.shown != run => {
                let lines = run.end - run.start;
                subscription.masking.shown = run;
                subscription.masking.prints = prints;
                subscription.shown_until = Some(Instant::now() + shown_for);
                let plural = if lines == 1 { "" } else { "s" };
                format!(
                    "{lines} hidden line{plural} shown for {}s",
                    shown_for.as_secs()
                )
            }
            Some(_) | None if !subscription.masking.shown.is_empty() => {
                subscription.masking.shown = 0..0;
                subscription.masking.prints.clear();
                subscription.shown_until = None;
                "hidden again".to_owned()
            }
            _ => return Err(Error::Other("nothing's hidden there".into())),
        };
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
            let hidden = self.hidden_for(Token(key), true);
            let Some(Source::Client(client)) = self.sources.get_mut(key) else {
                continue;
            };
            for subscription in client.views.values_mut() {
                if subscription.masking.hidden != hidden {
                    // What was shown is masked again too.
                    subscription.masking = clackatoo_protocol::Masking {
                        hidden,
                        ..clackatoo_protocol::Masking::NONE
                    };
                    subscription.shown_until = None;
                    subscription.forget_sent();
                    self.views_pending = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {

    use clackatoo_protocol::{NewTerminal, Node, Position, Request, Size, ViewUpdate};
    use clackatoo_term::hints::MASK;

    use super::*;
    use crate::server::{Client, Hosted, Link, PROTOCOL_VERSION};

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
            b"one\r\n\x1b]3008;region;start;r1;hide\x07hunter2\r\n\x1b]3008;region;end;r1\x07two",
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
            b"\x1b[2H\x1b]3008;region;start;r2;hide\x07\x1b[2Kswordfish\x1b]3008;region;end;r2\x07",
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
            b"\r\n\x1b]3008;region;start;p;private\x07token\x1b]3008;region;end;p\x07",
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

    /// A daemon relaying can't ask to see what's hidden, as it would show
    /// it to every client it has.
    #[test]
    fn a_daemon_relaying_cant_ask_to_see_what_is_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        if let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) {
            client.relays = true;
        }
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
            b"\r\n\x1b]3008;region;start;s;secret\x07token\x1b]3008;region;end;s\x07",
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

    /// A daemon relaying, following a terminal for its windows, is sent
    /// what's kept from agents masked: it never leaves this machine as it
    /// is.
    #[test]
    fn whats_kept_from_agents_never_reaches_another_machine() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, _, _theirs) = hidden(dir.path(), true);
        feed(
            &mut daemon,
            terminal,
            b"\r\n\x1b]3008;region;start;p;private\x07token\x1b]3008;region;end;p\x07",
        );
        let (relay, _theirs) = window(&mut daemon);
        if let Some(Source::Client(client)) = daemon.sources.get_mut(relay.0) {
            client.relays = true;
        }
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
    /// fetch, even shown as it is, and its image isn't sent.
    #[test]
    fn a_picture_on_a_hidden_line_is_never_sent() {
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
            b"\x1b]3008;region;start;s;hide\x07\x1b_Ga=T,f=100,i=8;iVBORw0K\x1b\\\x1b]3008;region;end;s\x07",
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
        // Shown as it is, its text is, but not its picture.
        let unmask = Request::Unmask {
            terminal,
            line: Some(4),
        };
        let asked = daemon.respond(unmask, token);
        assert!(matches!(asked, Response::Text(_)), "{asked:?}");
        assert_eq!(images(&mut daemon), [1]);
        let fetch = |key| Request::Image {
            from: clackatoo_protocol::ImageFrom::Terminal(terminal),
            image: clackatoo_protocol::ImageId([key; 16]),
            fit: None,
            at: 0,
        };
        assert_eq!(
            daemon.respond(fetch(2), token),
            Response::Image(clackatoo_protocol::ImageAnswer::Missing)
        );
        // In history, still left out.
        feed(&mut daemon, terminal, &b"\r\n".repeat(12));
        let history = Request::History {
            terminal,
            end: 6,
            count: 6,
        };
        let Response::History(lines) = daemon.respond(history, token) else {
            panic!("no history");
        };
        let keys: Vec<u8> = (lines.pictures.pictures.iter())
            .map(|(_, picture)| picture.image[0])
            .collect();
        assert_eq!(keys, [1]);
    }

    /// A window that starts relaying has what it followed before masked as
    /// a daemon relaying's is.
    #[test]
    fn a_client_that_starts_relaying_gets_whats_kept_from_agents_masked() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, terminal, token, _theirs) = hidden(dir.path(), true);
        feed(
            &mut daemon,
            terminal,
            b"\r\n\x1b]3008;region;start;p;private\x07token\x1b]3008;region;end;p\x07",
        );
        assert_eq!(snapshot(&mut daemon, token, terminal)[3], "token");
        assert_eq!(daemon.respond(Request::Relay, token), Response::Done);
        assert_eq!(snapshot(&mut daemon, token, terminal)[3], MASK);
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
            b"\x1b]3008;region;start;s;hide\x07\x1b_Ga=T,U=1,f=100,i=10,c=1,r=1,q=2;AAAA\x1b\\\x1b]3008;region;end;s\x07",
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
