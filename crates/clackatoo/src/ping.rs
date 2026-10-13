//! `clackatoo ping`: how long the daemon takes to answer, and how long a
//! key takes to come back.
//!
//! Each round, a request that does nothing is timed, which is what the
//! connection costs, and then a key, from sending it to the view update
//! that shows the program's echo, which is what typing costs. The key goes
//! to a terminal of ping's own, running `cat`, as a window would send it.
//! For a daemon on another machine, each round also says whether it went
//! through ssh or over UDP.

use std::fmt::Display;
use std::time::{Duration, Instant};

use anyhow::Context;
use clackatoo_daemon::{Client, Route};
use clackatoo_protocol::{Event, NewTerminal, Size, TerminalId, View, ViewUpdate};

/// How long to wait for any one answer.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The title the terminal sets once its echo is ready.
const READY: &str = "ready";

/// What to measure.
pub(crate) struct Rounds {
    pub(crate) count: u32,
    /// The pause before each request and each key. Typing leaves gaps
    /// between keys, and some delays only show after one.
    pub(crate) interval: Duration,
}

/// Measures `rounds` on `client`, printing each as it goes and then a
/// summary. `daemon` names the daemon in what's printed.
pub(crate) fn ping(client: &mut Client, daemon: &str, rounds: &Rounds) -> anyhow::Result<()> {
    anyhow::ensure!(rounds.count > 0, "there must be at least one round");
    client.set_read_timeout(Some(TIMEOUT))?;
    // `cat` echoes each key exactly, and gives up on its own if ping is
    // interrupted and never closes the terminal.
    let limit = TIMEOUT.as_secs() + u64::from(rounds.count) * (rounds.interval.as_secs() + 1);
    let script = format!("stty raw -echo; printf '\\033]0;{READY}\\007'; exec timeout {limit} cat");
    let terminal = client.new_terminal(NewTerminal {
        size: Size::new(20, 1).expect("20x1 is a valid size"),
        command: Some(vec!["sh".into(), "-c".into(), script]),
        cwd: None,
        record_to: None,
    })?;
    let result = measure(client, terminal, daemon, rounds);
    let closed = client.close_terminal(terminal);
    result?;
    Ok(closed?)
}

fn measure(
    client: &mut Client,
    terminal: TerminalId,
    daemon: &str,
    rounds: &Rounds,
) -> anyhow::Result<()> {
    client.attach(terminal)?;
    let mut view = next_view(client, terminal, None)?;
    while view.title != READY {
        view = next_view(client, terminal, Some(view))?;
    }

    println!("PING {daemon}: a request, then a key echoed by cat");
    let (mut requests, mut keys) = (Vec::new(), Vec::new());
    for round in 1..=rounds.count {
        std::thread::sleep(rounds.interval);
        let started = Instant::now();
        client.list_terminals()?;
        let request = started.elapsed();

        // The first message after a pause is the slowest, as whatever
        // carries it wakes up: so the key has a pause of its own too.
        std::thread::sleep(rounds.interval);
        // Back to the start of the line, then a letter different from the
        // last, so the echo is the line starting with it.
        let letter = char::from(b'a' + u8::try_from(round % 26)?);
        let started = Instant::now();
        client.send_input(terminal, format!("\r{letter}").as_bytes())?;
        while !first_line(&view).starts_with(letter) {
            view = next_view(client, terminal, Some(view))?;
        }
        let key = started.elapsed();

        println!(
            "round {round}: request {}, key echo {}{}",
            Millis(request),
            Millis(key),
            route(client)
        );
        requests.push(request);
        keys.push(key);
    }
    println!();
    println!("--- {daemon} ping statistics ---");
    println!("request   {}", Summary(&requests));
    println!("key echo  {}", Summary(&keys));
    Ok(())
}

/// How the connection reaches a daemon on another machine, to follow a
/// round's times.
fn route(client: &Client) -> &'static str {
    match client.route() {
        None => "",
        Some(Route::Ssh) => " (through ssh)",
        Some(Route::Udp) => " (over UDP)",
    }
}

/// Waits for the next update of `terminal`'s view, applies it to `view`
/// (or, with none yet, takes it whole), and acknowledges it, as a window
/// does once it has drawn it.
fn next_view(
    client: &mut Client,
    terminal: TerminalId,
    view: Option<View>,
) -> anyhow::Result<View> {
    loop {
        let event = client
            .next_event()
            .context("the daemon stopped sending updates")?;
        let Event::View {
            terminal: updated,
            update,
        } = event
        else {
            continue;
        };
        if updated != terminal {
            continue;
        }
        client.frame_done(terminal)?;
        return match (view, update) {
            (_, ViewUpdate::Snapshot(snapshot)) => Ok(snapshot),
            (Some(mut view), update) => {
                view.apply(update);
                Ok(view)
            }
            (None, ViewUpdate::Patch(_)) => anyhow::bail!("the view started with a patch"),
        };
    }
}

fn first_line(view: &View) -> String {
    view.grid()
        .and_then(|grid| grid.lines.first())
        .map(clackatoo_protocol::Line::text)
        .unwrap_or_default()
}

/// A duration in milliseconds, with as many decimals as matter.
struct Millis(Duration);

impl Display for Millis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let millis = self.0.as_secs_f64() * 1000.0;
        if millis < 1.0 {
            write!(f, "{millis:.3} ms")
        } else if millis < 100.0 {
            write!(f, "{millis:.1} ms")
        } else {
            write!(f, "{millis:.0} ms")
        }
    }
}

/// The minimum, median, 90th percentile and maximum of some durations.
struct Summary<'a>(&'a [Duration]);

impl Display for Summary<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut times = self.0.to_vec();
        times.sort();
        let at = |fraction: f64| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let index = ((times.len() - 1) as f64 * fraction).round() as usize;
            Millis(times[index])
        };
        write!(
            f,
            "min {}, median {}, p90 {}, max {}",
            at(0.0),
            at(0.5),
            at(0.9),
            at(1.0)
        )
    }
}

/// How long a soak waits for a key's echo before counting the connection
/// as dropped: far longer than any blackout it should ride out.
const SOAK_ECHO_LIMIT: Duration = Duration::from_mins(2);

/// How many rounds a soak sums up at a time.
const SOAK_SUMMARY_EVERY: usize = 60;

/// How long a soak's terminal lives, if nothing closes it.
const SOAK_TERMINAL_LIFE: Duration = Duration::from_hours(24);

/// Keeps a connection made by `connect` open until interrupted, typing a
/// key every `interval` and timing its echo, as someone typing into a
/// window would. Logs every round, a summary of each minute or so, and
/// every drop with its reason, then connects again and carries on in the
/// same terminal, as a window does. For finding out how a real network
/// treats a connection.
pub(crate) fn soak(
    mut connect: impl FnMut() -> anyhow::Result<Client>,
    daemon: &str,
    interval: Duration,
) -> anyhow::Result<()> {
    let mut terminal = None;
    let mut drops = 0_u32;
    let mut connected_since = Instant::now();
    loop {
        let started = Instant::now();
        let mut client = match connect() {
            Ok(client) => client,
            Err(err) => {
                log::warn!("couldn't connect to {daemon}: {err:#}");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        log::info!(
            "connected to {daemon} in {}{}",
            Millis(started.elapsed()),
            route(&client)
        );
        let Err(err) = soak_connected(&mut client, &mut terminal, interval);
        drops += 1;
        log::warn!(
            "drop {drops}: lost {daemon} after {:.0?}: {err:#}",
            connected_since.elapsed()
        );
        connected_since = Instant::now();
    }
}

/// Soaks one connection, in `terminal` if it's still running, or else a
/// new one, until the connection fails.
fn soak_connected(
    client: &mut Client,
    terminal: &mut Option<TerminalId>,
    interval: Duration,
) -> anyhow::Result<std::convert::Infallible> {
    client.set_read_timeout(Some(SOAK_ECHO_LIMIT))?;
    let running = client.list_terminals()?;
    let still_running = terminal.filter(|id| running.iter().any(|info| info.id == *id));
    let id = if let Some(id) = still_running {
        id
    } else {
        let script = format!(
            "stty raw -echo; printf '\\033]0;{READY}\\007'; exec timeout {} cat",
            SOAK_TERMINAL_LIFE.as_secs()
        );
        client.new_terminal(NewTerminal {
            size: Size::new(20, 1).expect("20x1 is a valid size"),
            command: Some(vec!["sh".into(), "-c".into(), script]),
            cwd: None,
            record_to: None,
        })?
    };
    *terminal = Some(id);
    client.attach(id)?;
    let mut view = next_view(client, id, None)?;
    while view.title != READY {
        view = next_view(client, id, Some(view))?;
    }
    let mut echoes = Vec::new();
    let mut round = 0_u64;
    // Until the connection fails.
    loop {
        round += 1;
        std::thread::sleep(interval);
        let letter = char::from(b'a' + u8::try_from(round % 26)?);
        let started = Instant::now();
        client.send_input(id, format!("\r{letter}").as_bytes())?;
        while !first_line(&view).starts_with(letter) {
            view = next_view(client, id, Some(view))?;
        }
        let echo = started.elapsed();
        log::info!("round {round}: key echo {}{}", Millis(echo), route(client));
        echoes.push(echo);
        if echoes.len() == SOAK_SUMMARY_EVERY {
            log::info!("last {SOAK_SUMMARY_EVERY} key echoes: {}", Summary(&echoes));
            echoes.clear();
        }
    }
}
