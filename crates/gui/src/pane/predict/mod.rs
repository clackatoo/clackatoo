//! Predicting the echo of what's typed in a terminal, as mosh does, so
//! that typing over a slow link shows at once rather than a round trip
//! later.
//!
//! The daemon runs the terminal, so a key typed in the window shows only
//! once it has gone to the daemon, the program has echoed it, and the
//! daemon's screen has come back. Over a link with a round trip of 300 ms,
//! that's 300 ms for every key. The window instead guesses what the echo
//! will be, for the keys whose echo is easy to guess: printable
//! characters, Backspace, and Left and Right within what was typed on the
//! line. Guesses are drawn over the window's copy of the screen,
//! underlined, until the daemon's screen shows them. They are never put in
//! the copy itself, so selecting and copying see only what the program
//! drew.
//!
//! Guesses are checked against every screen the daemon sends. A screen
//! shows the program's answer to the keys up to some point, since keys are
//! read in order, so a screen is taken as confirming the first `k` guesses
//! if it shows exactly what they predicted: the cursor where they put it,
//! and the text left of it as they left it. Text right of the cursor isn't
//! compared, as shells draw suggestions there. A screen confirming none
//! proves nothing, as it may simply be older than the keys. A guess is
//! wrong only once the daemon has answered its key, its echo has had time
//! to come back, and a screen sent after that doesn't show it (see
//! `Predictor::due`). A password prompt, say, echoes nothing.
//!
//! As in mosh, guesses come in epochs. A key whose effect can't be guessed
//! (Enter, Tab, Up, a paste) starts a new one, and a new epoch's guesses
//! are tentative: kept and checked, but not drawn until one of them is
//! confirmed. So on a new line, the first key waits for its echo, and the
//! rest show at once if it came back as guessed. A wrong guess that was
//! drawn starts a new epoch too, so after one, guessing is cautious until
//! the program is seen to echo again.
//!
//! The screen may not yet show a key whose effect is unknown, so keys
//! typed after one wait, and are guessed once it has been carried out: on
//! the screen as it is then, picking up where the echo has got to (see
//! `Predictor::anchor`).
//!
//! A shell with shell integration says when it's at its prompt, reading a
//! command line, which it always echoes. There, guesses are drawn at once,
//! unless a guess drawn at that prompt has been wrong, or Esc was typed
//! (the line editor may be in vi mode, where letters are commands). And a
//! prompt shown below where Enter was typed shows that Enter has been
//! carried out, without waiting for time to pass.
//!
//! Guessing happens only for a daemon on another machine, unless the
//! `predictecho` option says `always`: on this machine, the round trip
//! takes microseconds, and the window behaves exactly as without it. Even
//! then, guesses are drawn only while the round trip is over 30 ms, or
//! a key has just taken over 250 ms to be answered, as mosh decides. The
//! round trip is timed from keys sent to the daemon's answers.

use std::collections::VecDeque;
use std::ops::Range;
use std::time::{Duration, Instant};

use clackatoo_protocol::{GridState, ShellState};
use clackatoo_term::mouse::MouseTracking;
use clackatoo_term::{Cell, Row};

use crate::input::KeyPress;
use crate::options::PredictEcho;
use crate::screen::Screen;
use winit::keyboard::{Key, NamedKey};

/// Guesses are drawn once the smoothed round trip is over this...
const SHOW_ABOVE: Duration = Duration::from_millis(30);
/// ...and stop being drawn once it's back under this: mosh's thresholds.
const HIDE_BELOW: Duration = Duration::from_millis(20);
/// A key taking longer than this to be answered is a glitch in the link,
/// and guesses are drawn for a while after, however short the round trip
/// usually is.
const GLITCH: Duration = Duration::from_millis(250);
/// How many quick answers end the while after a glitch...
const GLITCH_REPAIRS: u32 = 10;
/// ...each at least this long after the last that counted.
const GLITCH_REPAIR_SPACING: Duration = Duration::from_millis(150);
/// Time for the program to echo a key, and the daemon to see it, beyond
/// the round trip, before the key's guess is taken as wrong.
const ECHO_GRACE: Duration = Duration::from_millis(100);
/// How long a key's echo is waited for, from its answer, before any round
/// trip has been timed.
const UNTIMED_WAIT: Duration = Duration::from_secs(1);
/// Stands for a cell holding a wide character or a cluster of several
/// characters, which guesses don't move or replace.
const COMPLEX: char = '\0';

/// A key whose echo can be guessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Guess {
    /// A printable character, one cell wide, typed at the cursor.
    Print(char),
    /// Backspace, deleting the character left of the cursor.
    Backspace,
    /// Left, moving the cursor back a cell.
    Left,
    /// Right, moving the cursor on a cell.
    Right,
}

/// An input sent to the program, as guessing sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Input {
    /// A key whose echo can be guessed.
    Guess(Guess),
    /// Esc, which may put a shell's line editor in vi mode.
    Escape,
    /// Anything else: a key whose effect can't be guessed, a paste, a
    /// mouse report.
    Other,
}

impl Input {
    /// What `press` is, typed in insert mode.
    pub(crate) fn of(press: &KeyPress<'_>) -> Self {
        let modifiers = press.modifiers;
        if modifiers.ctrl || modifiers.alt {
            return Self::Other;
        }
        let guess = match press.key {
            Key::Named(NamedKey::Escape) => return Self::Escape,
            Key::Named(NamedKey::Backspace) if !modifiers.shift => Guess::Backspace,
            Key::Named(NamedKey::ArrowLeft) if !modifiers.shift => Guess::Left,
            Key::Named(NamedKey::ArrowRight) if !modifiers.shift => Guess::Right,
            Key::Named(NamedKey::Space) => Guess::Print(' '),
            _ => {
                let mut chars = press.text.unwrap_or_default().chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None)
                        if !ch.is_control() && clackatoo_term::char_width(ch) == Some(1) =>
                    {
                        Guess::Print(ch)
                    }
                    _ => return Self::Other,
                }
            }
        };
        Self::Guess(guess)
    }
}

/// An input sent to the program and not yet settled: confirmed, or taken
/// as wrong, or for one not guessed, given time to have been echoed.
#[derive(Debug)]
struct Sent {
    /// The input's number, as [`Predictor::sent`] gave it.
    number: u64,
    /// What it was guessed to do, or `None` if that can't be guessed.
    guess: Option<Guess>,
    /// The epoch it was guessed in: its guess is drawn once a guess of
    /// that epoch, or a later one, has been confirmed by an echo.
    epoch: u64,
    /// Whether it was typed at a prompt the shell is trusted to echo,
    /// with nothing outstanding but other such keys, so the screen showed
    /// everything sent: its guess is drawn at once. A key typed while
    /// anything else was on its way, such as a password typed after
    /// Enter, is drawn only once an echo confirms its epoch.
    trusted: bool,
    /// When the daemon answered it, saying it was written to the program.
    answered: Option<Instant>,
    /// Whether its guess has been drawn.
    shown: bool,
    /// The cursor's line when it was sent, and the shell's latest block.
    /// A shell showing a new block's prompt on a later line has carried
    /// out an input of unknown effect, such as Enter.
    line: u64,
    block: Option<u32>,
    /// How many screens the daemon has sent since it answered.
    screens: u8,
}

/// What the line being typed on holds, as guesses change it: its cells'
/// characters, where the cursor is, and what part of it was typed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    /// The line's number in the document: history, then the screen.
    number: u64,
    /// The character in each cell, with [`COMPLEX`] for those guesses
    /// leave alone.
    cells: Vec<char>,
    /// The cursor's column.
    cursor: usize,
    /// Where typing began on the line: Backspace and Left go no further
    /// back, since a shell won't go back into its prompt.
    from: usize,
    /// Where the typed text ends: Right goes no further, since a shell
    /// won't go past the end of the line, and fish takes it to accept a
    /// suggestion.
    end: usize,
    /// The cells guesses have changed.
    touched: Option<Range<usize>>,
}

impl Line {
    /// The line the cursor is on in `screen`, if it's a screen guesses can
    /// be made on, with what's known of what was typed on it.
    fn of(screen: &Screen, typed: Option<&Typed>) -> Option<Self> {
        let state = &screen.state;
        if !predictable(state) {
            return None;
        }
        let row = screen.rows.get(usize::from(state.cursor.line))?;
        let number = cursor_line(state);
        let cursor = usize::from(state.cursor.col);
        let (from, end) = match typed {
            Some(typed) if typed.line == number && (typed.from..=typed.end).contains(&cursor) => {
                (typed.from, typed.end)
            }
            _ => (cursor, cursor),
        };
        Some(Self {
            number,
            cells: cells(row),
            cursor,
            from,
            end,
            touched: None,
        })
    }

    /// Carries out `guess`. Returns `false`, changing nothing, if it can't
    /// be guessed here.
    fn apply(&mut self, guess: Guess) -> bool {
        let width = self.cells.len();
        let cursor = self.cursor;
        match guess {
            // A character typed at the end of the line takes the cell at
            // the cursor, and one typed inside the typed text pushes the
            // rest of it on. The last cell is left alone, as the cursor
            // would wait there to wrap, which programs treat differently.
            Guess::Print(ch) => {
                let end = if cursor < self.end {
                    self.end + 1
                } else {
                    cursor + 1
                };
                if end >= width || self.cells[cursor..end].contains(&COMPLEX) {
                    return false;
                }
                self.cells.copy_within(cursor..end - 1, cursor + 1);
                self.cells[cursor] = ch;
                self.cursor += 1;
                self.end = end;
                self.touch(cursor..end);
            }
            Guess::Backspace => {
                if cursor <= self.from || self.cells[cursor - 1..self.end].contains(&COMPLEX) {
                    return false;
                }
                let end = self.end.max(cursor);
                self.cells.copy_within(cursor..end, cursor - 1);
                self.cells[end - 1] = ' ';
                self.cursor -= 1;
                self.end = end - 1;
                self.touch(cursor - 1..end);
            }
            Guess::Left if cursor > self.from => self.cursor -= 1,
            Guess::Right if cursor < self.end => self.cursor += 1,
            Guess::Left | Guess::Right => return false,
        }
        true
    }

    fn touch(&mut self, cells: Range<usize>) {
        self.touched = Some(match self.touched.take() {
            Some(touched) => touched.start.min(cells.start)..touched.end.max(cells.end),
            None => cells,
        });
    }

    /// Whether `screen` shows what this line predicts, as far as can be
    /// known: the cursor where it is, and the cells from `from` to it as
    /// they are. With `moved_on`, inputs of unknown effect came after, so
    /// the cursor may have gone anywhere, and the cells alone are
    /// compared, if there are any.
    fn shown_in(&self, screen: &Screen, from: usize, moved_on: bool) -> bool {
        let state = &screen.state;
        let at_cursor =
            cursor_line(state) == self.number && usize::from(state.cursor.col) == self.cursor;
        if !(at_cursor || moved_on && from < self.cursor) {
            return false;
        }
        let row = (self.number.checked_sub(state.history_end))
            .and_then(|row| screen.rows.get(usize::try_from(row).ok()?));
        let Some(row) = row else {
            return false;
        };
        let real = row.cells();
        (from..self.cursor)
            .all(|col| real.get(col).map(|&cell| cell_char(cell)) == Some(self.cells[col]))
    }

    /// What was typed on the line, to carry over to the next guesses.
    fn typed(&self) -> Typed {
        Typed {
            line: self.number,
            from: self.from,
            end: self.end,
        }
    }
}

/// What part of a line was typed, known from guesses confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Typed {
    line: u64,
    from: usize,
    end: usize,
}

/// The guesses to draw: characters for cells of a line, and where the
/// cursor goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    /// The line's number in the document.
    pub(crate) line: u64,
    /// The cells to draw over, each with its character.
    pub(crate) cells: Vec<(usize, char)>,
    pub(crate) cursor: usize,
}

/// How guessing has gone, for tests and measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    /// Guesses drawn.
    pub(crate) shown: u64,
    /// Guesses drawn and then confirmed.
    pub(crate) right: u64,
    /// Guesses drawn and then found wrong.
    pub(crate) wrong: u64,
    /// Tentative guesses, never drawn, found wrong.
    pub(crate) unseen_wrong: u64,
}

/// The round trip to the daemon, smoothed as TCP smooths it (RFC 6298).
#[derive(Clone, Copy, Debug)]
struct RoundTrip {
    smoothed: Duration,
    variation: Duration,
}

impl RoundTrip {
    fn sample(this: &mut Option<Self>, rtt: Duration) {
        *this = Some(match *this {
            None => Self {
                smoothed: rtt,
                variation: rtt / 2,
            },
            Some(Self {
                smoothed,
                variation,
            }) => Self {
                variation: (variation * 3 + smoothed.abs_diff(rtt)) / 4,
                smoothed: (smoothed * 7 + rtt) / 8,
            },
        });
    }
}

/// Guesses at the echo of what's typed in one terminal.
#[derive(Debug)]
pub(crate) struct Predictor {
    choice: PredictEcho,
    /// Whether the daemon is on another machine.
    remote: bool,
    round_trip: Option<RoundTrip>,
    /// Whether the round trip is long enough to draw guesses.
    slow: bool,
    /// How many quick answers are still to come before guesses stop being
    /// drawn after a glitch.
    glitch: u32,
    /// The last quick answer that counted towards that.
    last_repair: Option<Instant>,
    /// The epoch new guesses go in.
    epoch: u64,
    /// The latest epoch a guess was confirmed in: guesses in it and before
    /// are drawn.
    confirmed: u64,
    /// The number the next input gets.
    next: u64,
    /// When each input not yet answered was sent, oldest first, as the
    /// daemon answers them in order.
    unanswered: VecDeque<(u64, Instant)>,
    /// Inputs not yet settled, oldest first: guesses first, and after
    /// them any whose effect is unknown.
    sent: VecDeque<Sent>,
    /// The line as the screen showed it when the guesses in `sent` were
    /// last confirmed, or first made.
    basis: Option<Line>,
    /// What's known to have been typed on the cursor's line.
    typed: Option<Typed>,
    /// Whether the shell, at its prompt, is trusted to echo.
    trust_prompt: bool,
    /// What the screen last said the shell was doing.
    shell: ShellState,
    tally: Tally,
}

impl Predictor {
    pub(crate) fn new() -> Self {
        Self {
            choice: PredictEcho::Auto,
            remote: false,
            round_trip: None,
            slow: false,
            glitch: 0,
            last_repair: None,
            epoch: 0,
            confirmed: 0,
            next: 0,
            unanswered: VecDeque::new(),
            sent: VecDeque::new(),
            basis: None,
            typed: None,
            trust_prompt: true,
            shell: ShellState::Unmarked,
            tally: Tally::default(),
        }
    }

    /// Takes in what `predictecho` says.
    pub(crate) fn set_choice(&mut self, choice: PredictEcho) {
        if choice != self.choice {
            self.choice = choice;
            self.settle_unknown();
        }
    }

    /// Takes in whether the daemon is on another machine, as a new
    /// connection starts.
    pub(crate) fn set_remote(&mut self, remote: bool) {
        self.remote = remote;
    }

    /// Whether guesses are made at all: with a daemon on another machine,
    /// or always if the option says so.
    fn engaged(&self) -> bool {
        match self.choice {
            PredictEcho::Always => true,
            PredictEcho::Auto => self.remote,
            PredictEcho::Never => false,
        }
    }

    /// Whether guesses are drawn: when made, and the link is slow enough
    /// to need them.
    fn drawing(&self) -> bool {
        match self.choice {
            PredictEcho::Always => true,
            PredictEcho::Auto => self.remote && (self.slow || self.glitch > 0),
            PredictEcho::Never => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn tally(&self) -> Tally {
        self.tally
    }

    /// Starts over on a new connection, on which nothing sent before will
    /// be answered.
    pub(crate) fn reconnected(&mut self) {
        self.unanswered.clear();
        self.sent.clear();
        self.basis = None;
        self.typed = None;
        self.become_tentative();
    }

    /// Records `input`, sent to the program, `screen` showing what it
    /// showed then. Returns the input's number, to give
    /// [`answered`](Self::answered) once the daemon answers it.
    pub(crate) fn sent(&mut self, input: Input, screen: Option<&Screen>, now: Instant) -> u64 {
        let number = self.next;
        self.next += 1;
        if !self.engaged() {
            return number;
        }
        self.unanswered.push_back((number, now));
        if let Some(screen) = screen {
            self.shell = screen.state.shell;
        }
        let at_prompt = self.shell == ShellState::Prompt;
        // A prompt is trusted for a key typed when nothing is outstanding
        // but keys trusted likewise, so the screen shows everything sent.
        let settled = self.sent.iter().all(|sent| sent.trusted);
        let guessed = match input {
            Input::Guess(guess) => Some(guess).filter(|&guess| self.guess(guess, screen)),
            // Esc at a prompt may put the line editor in vi mode, where
            // letters are commands, and don't type themselves.
            Input::Escape => {
                self.trust_prompt &= !at_prompt;
                None
            }
            Input::Other => None,
        };
        if guessed.is_none() {
            self.typed = None;
            self.become_tentative();
        }
        let trusted = guessed.is_some() && settled && at_prompt && self.trust_prompt;
        self.sent.push_back(Sent {
            number,
            guess: guessed,
            epoch: self.epoch,
            trusted,
            answered: None,
            shown: false,
            line: screen.map_or(0, |screen| cursor_line(&screen.state)),
            block: screen.and_then(|screen| screen.state.block),
            screens: 0,
        });
        number
    }

    /// Whether `guess` can be made now, on `screen`.
    fn guess(&mut self, guess: Guess, screen: Option<&Screen>) -> bool {
        // A key typed into a full-screen program, as far as the window
        // knows, isn't guessed, even once that program has gone.
        let Some(screen) = screen.filter(|screen| predictable(&screen.state)) else {
            return false;
        };
        // After an input whose effect is unknown, the screen may not show
        // it yet, so the guess waits, to be made on the screen as it is
        // once the input has had time to be echoed (see `anchor`).
        if self.sent.iter().any(|sent| sent.guess.is_none()) {
            return true;
        }
        if self.basis.is_none() {
            self.basis = Line::of(screen, self.typed.as_ref());
        }
        let Some(mut line) = self.predicted() else {
            return false;
        };
        let applies = line.apply(guess);
        if applies {
            self.typed = Some(line.typed());
        }
        applies
    }

    /// The line as every guess made on it leaves it.
    fn predicted(&self) -> Option<Line> {
        let mut line = self.basis.clone()?;
        for guess in self.sent.iter().map_while(|sent| sent.guess) {
            line.apply(guess);
        }
        Some(line)
    }

    /// Takes the daemon's answer to input `number`: it has been written to
    /// the program.
    pub(crate) fn answered(&mut self, number: u64, now: Instant) {
        while let Some(&(oldest, sent)) = self.unanswered.front() {
            if oldest > number {
                break;
            }
            self.unanswered.pop_front();
            if oldest == number {
                self.timed(now.saturating_duration_since(sent), now);
            }
        }
        for sent in &mut self.sent {
            if sent.number <= number && sent.answered.is_none() {
                sent.answered = Some(now);
            }
        }
    }

    /// Takes in a round trip timed.
    fn timed(&mut self, rtt: Duration, now: Instant) {
        RoundTrip::sample(&mut self.round_trip, rtt);
        let smoothed = self.round_trip.map_or(rtt, |trip| trip.smoothed);
        if smoothed > SHOW_ABOVE {
            self.slow = true;
        } else if smoothed < HIDE_BELOW {
            self.slow = false;
        }
        if rtt > GLITCH {
            self.glitch = GLITCH_REPAIRS;
        } else if self.glitch > 0
            && self
                .last_repair
                .is_none_or(|last| now.saturating_duration_since(last) >= GLITCH_REPAIR_SPACING)
        {
            self.glitch -= 1;
            self.last_repair = Some(now);
        }
    }

    /// How long an answered input's echo is waited for.
    fn echo_wait(&self) -> Duration {
        self.round_trip.map_or(UNTIMED_WAIT, |trip| {
            trip.smoothed + 4 * trip.variation + ECHO_GRACE
        })
    }

    /// Checks the guesses against a new screen from the daemon, confirming
    /// those it shows.
    pub(crate) fn screen_changed(&mut self, screen: &Screen) {
        for sent in self.sent.iter_mut().filter(|sent| sent.answered.is_some()) {
            sent.screens = sent.screens.saturating_add(1);
        }
        self.shell = screen.state.shell;
        if self.shell != ShellState::Prompt {
            self.trust_prompt = true;
        }
        if predictable(&screen.state) {
            self.confirm(screen);
        } else {
            // A full-screen program has taken over: whether the guesses
            // were right no longer shows, so they go, uncounted. Keys
            // waiting for an input of unknown effect went to the program,
            // and aren't to be guessed when it has gone.
            let guessed = self
                .sent
                .iter()
                .take_while(|sent| sent.guess.is_some())
                .count();
            self.sent.drain(..guessed);
            self.drop_guesses_kept();
            self.basis = None;
            self.typed = None;
        }
        // A shell showing a new prompt, below where an input of unknown
        // effect was typed, has carried it out: Enter ran a command, or
        // Ctrl+C gave up on the line. What was typed after it can be
        // guessed now, on the new line. A new line alone isn't enough, as
        // Enter on an empty line shows one before the prompt; that, which
        // runs no command and so starts no block, is waited out instead.
        let state = &screen.state;
        let line = cursor_line(state);
        if self.shell == ShellState::Prompt
            && self.sent.front().is_some_and(|sent| {
                sent.guess.is_none() && sent.line < line && sent.block != state.block
            })
        {
            self.sent.pop_front();
            // Another such input must be seen carried out on its own.
            if let Some(next) = self.sent.front_mut() {
                next.line = next.line.max(line);
                next.block = state.block;
            }
        }
        if self.sent.front().is_some_and(|sent| sent.guess.is_some()) && self.basis.is_none() {
            self.anchor(Some(screen));
        }
    }

    /// Confirms the guesses `screen` shows.
    fn confirm(&mut self, screen: &Screen) {
        let Some(basis) = &self.basis else {
            return;
        };
        let guesses: Vec<Guess> = self.sent.iter().map_while(|sent| sent.guess).collect();
        let moved_on = guesses.len() < self.sent.len();
        // Their line has scrolled away, as the output of a command run by
        // the Enter after them pushed it off the screen. Whether they were
        // right no longer shows.
        if moved_on && basis.number < screen.state.history_end {
            self.sent.drain(..guesses.len());
            self.basis = None;
            self.typed = None;
            return;
        }
        let mut line = basis.clone();
        let mut from = line.cursor;
        let mut confirmed = (line.shown_in(screen, from, false)).then_some((0, line.clone()));
        for (index, &guess) in guesses.iter().enumerate() {
            line.apply(guess);
            from = from.min(line.cursor);
            let last = index + 1 == guesses.len();
            if line.shown_in(screen, from, moved_on && last) {
                confirmed = Some((index + 1, line.clone()));
            }
        }
        let Some((count, line)) = confirmed else {
            return;
        };
        for sent in self.sent.drain(..count) {
            self.confirmed = self.confirmed.max(sent.epoch);
            if sent.shown {
                self.tally.right += 1;
            }
        }
        // Later guesses go on from the screen as it is now. After an input
        // of unknown effect, nothing is known of what was typed.
        let typed = line.typed();
        let unknown = self.sent.front().is_some_and(|sent| sent.guess.is_none());
        self.typed = (!unknown).then_some(typed);
        self.basis = match self.sent.front() {
            Some(sent) if sent.guess.is_some() => Line::of(screen, Some(&typed)),
            _ => None,
        };
    }

    /// With no line to make them on, the guesses left (there shouldn't be
    /// any) are only waited out.
    fn drop_guesses_kept(&mut self) {
        for sent in &mut self.sent {
            sent.guess = None;
        }
    }

    /// Settles the inputs whose time is up at `now`, `screen` showing what
    /// it shows: a guess not confirmed is wrong, and an input not guessed
    /// has been echoed if it ever will be, so the guesses waiting for it
    /// are made.
    pub(crate) fn tick(&mut self, now: Instant, screen: Option<&Screen>) {
        while let Some(oldest) = self.sent.front() {
            if oldest.guess.is_some() && self.basis.is_none() {
                self.anchor(screen);
                continue;
            }
            if self.due(oldest).is_none_or(|due| due > now) {
                break;
            }
            if oldest.guess.is_some() {
                self.wrong();
            }
            self.sent.pop_front();
        }
        if self.sent.is_empty() {
            self.basis = None;
        }
    }

    /// Makes the guesses that waited for inputs of unknown effect, now
    /// settled, on `screen`, tentatively.
    ///
    /// Typing went on meanwhile, so the screen may show some of them
    /// already. Characters typed at the end of the line are easy to spot:
    /// the most of the first ones found just left of the cursor are taken
    /// as shown, and the rest are guessed from there. If that's wrong, the
    /// rest won't be confirmed, and go without having been drawn.
    fn anchor(&mut self, screen: Option<&Screen>) {
        let Some(mut line) = screen.and_then(|screen| Line::of(screen, None)) else {
            self.drop_guesses_kept();
            return;
        };
        let typed: Vec<char> = (self.sent.iter())
            .map_while(|sent| match sent.guess {
                Some(Guess::Print(ch)) => Some(ch),
                _ => None,
            })
            .collect();
        let cursor = line.cursor;
        let shown = (0..=typed.len().min(cursor))
            .rev()
            .find(|&count| line.cells[cursor - count..cursor] == typed[..count])
            .unwrap_or(0);
        self.sent.drain(..shown);
        line.from = cursor - shown;
        self.become_tentative();
        let mut predicted = line.clone();
        let mut made = 0;
        for sent in &mut self.sent {
            match sent.guess {
                Some(guess) if predicted.apply(guess) => {
                    sent.epoch = self.epoch;
                    made += 1;
                }
                _ => break,
            }
        }
        // Those that can't be made here are of unknown effect. Guesses
        // waiting for inputs further on are made in their turn.
        for sent in self.sent.iter_mut().skip(made) {
            if sent.guess.take().is_none() {
                break;
            }
        }
        let unknown_next = self.sent.front().is_some_and(|sent| sent.guess.is_none());
        self.typed = (!unknown_next).then(|| predicted.typed());
        // They were typed while something else was on its way, perhaps a
        // command that asked for a password, so they stay tentative even
        // at a prompt, until an echo confirms them.
        self.basis = (made > 0).then_some(line);
    }

    /// The oldest guess has been found wrong: every guess goes, and those
    /// after it are waited out as inputs of unknown effect.
    fn wrong(&mut self) {
        let shown = self.sent.iter().filter(|sent| sent.shown).count() as u64;
        let guessed = self.sent.iter().filter(|sent| sent.guess.is_some()).count() as u64;
        self.tally.wrong += shown;
        self.tally.unseen_wrong += guessed - shown;
        if shown > 0 && self.shell == ShellState::Prompt {
            self.trust_prompt = false;
        }
        self.drop_guesses_kept();
        self.basis = None;
        self.typed = None;
        self.become_tentative();
    }

    /// Forgets the guesses, as when the screen is replaced, leaving the
    /// inputs to be waited out.
    pub(crate) fn screen_replaced(&mut self) {
        self.settle_unknown();
    }

    fn settle_unknown(&mut self) {
        self.drop_guesses_kept();
        self.basis = None;
        self.typed = None;
        self.become_tentative();
    }

    /// Starts a new epoch, whose guesses aren't drawn until one is
    /// confirmed.
    fn become_tentative(&mut self) {
        self.epoch += 1;
    }

    /// When an input's time is next up, for [`tick`](Self::tick).
    pub(crate) fn wakes(&self) -> Option<Instant> {
        self.due(self.sent.front()?)
    }

    /// When `sent` is settled, if it has been answered: once its echo has
    /// had time to arrive, and the second screen since its answer has.
    ///
    /// The daemon sends a terminal's next screen only once the window has
    /// drawn the last, so the second screen after the answer was taken at
    /// least a round trip after the input went to the program, which has
    /// echoed it by then if it ever will. The first may have been taken
    /// just before, and screens delayed by a lossy link can arrive long
    /// after the answer. If no screens come, as a program echoing nothing
    /// has nothing to send, it's settled after a while anyway.
    fn due(&self, sent: &Sent) -> Option<Instant> {
        let answered = sent.answered?;
        let wait = self.echo_wait();
        Some(answered + if sent.screens >= 2 { wait } else { wait * 3 })
    }

    /// The guesses to draw over `screen`, if any are to be drawn.
    pub(crate) fn shown(&mut self, screen: &Screen) -> Option<Shown> {
        if !self.drawing() || !predictable(&screen.state) {
            return None;
        }
        let mut line = self.basis.clone()?;
        let mut drawn = false;
        for sent in &mut self.sent {
            let drawn_now = sent.trusted || sent.epoch <= self.confirmed;
            let Some(guess) = sent.guess.filter(|_| drawn_now) else {
                break;
            };
            line.apply(guess);
            if !sent.shown {
                sent.shown = true;
                self.tally.shown += 1;
            }
            drawn = true;
        }
        if !drawn {
            return None;
        }
        let state = &screen.state;
        let row = screen
            .rows
            .get(usize::try_from(line.number.checked_sub(state.history_end)?).ok()?)?;
        let real = row.cells();
        let cells = line
            .touched
            .clone()
            .into_iter()
            .flatten()
            .filter(|&col| real.get(col).map(|&cell| cell_char(cell)) != Some(line.cells[col]))
            .map(|col| (col, line.cells[col]))
            .collect();
        Some(Shown {
            line: line.number,
            cells,
            cursor: line.cursor,
        })
    }
}

/// Whether guesses can be made on a screen in `state`: not one a
/// full-screen program draws, which has the alternate screen or the mouse,
/// or hides the cursor.
fn predictable(state: &GridState) -> bool {
    !state.alternate && state.cursor.visible && state.modes.mouse_tracking == MouseTracking::Off
}

/// The cursor's line in the document.
fn cursor_line(state: &GridState) -> u64 {
    state.history_end + u64::from(state.cursor.line)
}

/// The characters of `row`'s cells, with [`COMPLEX`] for those guesses
/// leave alone.
fn cells(row: &Row) -> Vec<char> {
    row.cells().iter().map(|&cell| cell_char(cell)).collect()
}

/// The character of `cell`, or [`COMPLEX`].
fn cell_char(cell: Cell) -> char {
    if cell.is_wide() || cell.is_wide_spacer() || cell.has_grapheme() {
        COMPLEX
    } else {
        cell.ch()
    }
}

#[cfg(test)]
mod measure;
#[cfg(test)]
mod tests;
