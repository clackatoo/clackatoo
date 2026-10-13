//! Remote connections over a bad network.
//!
//! A real daemon, and a client on "another machine" that reaches it as a
//! window does: QUIC inside an ssh stream, moving onto UDP. Both the stream
//! and UDP pass through relays from `clackatoo-netem`, which lose, delay,
//! stall and cut what passes. Each test types into a terminal running
//! `cat`, as someone would, and measures how long each key takes to come
//! back, how often the connection had to be made again, and how long it
//! took to recover from an outage. With `--nocapture`, each prints its
//! numbers on lines starting `NETEM`.
//!
//! The slow ones are ignored: `cargo test -p clackatoo-daemon --test remote
//! -- --ignored --nocapture --test-threads 1` runs them all.

use std::net::{SocketAddr, UdpSocket};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use std::sync::{Arc, Mutex};

use clackatoo_daemon::{
    AgentConfig, Client, ClientError, Daemon, DaemonConfig, Redial, Redialed, Route,
    ShutdownHandle, UdpTarget,
};
use clackatoo_netem::{
    Conditions, Impairment, Latencies, Link, StreamKiller, StreamRelay, UdpRelay,
};
use clackatoo_protocol::{Event, NewTerminal, Size, TerminalId, View, ViewUpdate};

/// A daemon on its own thread, in its own directory, taking UDP on a port
/// chosen for it.
struct TestDaemon {
    dir: tempfile::TempDir,
    udp: SocketAddr,
    shutdown: ShutdownHandle,
    thread: Option<JoinHandle<()>>,
}

impl TestDaemon {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        // A port that was free a moment ago, so the relay knows where to
        // send before the daemon says.
        let port = UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.local_addr())
            .expect("a free port")
            .port();
        let config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: dir.path().to_owned(),
            shell_integration: None,
            agents: AgentConfig::default(),
            udp_ports: Some(port..=port),
            languages: None,
            servers: None,
            declined: None,
            server_idle: Duration::ZERO,
            rename_gate: None,
            trash: None,
            mcp: None,
            prompt_wait: clackatoo_daemon::DEFAULT_PROMPT_WAIT,
            probe_wait: clackatoo_daemon::DEFAULT_PROBE_WAIT,
            park_after: clackatoo_daemon::DEFAULT_PARK_AFTER,
            boards: None,
        };
        let mut daemon = Daemon::bind(&dir.path().join("daemon.sock"), config).expect("bind");
        let shutdown = daemon.shutdown_handle();
        let thread = std::thread::spawn(move || drop(daemon.run().expect("daemon failed")));
        Self {
            dir,
            // The daemon binds IPv6 first, which takes IPv4 too.
            udp: SocketAddr::from(([127, 0, 0, 1], port)),
            shutdown,
            thread: Some(thread),
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("daemon.sock")
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.shutdown.shutdown();
        if let Some(thread) = self.thread.take() {
            thread.join().expect("daemon thread panicked");
        }
    }
}

/// The network between the client and the daemon: one link for UDP, one
/// for the ssh stream. Both cross the same network, so an outage hits
/// both.
struct Network {
    udp: Link,
    ssh: Link,
}

impl Network {
    /// UDP with `udp`, and the ssh stream with the same delays but nothing
    /// lost, as TCP sends it again.
    fn new(udp: Conditions) -> Self {
        let mut ssh = udp.clone();
        for way in [&mut ssh.up, &mut ssh.down] {
            *way = Impairment {
                loss: 0.0,
                in_order: true,
                ..way.clone()
            };
        }
        Self {
            udp: Link::new(udp, 1),
            ssh: Link::new(ssh, 2),
        }
    }

    fn black_out(&self, duration: Duration) {
        self.udp.black_out(duration);
        self.ssh.black_out(duration);
    }
}

/// The letter typed each round: different from the last, so the echo is
/// the line starting with it.
fn letter(round: u64) -> char {
    char::from(b'a' + u8::try_from(round % 26).expect("under 26"))
}

/// How long to wait for any one key's echo before giving up on it.
const ECHO_LIMIT: Duration = Duration::from_secs(150);

/// A client on another machine, typing into a terminal running `cat`. When
/// its ssh stream closes, it opens another, as a window does with ssh.
/// When its connection is lost, it connects again, through a new ssh
/// stream, and goes on in the same terminal, as a window does.
struct Session<'a> {
    daemon: &'a TestDaemon,
    network: &'a Network,
    udp: UdpRelay,
    /// The first ssh stream; the client keeps any it opens after.
    ssh: StreamRelay,
    /// Cuts whichever ssh stream is the newest.
    carriers: Carriers,
    client: Client,
    terminal: TerminalId,
    view: View,
    round: u64,
    /// How many times the connection had to be made again.
    reconnects: u32,
    echoes: Latencies,
}

impl<'a> Session<'a> {
    fn open(daemon: &'a TestDaemon, network: &'a Network) -> Self {
        let udp = UdpRelay::start(daemon.udp, network.udp.clone()).expect("UDP relay");
        let carriers = Carriers::default();
        let (ssh, client) = connect(daemon, network, &udp, &carriers);
        let mut client = client.expect("connect");
        let script = "stty raw -echo; printf '\\033]0;ready\\007'; exec cat";
        let terminal = client
            .new_terminal(NewTerminal {
                size: Size::new(40, 4).unwrap(),
                command: Some(vec!["sh".into(), "-c".into(), script.into()]),
                cwd: None,
                record_to: None,
            })
            .expect("a terminal");
        let view = attach(&mut client, terminal).expect("attach");
        Self {
            daemon,
            network,
            udp,
            ssh,
            carriers,
            client,
            terminal,
            view,
            round: 0,
            reconnects: 0,
            echoes: Latencies::new(),
        }
    }

    /// How the connection goes now.
    fn route(&self) -> Option<Route> {
        self.client.route()
    }

    /// Cuts the ssh stream in use, as when ssh dies.
    fn kill_ssh(&self) {
        self.carriers.kill();
    }

    /// Cuts the ssh stream in use `after` from now.
    fn kill_ssh_after(&self, after: Duration) {
        let carriers = self.carriers.clone();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            carriers.kill();
        });
    }

    /// Types a key, and waits for its echo: how long it took, counting
    /// any reconnecting on the way.
    fn echo(&mut self) -> Duration {
        self.round += 1;
        let letter = letter(self.round);
        let started = Instant::now();
        loop {
            assert!(started.elapsed() < ECHO_LIMIT, "no echo in {ECHO_LIMIT:?}");
            match self.try_echo(letter, started) {
                Ok(()) => break,
                Err(err) => {
                    eprintln!(
                        "NETEM   connection lost after {:?}: {err}",
                        started.elapsed()
                    );
                    self.reconnect();
                }
            }
        }
        let echo = started.elapsed();
        self.echoes.push(echo);
        echo
    }

    fn try_echo(&mut self, letter: char, started: Instant) -> Result<(), ClientError> {
        self.client
            .send_input(self.terminal, format!("\r{letter}").as_bytes())?;
        while !cursor_line(&self.view).starts_with(letter) {
            match next_update(&mut self.client, self.terminal) {
                Ok(Some(update)) => self.view.apply(update),
                Ok(None) => {}
                Err(err) => return Err(err),
            }
            if started.elapsed() > ECHO_LIMIT {
                return Ok(());
            }
        }
        Ok(())
    }

    /// Connects again, through a new ssh stream, until it works.
    fn reconnect(&mut self) {
        loop {
            self.reconnects += 1;
            let (ssh, client) = connect(self.daemon, self.network, &self.udp, &self.carriers);
            self.ssh = ssh;
            match client.and_then(|mut client| {
                let view = attach(&mut client, self.terminal)?;
                Ok((client, view))
            }) {
                Ok((client, view)) => {
                    self.client = client;
                    self.view = view;
                    return;
                }
                Err(err) => {
                    eprintln!("NETEM   reconnecting failed: {err}");
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }

    /// Types a key every `interval` for `duration`.
    fn type_for(&mut self, duration: Duration, interval: Duration) {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            std::thread::sleep(interval);
            self.echo();
        }
    }

    fn report(&self, name: &str) {
        eprintln!(
            "NETEM {name}: echoes {}; reconnects {}; new ssh streams {}; route {:?}; UDP {:?}, \
             ssh {:?}",
            self.echoes,
            self.reconnects,
            self.carriers.redials(),
            self.route(),
            self.network.udp.stats(),
            self.network.ssh.stats(),
        );
    }
}

/// The ssh streams a session has opened: a way to cut the newest, and how
/// many were opened after the first.
#[derive(Clone, Default)]
struct Carriers(Arc<Mutex<(Option<StreamKiller>, u32)>>);

impl Carriers {
    fn opened(&self, relay: &StreamRelay, again: bool) {
        let mut carriers = self.0.lock().unwrap();
        carriers.0 = Some(relay.killer().unwrap());
        carriers.1 += u32::from(again);
    }

    fn kill(&self) {
        if let Some(killer) = &self.0.lock().unwrap().0 {
            killer.kill();
        }
    }

    fn redials(&self) -> u32 {
        self.0.lock().unwrap().1
    }
}

/// Opens an ssh stream through the network, and a client through it that
/// moves onto UDP through `udp`, and opens another ssh stream whenever the
/// one it's using closes.
fn connect(
    daemon: &TestDaemon,
    network: &Network,
    udp: &UdpRelay,
    carriers: &Carriers,
) -> (StreamRelay, Result<Client, ClientError>) {
    let open = |socket: &PathBuf, link: &Link| {
        let stream = UnixStream::connect(socket)?;
        StreamRelay::start(stream, link.clone())
    };
    let (relay, carrier) = open(&daemon.socket(), &network.ssh).expect("relay");
    carriers.opened(&relay, false);
    let redial: Redial = {
        let socket = daemon.socket();
        let link = network.ssh.clone();
        let carriers = carriers.clone();
        Box::new(move || {
            let (relay, carrier) = open(&socket, &link)?;
            carriers.opened(&relay, true);
            Ok(Redialed::keeping(carrier, relay))
        })
    };
    let udp = Some(UdpTarget::Exactly(udp.address()));
    let client = Client::over_quic_redialing(carrier, udp, redial);
    (relay, client)
}

/// Attaches to `terminal`, and waits for its first view, once `cat` is
/// ready. Anything asked of the client from then on waits as long as an
/// echo may take: an outage holds up answers, and isn't a lost connection.
fn attach(client: &mut Client, terminal: TerminalId) -> Result<View, ClientError> {
    client.set_read_timeout(Some(ECHO_LIMIT))?;
    client.attach(terminal)?;
    let started = Instant::now();
    let mut view: Option<View> = None;
    while view.as_ref().is_none_or(|view| view.title != "ready") {
        if started.elapsed() > ECHO_LIMIT {
            return Err(ClientError::Disconnected);
        }
        match (next_update(client, terminal)?, &mut view) {
            (Some(ViewUpdate::Snapshot(snapshot)), _) => view = Some(snapshot),
            (Some(update), Some(view)) => view.apply(update),
            _ => {}
        }
    }
    Ok(view.expect("ready"))
}

/// The next update of `terminal`'s view, acknowledged, or `None` if none
/// came within the read timeout, which only gets that long if the
/// connection is gone without a word.
fn next_update(
    client: &mut Client,
    terminal: TerminalId,
) -> Result<Option<ViewUpdate>, ClientError> {
    loop {
        match client.next_event() {
            Ok(Event::View {
                terminal: updated,
                update,
            }) if updated == terminal => {
                client.frame_done(terminal)?;
                return Ok(Some(update));
            }
            Ok(_) => {}
            Err(ClientError::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(err) => return Err(err),
        }
    }
}

/// The line the cursor is on, where an echo shows.
fn cursor_line(view: &View) -> String {
    view.grid()
        .and_then(|grid| grid.lines.get(usize::from(grid.state.cursor.line)))
        .map(clackatoo_protocol::Line::text)
        .unwrap_or_default()
}

/// Waits until the session goes `route`, typing meanwhile so packets flow.
fn wait_for_route(session: &mut Session<'_>, route: Route, limit: Duration) {
    let started = Instant::now();
    while session.route() != Some(route) {
        assert!(started.elapsed() < limit, "never went {route:?}");
        session.echo();
        std::thread::sleep(Duration::from_millis(50));
    }
}

const TYPING: Duration = Duration::from_millis(200);

#[test]
fn a_clean_network_echoes_at_once_over_udp() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::clean());
    let mut session = Session::open(&daemon, &network);
    wait_for_route(&mut session, Route::Udp, Duration::from_secs(5));
    session.type_for(Duration::from_secs(1), Duration::from_millis(20));
    session.report("clean");
    assert_eq!(session.reconnects, 0);
    assert!(session.echoes.percentile(0.95) < Duration::from_millis(100));
}

#[test]
fn ssh_dying_leaves_a_session_on_udp_working() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::clean());
    let mut session = Session::open(&daemon, &network);
    wait_for_route(&mut session, Route::Udp, Duration::from_secs(5));
    session.kill_ssh();
    session.type_for(Duration::from_secs(2), TYPING);
    session.report("ssh dies");
    assert_eq!(session.reconnects, 0);
    // A new ssh stream opens behind it, in case UDP goes too.
    let started = Instant::now();
    while session.carriers.redials() == 0 {
        assert!(started.elapsed() < Duration::from_secs(5), "no new ssh");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ssh_dying_before_udp_ever_worked_is_ridden_out_through_a_new_ssh_stream() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::clean());
    // UDP never gets through.
    network
        .udp
        .set(Conditions::lossy(1.0, Duration::ZERO, Duration::ZERO));
    let mut session = Session::open(&daemon, &network);
    session.type_for(Duration::from_secs(1), TYPING);
    assert_eq!(session.route(), Some(Route::Ssh));
    session.kill_ssh();
    session.type_for(Duration::from_secs(2), TYPING);
    session.report("ssh dies before UDP worked");
    assert_eq!(session.reconnects, 0);
    assert_eq!(session.carriers.redials(), 1);
}

#[test]
fn a_daemon_that_stops_says_so_at_once() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::clean());
    let mut session = Session::open(&daemon, &network);
    wait_for_route(&mut session, Route::Udp, Duration::from_secs(5));
    let stopped = Instant::now();
    daemon.shutdown.shutdown();
    let err = loop {
        match session.client.list_terminals() {
            Ok(_) => std::thread::sleep(Duration::from_millis(10)),
            Err(err) => break err,
        }
    };
    eprintln!(
        "NETEM daemon stopped: the client knew after {:?}: {err}",
        stopped.elapsed()
    );
    assert!(
        matches!(&err, ClientError::Lost(reason) if reason == "the daemon stopped"),
        "{err:?}"
    );
    assert!(stopped.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_nat_rebinding_moves_the_session_without_a_reconnect() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::clean());
    let mut session = Session::open(&daemon, &network);
    wait_for_route(&mut session, Route::Udp, Duration::from_secs(5));
    session.udp.rebind().unwrap();
    let recovered = session.echo();
    session.type_for(Duration::from_secs(1), TYPING);
    session.report("NAT rebinding");
    eprintln!("NETEM   first echo after rebinding: {recovered:?}");
    assert_eq!(session.reconnects, 0);
}

#[test]
fn a_tunnel_with_an_mtu_of_1280_carries_everything() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::clean().with_mtu(1280));
    let mut session = Session::open(&daemon, &network);
    wait_for_route(&mut session, Route::Udp, Duration::from_secs(5));
    // A screenful at a time, wider than a packet, keeps coming.
    let started = Instant::now();
    session
        .client
        .send_input(session.terminal, &b"x".repeat(4000))
        .unwrap();
    let mut seen = 0;
    while seen < 4000 {
        if let Some(update) = next_update(&mut session.client, session.terminal).unwrap() {
            session.view.apply(update);
        }
        let screen = session.view.grid().unwrap();
        seen = (screen.lines.iter())
            .map(|line| line.text().matches('x').count())
            .sum::<usize>()
            .max(seen);
        assert!(
            started.elapsed() < Duration::from_mins(1),
            "stuck at {seen}"
        );
        if seen >= 40 * 3 {
            break;
        }
    }
    let took = started.elapsed();
    session.type_for(Duration::from_secs(2), TYPING);
    session.report("MTU 1280");
    eprintln!("NETEM   screen of big packets took {took:?}");
    assert_eq!(session.reconnects, 0);
}

/// Blacks the network out for `outage`, with a key typed at its start,
/// and says how long after the network came back the key was echoed.
fn recover_from(outage: Duration, ssh_dies_after: Option<Duration>) -> (Duration, u32) {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::clean());
    let mut session = Session::open(&daemon, &network);
    wait_for_route(&mut session, Route::Udp, Duration::from_secs(5));
    session.type_for(Duration::from_secs(1), TYPING);
    network.black_out(outage);
    let started = Instant::now();
    // ssh gives up on a silent connection (ServerAliveInterval 5 x 3).
    if let Some(after) = ssh_dies_after {
        session.kill_ssh_after(after);
    }
    let echo = session.echo();
    let late = started.elapsed().saturating_sub(outage);
    session.type_for(Duration::from_secs(2), TYPING);
    session.report(&format!("{outage:?} outage"));
    eprintln!(
        "NETEM   key typed at the outage's start: echoed after {echo:?}, {late:?} after the network came back"
    );
    (late, session.reconnects)
}

#[test]
#[ignore = "slow: an outage of 5 seconds"]
fn a_five_second_outage_is_ridden_out() {
    let (late, reconnects) = recover_from(Duration::from_secs(5), None);
    assert_eq!(reconnects, 0);
    assert!(late < Duration::from_secs(3), "recovered {late:?} late");
}

#[test]
#[ignore = "slow: an outage of 30 seconds"]
fn a_thirty_second_outage_that_kills_ssh_is_ridden_out() {
    let (late, reconnects) = recover_from(Duration::from_secs(30), Some(Duration::from_secs(15)));
    assert_eq!(reconnects, 0);
    assert!(late < Duration::from_secs(3), "recovered {late:?} late");
}

#[test]
#[ignore = "slow: a minute of typing over a terrible network"]
fn typing_over_heavy_loss_and_jitter_keeps_the_session() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::lossy(
        0.3,
        Duration::from_millis(300),
        Duration::from_millis(200),
    ));
    let mut session = Session::open(&daemon, &network);
    session.type_for(Duration::from_mins(1), TYPING);
    session.report("30% loss, 300 ms, 200 ms jitter");
    assert_eq!(session.reconnects, 0);
}

#[test]
#[ignore = "slow: a minute of typing through a DERP-like relay"]
fn typing_through_a_relay_that_stalls_keeps_the_session() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::derp());
    let mut session = Session::open(&daemon, &network);
    session.type_for(Duration::from_mins(1), TYPING);
    session.report("DERP-like");
    assert_eq!(session.reconnects, 0);
}

#[test]
#[ignore = "slow: a flood of output down a narrow link"]
fn a_flood_down_a_narrow_link_skips_to_the_latest_screen() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::both(Impairment {
        delay: Duration::from_millis(50),
        rate: Some(1_000_000 / 8),
        ..Impairment::default()
    }));
    let mut session = Session::open(&daemon, &network);
    wait_for_route(&mut session, Route::Udp, Duration::from_secs(5));
    // A terminal floods: the screen the client ends up on must be the
    // last one, soon after the output ends, however much came before.
    let last = "3000000";
    let flood = session
        .client
        .new_terminal(NewTerminal {
            size: Size::new(120, 40).unwrap(),
            command: Some(vec![
                "sh".into(),
                "-c".into(),
                format!("read go; seq 1 {last}; printf '\\033]0;done\\007'; exec cat"),
            ]),
            cwd: None,
            record_to: None,
        })
        .unwrap();
    let mut view = attach_any(&mut session.client, flood);
    let started = Instant::now();
    session.client.send_input(flood, b"\r").unwrap();
    let mut updates = 0_u32;
    // When the output ended, as the daemon saw it.
    let mut output_done = None;
    while !first_line_containing(&view, last) {
        assert!(
            started.elapsed() < Duration::from_mins(2),
            "never caught up"
        );
        if output_done.is_none()
            && (session.client.list_terminals().unwrap().iter())
                .any(|terminal| terminal.id == flood && terminal.title == "done")
        {
            output_done = Some(started.elapsed());
        }
        if let Some(update) = next_update(&mut session.client, flood).unwrap() {
            updates += 1;
            view.apply(update);
        }
    }
    let caught_up = started.elapsed();
    let done_at = output_done.unwrap_or(caught_up);
    // Typing in the other terminal is still quick.
    session.type_for(Duration::from_secs(2), TYPING);
    session.report("flood at 1 Mbit/s");
    eprintln!(
        "NETEM   flood: output done at {done_at:?}, caught up at {caught_up:?}, {:?} later, in \
         {updates} updates",
        caught_up.saturating_sub(done_at)
    );
    assert!(
        caught_up.saturating_sub(done_at) < Duration::from_secs(2),
        "fell behind"
    );
}

/// Attaches to `terminal` and takes its first view, whatever it shows.
fn attach_any(client: &mut Client, terminal: TerminalId) -> View {
    client.attach(terminal).unwrap();
    loop {
        if let Some(ViewUpdate::Snapshot(view)) = next_update(client, terminal).unwrap() {
            return view;
        }
    }
}

fn first_line_containing(view: &View, text: &str) -> bool {
    view.grid()
        .is_some_and(|grid| grid.lines.iter().any(|line| line.text().trim() == text))
}
