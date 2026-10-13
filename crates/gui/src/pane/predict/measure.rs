//! Measuring guesses at the echo of what's typed over a slow link: a real
//! daemon, running bash, reached through a stand-in for a network that
//! delays and loses what crosses it, driving a real pane.
//!
//! Run by hand, as they take a minute and print numbers rather than check
//! them:
//!
//! ```text
//! cargo test -p clackatoo-gui --lib measure -- --ignored --nocapture
//! ```

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clackatoo_daemon::{AgentConfig, Client, Daemon, DaemonConfig};
use clackatoo_protocol::{ClientMessage, Event, NewTerminal, ServerMessage, Size};
use winit::keyboard::{Key, KeyLocation, NamedKey};

use super::Tally;
use crate::input::{KeyPress, Modifiers};
use crate::options::{Options, PredictEcho};
use crate::pane::Pane;

/// bash marking its prompts and commands, as clackatoo's shell
/// integration makes it.
const BASHRC: &str = r#"PS1='\[\e]133;A\a\]$ \[\e]133;B\a\]'
PROMPT_COMMAND='printf "\033]133;D;%s\007" "$?"'
PS0=$'\e]133;C\a'
"#;

/// How often the window draws a frame.
const FRAME: Duration = Duration::from_millis(16);

/// A network between the window and the daemon, as seen by the messages
/// crossing it: each takes half the round trip, and one lost waits for
/// QUIC to notice and send it again, about one and a half round trips
/// more. Messages in each direction arrive in order, as on one stream.
struct Link {
    rtt: Duration,
    loss: f64,
    /// A xorshift generator's state, for repeatable losses.
    random: u64,
    to_daemon: Queue<ClientMessage>,
    to_window: Queue<ServerMessage>,
}

/// Messages on their way, each with when it arrives.
struct Queue<T> {
    queue: VecDeque<(Instant, T)>,
}

impl<T> Queue<T> {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    /// Puts `message` on its way, to arrive after `delay`, but not before
    /// one sent earlier.
    fn send(&mut self, message: T, now: Instant, delay: Duration) {
        let last = self.queue.back().map_or(now, |&(at, _)| at);
        self.queue.push_back(((now + delay).max(last), message));
    }

    fn arrived(&mut self, now: Instant) -> Option<T> {
        if self.queue.front().is_some_and(|&(at, _)| at <= now) {
            self.queue.pop_front().map(|(_, message)| message)
        } else {
            None
        }
    }
}

impl Link {
    fn new(rtt: Duration, loss: f64) -> Self {
        Self {
            rtt,
            loss,
            random: 0x9E37_79B9_7F4A_7C15,
            to_daemon: Queue::new(),
            to_window: Queue::new(),
        }
    }

    /// How long the next message takes to cross.
    fn delay(&mut self) -> Duration {
        let mut delay = self.rtt / 2;
        while self.chance() < self.loss {
            delay += self.rtt * 3 / 2;
        }
        delay
    }

    /// A number from 0 to 1.
    fn chance(&mut self) -> f64 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        #[allow(clippy::cast_precision_loss)]
        let chance = (self.random >> 11) as f64 / (1u64 << 53) as f64;
        chance
    }
}

/// A key pressed, after waiting.
struct Press {
    after: Duration,
    key: Key,
    /// Whether to time how long the key takes to show, at the end of the
    /// cursor's line.
    timed: bool,
}

/// `text` typed, a key every `after_ms`, and timed.
fn text(after_ms: u64, text: &str) -> Vec<Press> {
    (text.chars())
        .map(|ch| Press {
            after: Duration::from_millis(after_ms),
            key: Key::Character(ch.to_string().into()),
            timed: true,
        })
        .collect()
}

fn named(after_ms: u64, key: NamedKey) -> Press {
    Press {
        after: Duration::from_millis(after_ms),
        key: Key::Named(key),
        timed: false,
    }
}

/// A daemon on a thread of its own, in a directory of its own.
struct TestDaemon {
    dir: PathBuf,
    shutdown: clackatoo_daemon::ShutdownHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TestDaemon {
    fn start() -> Self {
        static STARTED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let started = STARTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = (std::env::temp_dir()).join(format!("predict-{}-{started}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bashrc"), BASHRC).unwrap();
        let config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/bash".into(),
            home: dir.clone(),
            shell_integration: None,
            agents: AgentConfig::default(),
            udp_ports: Some(0..=0),
            languages: None,
            servers: None,
            declined: None,
            server_idle: Duration::ZERO,
            rename_gate: None,
            trash: None,
            mcp: None,
            prompt_wait: clackatoo_daemon::DEFAULT_PROMPT_WAIT,
            probe_wait: clackatoo_daemon::DEFAULT_PROBE_WAIT,
            session_wait: clackatoo_daemon::DEFAULT_SESSION_WAIT,
            park_after: clackatoo_daemon::DEFAULT_PARK_AFTER,
            boards: None,
            browser: None,
            web: None,
        };
        let mut daemon = Daemon::bind(&dir.join("daemon.sock"), config).unwrap();
        let shutdown = daemon.shutdown_handle();
        let thread = std::thread::spawn(move || drop(daemon.run().unwrap()));
        Self {
            dir,
            shutdown,
            thread: Some(thread),
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.shutdown.shutdown();
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What a session measured.
#[derive(Default)]
struct Measured {
    /// For each printable key, how long until the window drew it, and how
    /// long until the daemon's screen showed it.
    keys: Vec<(Duration, Duration)>,
    tally: Tally,
    /// Whether the password typed was ever drawn.
    password_drawn: bool,
}

/// Types `presses` into bash, guessing as `choice` says, over `link`.
#[allow(
    clippy::too_many_lines,
    reason = "one loop, standing in for the window"
)]
fn session(choice: PredictEcho, mut link: Link, presses: &[Press]) -> Measured {
    let daemon = TestDaemon::start();
    let mut client = Client::connect(&daemon.dir.join("daemon.sock")).unwrap();
    let rc = daemon.dir.join("bashrc");
    let terminal = client
        .new_terminal(NewTerminal {
            size: Size::new(80, 24).unwrap(),
            command: Some(vec![
                "/bin/bash".into(),
                "--noprofile".into(),
                "--rcfile".into(),
                rc.to_str().unwrap().into(),
                "-i".into(),
            ]),
            cwd: None,
            record_to: None,
        })
        .unwrap();
    client.attach(terminal).unwrap();
    let (mut sender, mut receiver) = client.split().unwrap();
    let (heard, hear) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(message) = receiver.receive() {
            if heard.send(message).is_err() {
                break;
            }
        }
    });

    let mut pane = Pane::new(terminal);
    pane.set_remote(true);
    pane.set_options(&Options {
        predictecho: choice,
        ..Options::default()
    });

    let mut measured = Measured::default();
    // Keys waiting to show: what the line should end with, when typed,
    // and when drawn.
    let mut waiting: Vec<(String, Instant, Option<Instant>)> = Vec::new();
    let mut typed = String::new();
    let start = Instant::now() + Duration::from_secs(2);
    let mut next_press = start;
    let mut presses = presses.iter().peekable();
    let mut next_frame = Instant::now();
    let mut redraw = false;
    let end_after = Duration::from_secs(8);
    let mut finished: Option<Instant> = None;
    loop {
        let now = Instant::now();
        while let Ok(message) = hear.try_recv() {
            let delay = link.delay();
            link.to_window.send(message, now, delay);
        }
        while let Some(message) = link.to_window.arrived(now) {
            match message {
                ServerMessage::Event(Event::View { update, .. }) => {
                    redraw |= pane.view_changed(&update);
                }
                ServerMessage::Response(response) => redraw |= pane.answered(response),
                _ => {}
            }
        }
        redraw |= pane.tick(now);
        if let Some(press) = presses.peek()
            && now >= next_press + press.after
        {
            let text = match &press.key {
                Key::Character(text) => Some(text.to_string()),
                _ => None,
            };
            pane.key(&KeyPress {
                key: &press.key,
                text: text.as_deref(),
                location: KeyLocation::Standard,
                code: None,
                modifiers: Modifiers::default(),
            });
            redraw = true;
            match &press.key {
                Key::Character(ch) if press.timed => {
                    typed.push_str(ch);
                    waiting.push((typed.clone(), now, None));
                }
                _ => typed.clear(),
            }
            next_press = now;
            presses.next();
        }
        // What the window would draw now, and what the screen shows.
        let (rows, cursor) = pane.scene(24);
        let cursor_line = cursor.map_or(0, |cursor| cursor.line);
        let drawn = rows
            .get(cursor_line)
            .map(|row| row.text())
            .unwrap_or_default();
        measured.password_drawn |= rows.iter().any(|row| row.text().contains("hunter"));
        let real = pane.screen().map(|screen| screen.rows[cursor_line].text());
        for (expected, _, drawn_at) in &mut waiting {
            if drawn_at.is_none() && drawn.starts_with(&format!("$ {expected}")) {
                *drawn_at = Some(now);
            }
        }
        waiting.retain(|(expected, at, drawn_at)| {
            let shown = real
                .as_deref()
                .is_some_and(|real| real.starts_with(&format!("$ {expected}")));
            if shown && let Some(drawn_at) = drawn_at {
                measured.keys.push((*drawn_at - *at, now - *at));
            }
            !shown
        });
        if now >= next_frame {
            next_frame = now + FRAME;
            if std::mem::take(&mut redraw) {
                pane.drawn();
            }
        }
        for message in pane.take_outbox() {
            let delay = link.delay();
            link.to_daemon.send(message, now, delay);
        }
        while let Some(message) = link.to_daemon.arrived(now) {
            sender.send(&message).unwrap();
        }
        if presses.peek().is_none() {
            let finished = *finished.get_or_insert(now);
            if now >= finished + end_after {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    measured.tally = pane.predictor.tally();
    drop(daemon);
    measured
}

/// The middle and 95th percentile of `times`, in milliseconds.
fn spread(mut times: Vec<Duration>) -> String {
    times.sort();
    if times.is_empty() {
        return "none".into();
    }
    let at = |fraction: f64| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let index = ((times.len() - 1) as f64 * fraction).round() as usize;
        times[index].as_secs_f64() * 1000.0
    };
    format!("median {:6.1} ms, 95th {:6.1} ms", at(0.5), at(0.95))
}

#[test]
#[ignore = "a measurement, run by hand"]
fn keystroke_to_visible_over_a_slow_link() {
    // A first command times the round trip, untimed itself.
    let mut presses: Vec<Press> = (text(0, "true").into_iter())
        .map(|press| Press {
            timed: false,
            ..press
        })
        .collect();
    presses.push(named(150, NamedKey::Enter));
    presses.extend(text(1000, "e"));
    presses.extend(text(150, "cho the quick brown fox jumps over the lazy dog"));
    for loss in [0.0, 0.3] {
        for choice in [PredictEcho::Never, PredictEcho::Auto] {
            let link = Link::new(Duration::from_millis(300), loss);
            let measured = session(choice, link, &presses);
            let (drawn, shown): (Vec<_>, Vec<_>) = measured.keys.into_iter().unzip();
            println!(
                "rtt 300 ms, loss {:2.0}%, predictecho={choice:?}: {} keys drawn after {}; \
                 on the daemon's screen after {}; {:?}",
                loss * 100.0,
                drawn.len(),
                spread(drawn),
                spread(shown),
                measured.tally,
            );
        }
    }
}

#[test]
#[ignore = "a measurement, run by hand"]
fn mispredictions_in_a_scripted_session() {
    let mut presses = Vec::new();
    // A command typed with a typo, put right.
    presses.extend(text(1000, "l"));
    presses.extend(text(120, "s -lq"));
    presses.extend([named(200, NamedKey::Backspace)]);
    presses.extend(text(150, "a /"));
    presses.extend([named(200, NamedKey::Enter)]);
    // Moving within what was typed.
    presses.extend(text(1500, "echo hllo"));
    presses.extend([
        named(150, NamedKey::ArrowLeft),
        named(150, NamedKey::ArrowLeft),
        named(150, NamedKey::ArrowLeft),
    ]);
    presses.extend(text(150, "e"));
    presses.extend([named(150, NamedKey::Enter)]);
    // A password prompt, which echoes nothing.
    presses.extend(text(1500, "read -s -p 'Password: ' pw; echo"));
    presses.extend([named(150, NamedKey::Enter)]);
    presses.extend(text(1500, "hunter2"));
    presses.extend([named(150, NamedKey::Enter)]);
    // vim, on the alternate screen.
    presses.extend(text(1500, "vim -u NONE"));
    presses.extend([named(150, NamedKey::Enter)]);
    presses.extend(text(2000, "ihello world"));
    presses.extend([named(150, NamedKey::Escape)]);
    presses.extend(text(300, ":q!"));
    presses.extend([named(150, NamedKey::Enter)]);
    presses.extend(text(2000, "echo done"));
    presses.extend([named(150, NamedKey::Enter)]);
    for loss in [0.0, 0.3] {
        let link = Link::new(Duration::from_millis(300), loss);
        let measured = session(PredictEcho::Auto, link, &presses);
        let tally = measured.tally;
        #[allow(clippy::cast_precision_loss)]
        let rate = tally.wrong as f64 / tally.shown.max(1) as f64 * 100.0;
        println!(
            "rtt 300 ms, loss {:2.0}%: {tally:?}: {rate:.1}% of guesses drawn were wrong; password drawn: {}",
            loss * 100.0,
            measured.password_drawn,
        );
        assert!(!measured.password_drawn);
    }
}
