//! Tests of guessing at the echo of what's typed: a pane showing a real
//! terminal, standing in for the daemon's, with the keys' answers and the
//! program's echo arriving when each test says.

use std::time::{Duration, Instant};

use clackatoo_protocol::{
    ClientMessage, GridState, Line, NodeId, Patch, Request, Response, Size, TerminalId, View,
    ViewUpdate,
};
use clackatoo_term::{Config, Terminal, Underline};
use winit::keyboard::{Key, KeyLocation, NamedKey};

use super::{Guess, Input, PredictEcho, Predictor, Tally};
use crate::input::{KeyPress, Modifiers};
use crate::options::Options;
use crate::pane::{Mode, Pane};
use crate::screen::Screen;

/// A prompt, marked as a shell with shell integration marks it.
const PROMPT: &str = "\x1b]133;A\x07$ ";

/// A command run and finished, marked, with nothing printed: what a shell
/// that marks its commands has shown before its prompt is trusted.
const RAN: &str = "\x1b]133;A\x07\x1b]133;C\x07\x1b]133;D;0\x07";

/// A terminal standing in for the daemon's, and a pane showing it.
struct Fixture {
    terminal: Terminal,
    pane: Pane,
}

impl Fixture {
    /// A pane guessing as `choice` says, for a daemon on another machine,
    /// showing a terminal 20 × 4 after `output`, from a shell that has
    /// marked a command, printing nothing, before it.
    fn new(choice: PredictEcho, output: &str) -> Self {
        let mut terminal = Terminal::new(Size::new(20, 4).unwrap(), &Config::default());
        terminal.advance(RAN.as_bytes());
        terminal.advance(output.as_bytes());
        let mut pane = Pane::new(TerminalId(1));
        pane.set_remote(true);
        pane.set_options(&Options {
            predictecho: choice,
            ..Options::default()
        });
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal)));
        Self { terminal, pane }
    }

    fn type_keys(&mut self, keys: &[Key]) {
        for key in keys {
            let text = match key {
                Key::Character(text) => Some(text.to_string()),
                _ => None,
            };
            self.pane.key(&KeyPress {
                key,
                text: text.as_deref(),
                location: KeyLocation::Standard,
                code: None,
                modifiers: Modifiers::default(),
            });
        }
    }

    fn type_text(&mut self, text: &str) {
        let keys: Vec<Key> = (text.chars())
            .map(|ch| Key::Character(ch.to_string().into()))
            .collect();
        self.type_keys(&keys);
    }

    /// The daemon answers everything sent: input has gone to the program.
    /// Returns the input.
    fn answer(&mut self) -> Vec<u8> {
        let mut input = Vec::new();
        for message in self.pane.take_outbox() {
            if let ClientMessage::Request(request) = message {
                if let Request::SendInput { bytes, .. } = request {
                    input.extend(bytes);
                }
                self.pane.answered(Response::Done);
            }
        }
        input
    }

    /// The program writes `output`, and the daemon sends the screen.
    fn echo(&mut self, output: &str) {
        self.terminal.advance(output.as_bytes());
        let grid = self.terminal.grid();
        let lines = (0..grid.height())
            .map(|line| (u16::try_from(line).unwrap(), Line::from_row(grid.row(line))))
            .collect();
        self.pane.view_changed(&ViewUpdate::Patch(vec![Patch::Grid {
            node: NodeId::ROOT,
            state: GridState::of(&self.terminal),
            lines,
            links: Vec::new(),
            marks: clackatoo_protocol::BlockMarks::default(),
        }]));
    }

    /// Long enough passes for every echo to have arrived.
    fn time_passes(&mut self) {
        self.pane.tick(Instant::now() + Duration::from_secs(10));
    }

    /// Line `line` as the pane draws it, and the cursor's column if it's
    /// on that line.
    fn drawn(&self, line: usize) -> (String, Option<usize>) {
        let (rows, cursor) = self.pane.scene(4);
        let cursor = cursor.filter(|cursor| cursor.line == line);
        (rows[line].text(), cursor.map(|cursor| cursor.col))
    }

    /// The columns of line `line` drawn underlined.
    fn underlined(&self, line: usize) -> Vec<usize> {
        let (rows, _) = self.pane.scene(4);
        let row = rows[line];
        (row.cells().iter().enumerate())
            .filter(|&(_, &cell)| row.style(cell).underline != Underline::None)
            .map(|(col, _)| col)
            .collect()
    }

    /// Line `line` as the daemon last sent it.
    fn real(&self, line: usize) -> String {
        self.pane.screen().unwrap().rows[line].text()
    }

    fn tally(&self) -> Tally {
        self.pane.predictor.tally()
    }
}

fn named(key: NamedKey) -> Key {
    Key::Named(key)
}

#[test]
fn typing_at_a_prompt_shows_at_once_and_the_echo_confirms_it() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("ls");
    assert_eq!(fixture.drawn(0), ("$ ls".into(), Some(4)));
    assert_eq!(fixture.underlined(0), [2, 3]);
    assert_eq!(fixture.real(0), "$", "the screen itself is untouched");
    assert_eq!(fixture.answer(), b"ls");

    // The echo of the first key confirms it; the second still shows.
    fixture.echo("l");
    assert_eq!(fixture.drawn(0), ("$ ls".into(), Some(4)));
    assert_eq!(fixture.underlined(0), [3]);
    fixture.echo("s");
    assert_eq!(fixture.drawn(0), ("$ ls".into(), Some(4)));
    assert!(fixture.underlined(0).is_empty());
    assert!(fixture.pane.overlay.is_none());
    fixture.time_passes();
    assert_eq!(
        fixture.tally(),
        Tally {
            shown: 2,
            right: 2,
            ..Tally::default()
        }
    );
}

#[test]
fn a_key_whose_guess_shows_asks_for_a_redraw() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    let key = Key::Character("l".into());
    let press = KeyPress {
        key: &key,
        text: Some("l"),
        location: KeyLocation::Standard,
        code: None,
        modifiers: Modifiers::default(),
    };
    assert!(fixture.pane.key(&press));
    fixture.pane.set_options(&Options {
        predictecho: PredictEcho::Never,
        ..Options::default()
    });
    assert!(!fixture.pane.key(&press), "as before guessing");
}

#[test]
fn a_screen_older_than_the_keys_proves_nothing() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("ab");
    fixture.answer();
    // A screen comes without the keys' echo, which is on its way.
    fixture.echo("");
    assert_eq!(fixture.drawn(0), ("$ ab".into(), Some(4)));
    assert_eq!(fixture.tally().wrong, 0);
}

#[test]
fn without_shell_marks_a_line_waits_for_its_first_echo() {
    let mut fixture = Fixture::new(PredictEcho::Always, "> ");
    fixture.type_text("ls");
    assert_eq!(fixture.drawn(0), (">".into(), Some(2)), "tentative");
    fixture.answer();
    fixture.echo("l");
    // The echo came back as guessed, so the rest shows, and what's typed
    // next shows at once.
    assert_eq!(fixture.drawn(0), ("> ls".into(), Some(4)));
    assert_eq!(fixture.underlined(0), [3]);
    fixture.type_text("x");
    assert_eq!(fixture.drawn(0), ("> lsx".into(), Some(5)));
}

#[test]
fn a_wrong_guess_goes_and_guessing_turns_cautious() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("j");
    assert_eq!(fixture.drawn(0), ("$ j".into(), Some(3)));
    fixture.answer();
    // The line editor took it as a command, as in vi mode, and echoed
    // nothing.
    fixture.echo("");
    fixture.time_passes();
    assert_eq!(fixture.drawn(0), ("$".into(), Some(2)));
    assert_eq!(fixture.tally().wrong, 1);

    // At the same prompt, guesses wait for an echo now.
    fixture.type_text("ab");
    assert_eq!(fixture.drawn(0), ("$".into(), Some(2)));
    fixture.answer();
    fixture.echo("a");
    assert_eq!(fixture.drawn(0), ("$ ab".into(), Some(4)));

    // The next prompt is trusted again.
    fixture.type_keys(&[named(NamedKey::Enter)]);
    fixture.answer();
    fixture.echo("b\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
    fixture.echo(PROMPT);
    fixture.time_passes();
    fixture.type_text("c");
    assert_eq!(fixture.drawn(1), ("$ c".into(), Some(3)));
}

#[test]
fn a_password_prompt_never_shows_what_is_typed() {
    let mut fixture = Fixture::new(
        PredictEcho::Always,
        "$ sudo true\r\n\x1b]133;C\x07Password: ",
    );
    fixture.type_text("secret");
    assert_eq!(fixture.drawn(1).0, "Password:");
    fixture.answer();
    fixture.echo("");
    assert_eq!(fixture.drawn(1).0, "Password:");
    fixture.time_passes();
    fixture.type_text("word");
    assert_eq!(fixture.drawn(1).0, "Password:");
    fixture.answer();
    fixture.time_passes();
    assert_eq!(
        fixture.tally(),
        Tally {
            unseen_wrong: 10,
            ..Tally::default()
        }
    );
}

#[test]
fn nothing_is_guessed_on_the_alternate_screen() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.echo("\x1b[?1049h");
    fixture.type_text("ihello");
    assert!(fixture.pane.overlay.is_none());
    assert_eq!(fixture.answer(), b"ihello", "the keys still go");
}

#[test]
fn keys_typed_into_a_full_screen_program_are_not_guessed_once_it_has_gone() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("vi");
    fixture.type_keys(&[named(NamedKey::Enter)]);
    // Typed before the window saw vi start.
    fixture.type_text("ZZ");
    fixture.answer();
    // The echo of "vi" and vi's screen come in one update.
    fixture.echo("vi\r\n\x1b]133;C\x07\x1b[?1049h~");
    assert!(fixture.pane.overlay.is_none(), "nothing drawn over vi");
    fixture.echo("\x1b[?1049l\x1b]133;D;0\x07\r\n");
    fixture.echo(PROMPT);
    fixture.time_passes();
    assert_eq!(fixture.drawn(2), ("$".into(), Some(2)));
    assert_eq!(fixture.tally().wrong, 0);
}

#[test]
fn nothing_is_guessed_for_a_program_using_the_mouse() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.echo("\x1b[?1000h");
    fixture.type_text("a");
    assert!(fixture.pane.overlay.is_none());
}

#[test]
fn guesses_are_not_drawn_in_normal_mode() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("ls");
    assert_eq!(fixture.drawn(0).0, "$ ls");
    fixture.pane.enter_normal_mode();
    assert_eq!(fixture.pane.mode, Mode::Normal);
    assert_eq!(fixture.drawn(0).0, "$", "normal mode shows the real screen");
}

#[test]
fn a_daemon_on_this_machine_is_left_alone_unless_asked() {
    let mut fixture = Fixture::new(PredictEcho::Auto, PROMPT);
    fixture.pane.set_remote(false);
    fixture.type_text("ls");
    assert!(fixture.pane.overlay.is_none());
    fixture.answer();
    assert_eq!(fixture.pane.wakes(), None, "nothing to wake for");
    assert_eq!(fixture.tally(), Tally::default());

    let mut never = Fixture::new(PredictEcho::Never, PROMPT);
    never.type_text("ls");
    assert!(never.pane.overlay.is_none());

    let mut always = Fixture::new(PredictEcho::Always, PROMPT);
    always.pane.set_remote(false);
    always.type_text("ls");
    assert_eq!(always.drawn(0).0, "$ ls");
}

#[test]
fn backspace_and_arrows_within_what_was_typed() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("abc");
    fixture.type_keys(&[named(NamedKey::ArrowLeft), named(NamedKey::ArrowLeft)]);
    assert_eq!(fixture.drawn(0), ("$ abc".into(), Some(3)));
    fixture.type_text("X");
    assert_eq!(fixture.drawn(0), ("$ aXbc".into(), Some(4)));
    fixture.type_keys(&[named(NamedKey::Backspace)]);
    assert_eq!(fixture.drawn(0), ("$ abc".into(), Some(3)));
    fixture.type_keys(&[named(NamedKey::ArrowRight), named(NamedKey::ArrowRight)]);
    assert_eq!(fixture.drawn(0), ("$ abc".into(), Some(5)));
    fixture.type_keys(&[named(NamedKey::Backspace)]);
    assert_eq!(fixture.drawn(0), ("$ ab".into(), Some(4)));

    // The line editor's echo of all that, as readline draws it.
    fixture.answer();
    fixture.echo("abc\x08\x08X\x1b[Kbc\x08\x08\x08\x1b[Kbc\x08\x08\x1b[C\x1b[C\x08\x1b[K");
    assert_eq!(fixture.real(0), "$ ab");
    assert!(fixture.pane.overlay.is_none());
    fixture.time_passes();
    let tally = fixture.tally();
    assert_eq!((tally.right, tally.wrong), (tally.shown, 0));
}

#[test]
fn going_back_into_the_prompt_is_not_guessed() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_keys(&[named(NamedKey::Backspace)]);
    // Nothing was typed here to go back into, so Backspace's effect is
    // unknown, and the next key waits until it has had time to echo.
    fixture.type_text("a");
    assert!(fixture.pane.overlay.is_none());
    fixture.answer();
    fixture.echo("a");
    fixture.time_passes();
    fixture.type_text("b");
    assert_eq!(fixture.drawn(0), ("$ ab".into(), Some(4)));
}

#[test]
fn keys_of_unknown_effect_pause_guessing_but_keep_what_shows() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("ls");
    fixture.type_keys(&[named(NamedKey::Tab)]);
    fixture.type_text("x");
    // What was guessed before Tab stays; nothing after it is guessed.
    assert_eq!(fixture.drawn(0), ("$ ls".into(), Some(4)));
    fixture.answer();
    fixture.echo("ls -la x");
    assert!(fixture.pane.overlay.is_none());
    assert_eq!(fixture.tally().right, 2);
    fixture.time_passes();
    fixture.type_text("y");
    assert_eq!(fixture.drawn(0), ("$ ls -la xy".into(), Some(11)));
}

#[test]
fn typing_on_after_enter_waits_for_an_echo_at_the_next_prompt() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("ls");
    fixture.type_keys(&[named(NamedKey::Enter)]);
    // Typed before Enter's effect is known: they wait.
    fixture.type_text("pw");
    assert_eq!(fixture.drawn(0), ("$ ls".into(), Some(4)));
    fixture.answer();
    fixture.echo("ls\r\n\x1b]133;C\x07a.txt\r\n\x1b]133;D;0\x07");
    // The shell's next prompt shows Enter has been carried out, but the
    // keys went while the command ran, which might have read them.
    fixture.echo(PROMPT);
    assert_eq!(fixture.drawn(2), ("$".into(), Some(2)));
    fixture.echo("p");
    assert_eq!(fixture.drawn(2), ("$ pw".into(), Some(4)));
    assert_eq!(fixture.underlined(2), [3]);
    fixture.type_text("d");
    assert_eq!(fixture.drawn(2), ("$ pwd".into(), Some(5)));
}

#[test]
fn a_password_typed_ahead_after_sudo_is_never_drawn() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    // `sudo true⏎pw⏎`, typed faster than the round trip.
    fixture.type_text("sudo true");
    fixture.type_keys(&[named(NamedKey::Enter)]);
    fixture.type_text("pw");
    fixture.type_keys(&[named(NamedKey::Enter)]);
    fixture.answer();
    fixture.echo("sudo true\r\n\x1b]133;C\x07Password: ");
    // sudo read the password, echoing nothing, and the shell's next
    // prompt comes, showing the first Enter has been carried out.
    fixture.echo("\r\n\x1b]133;D;0\x07");
    fixture.echo(PROMPT);
    for _ in 0..2 {
        let (rows, _) = fixture.pane.scene(4);
        assert!(rows.iter().all(|row| !row.text().contains("pw")));
        fixture.time_passes();
    }
    assert_eq!(fixture.tally().wrong, 0);
}

#[test]
fn keys_typed_during_a_command_are_never_drawn_without_an_echo() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("make");
    fixture.type_keys(&[named(NamedKey::Enter)]);
    // Some typed before the command's output, some after.
    fixture.type_text("sec");
    fixture.answer();
    fixture.echo("\r\n\x1b]133;C\x07cc -c a.c\r\n");
    fixture.time_passes();
    fixture.type_text("ret");
    fixture.echo("cc -c b.c\r\n");
    fixture.answer();
    fixture.echo("[sudo] pw: ");
    let drawn = |fixture: &Fixture| {
        let (rows, _) = fixture.pane.scene(4);
        rows.iter()
            .any(|row| row.text().contains("sec") || row.text().contains("ret"))
    };
    assert!(!drawn(&fixture));
    fixture.time_passes();
    fixture.echo("\r\n\x1b]133;D;0\x07");
    fixture.echo(PROMPT);
    fixture.time_passes();
    assert!(!drawn(&fixture));
    assert_eq!(fixture.tally().shown, 4, "only \"make\"");
}

#[test]
fn a_shell_that_never_marks_commands_is_not_trusted() {
    // bash before 4.4: prompts marked, but no C, so the shell never says
    // it has left the prompt.
    let mut terminal = Terminal::new(Size::new(20, 4).unwrap(), &Config::default());
    terminal.advance(PROMPT.as_bytes());
    let mut pane = Pane::new(TerminalId(1));
    pane.set_remote(true);
    pane.set_options(&Options {
        predictecho: PredictEcho::Always,
        ..Options::default()
    });
    pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal)));
    let mut fixture = Fixture { terminal, pane };
    fixture.type_text("pw");
    assert_eq!(fixture.drawn(0), ("$".into(), Some(2)));
}

#[test]
fn enter_on_an_empty_line_is_waited_out() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_keys(&[named(NamedKey::Enter)]);
    fixture.answer();
    // No command ran, so the shell still reads a command line, but the
    // new prompt hasn't come yet.
    fixture.echo("\r\n");
    fixture.type_text("pw");
    assert_eq!(fixture.drawn(1), (String::new(), Some(0)));
    // The same block's prompt, drawn again, doesn't show Enter is done...
    fixture.echo(PROMPT);
    assert_eq!(fixture.drawn(1), ("$".into(), Some(2)));
    // ...and once it has had time to be, the keys after it are guessed,
    // but not drawn until an echo shows they went to the shell.
    fixture.time_passes();
    assert_eq!(fixture.drawn(1), ("$".into(), Some(2)));
    fixture.answer();
    fixture.echo("p");
    assert_eq!(fixture.drawn(1), ("$ pw".into(), Some(4)));
}

#[test]
fn guesses_scrolled_away_by_their_command_are_not_wrong() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("ls");
    // The echo of "ls" and Enter, and a lot of output, come at once.
    fixture.type_keys(&[named(NamedKey::Enter)]);
    fixture.answer();
    fixture.echo("ls\r\n\x1b]133;C\x071\r\n2\r\n3\r\n4\r\n5\r\n\x1b]133;D;0\x07");
    fixture.echo(PROMPT);
    fixture.time_passes();
    assert!(fixture.pane.overlay.is_none());
    assert_eq!(fixture.tally().wrong, 0);
}

#[test]
fn without_shell_marks_typing_on_after_enter_waits_for_an_echo() {
    let mut fixture = Fixture::new(PredictEcho::Always, "> ");
    fixture.type_text("ls");
    fixture.type_keys(&[named(NamedKey::Enter)]);
    fixture.answer();
    fixture.echo("ls\r\na.txt\r\n> ");
    fixture.type_text("pw");
    assert_eq!(fixture.drawn(2), (">".into(), Some(2)));
    // Enter has settled, and they're guessed, but tentatively, since
    // nobody knows what the screen showed when they arrived.
    fixture.time_passes();
    assert_eq!(fixture.drawn(2), (">".into(), Some(2)));
    fixture.answer();
    fixture.echo("p");
    assert_eq!(fixture.drawn(2), ("> pw".into(), Some(4)));
    assert_eq!(fixture.underlined(2), [3]);
}

#[test]
fn guesses_waiting_on_enter_that_already_show_are_taken_as_shown() {
    let mut fixture = Fixture::new(PredictEcho::Always, "\x1b]133;A\x07$ true");
    fixture.type_keys(&[named(NamedKey::Enter)]);
    fixture.answer();
    fixture.type_text("pwd");
    fixture.echo("\r\n\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;A\x07$ pw");
    // "pw" showed; "d" is guessed after them, tentatively.
    assert_eq!(fixture.drawn(1), ("$ pw".into(), Some(4)));
    fixture.answer();
    fixture.echo("d");
    assert!(fixture.pane.overlay.is_none());
    fixture.time_passes();
    let tally = fixture.tally();
    assert_eq!((tally.wrong, tally.unseen_wrong), (0, 0));
}

#[test]
fn escape_at_a_prompt_stops_trusting_it() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_keys(&[named(NamedKey::Escape)]);
    fixture.answer();
    fixture.time_passes();
    fixture.type_text("x");
    assert!(fixture.pane.overlay.is_none(), "x may be a vi command");
}

#[test]
fn a_new_snapshot_forgets_the_guesses() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("ls");
    let snapshot = ViewUpdate::Snapshot(View::capture(&fixture.terminal));
    fixture.pane.view_changed(&snapshot);
    assert!(fixture.pane.overlay.is_none());
    fixture.type_text("x");
    assert!(fixture.pane.overlay.is_none(), "waits for the keys sent");
}

#[test]
fn a_new_connection_forgets_everything() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.type_text("l");
    fixture.pane.reconnected();
    assert!(fixture.pane.overlay.is_none());
    assert_eq!(fixture.pane.wakes(), None);
    let snapshot = ViewUpdate::Snapshot(View::capture(&fixture.terminal));
    fixture.pane.view_changed(&snapshot);
    fixture.type_text("s");
    assert_eq!(fixture.drawn(0), ("$ s".into(), Some(3)));
}

#[test]
fn nothing_is_guessed_into_the_last_column() {
    let mut fixture = Fixture::new(PredictEcho::Always, PROMPT);
    fixture.echo("0123456789abcdef");
    fixture.type_text("xy");
    assert_eq!(fixture.drawn(0), ("$ 0123456789abcdefx".into(), Some(19)));
    assert_eq!(fixture.underlined(0), [18]);
}

/// A screen at a shell's prompt.
fn prompt_screen() -> Screen {
    let mut terminal = Terminal::new(Size::new(20, 4).unwrap(), &Config::default());
    terminal.advance(RAN.as_bytes());
    terminal.advance(PROMPT.as_bytes());
    Screen::new(&View::capture(&terminal)).unwrap()
}

/// `predictor` sends `count` inputs, each answered after `rtt`, 200 ms
/// apart, from `now`, and they're all settled. Returns the time after.
fn round_trips(predictor: &mut Predictor, rtt: Duration, count: u32, mut now: Instant) -> Instant {
    let screen = prompt_screen();
    for _ in 0..count {
        let number = predictor.sent(Input::Other, Some(&screen), now);
        now += rtt;
        predictor.answered(number, now);
        now += Duration::from_millis(200);
    }
    now += Duration::from_secs(10);
    predictor.tick(now, Some(&screen));
    now
}

/// A predictor for a daemon on another machine.
fn remote() -> Predictor {
    let mut predictor = Predictor::new();
    predictor.set_remote(true);
    predictor
}

const A: Input = Input::Guess(Guess::Print('a'));

#[test]
fn guesses_are_drawn_only_while_the_link_is_slow() {
    let screen = prompt_screen();
    let mut fast = remote();
    let now = round_trips(&mut fast, Duration::from_millis(5), 20, Instant::now());
    fast.sent(A, Some(&screen), now);
    assert_eq!(fast.shown(&screen), None, "made, but not drawn");

    let mut slow = remote();
    let now = round_trips(&mut slow, Duration::from_millis(300), 3, Instant::now());
    slow.sent(A, Some(&screen), now);
    assert!(slow.shown(&screen).is_some());
}

#[test]
fn a_glitch_draws_guesses_for_a_while_on_a_fast_link() {
    let screen = prompt_screen();
    let mut predictor = remote();
    let mut now = round_trips(&mut predictor, Duration::from_millis(5), 20, Instant::now());
    now = round_trips(&mut predictor, Duration::from_millis(400), 1, now);
    predictor.sent(A, Some(&screen), now);
    assert!(predictor.shown(&screen).is_some(), "drawn after the glitch");

    // Ten quick answers, spaced out, end it.
    predictor.reconnected();
    now = round_trips(&mut predictor, Duration::from_millis(5), 10, now);
    predictor.sent(A, Some(&screen), now);
    assert_eq!(predictor.shown(&screen), None);
}

#[test]
fn auto_is_off_for_a_daemon_on_this_machine_however_slow() {
    let screen = prompt_screen();
    let mut predictor = Predictor::new();
    let now = round_trips(
        &mut predictor,
        Duration::from_millis(500),
        5,
        Instant::now(),
    );
    predictor.sent(A, Some(&screen), now);
    assert_eq!(predictor.shown(&screen), None);
    assert_eq!(predictor.wakes(), None);
}
