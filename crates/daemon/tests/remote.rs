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
use clackatoo_protocol::{
    BufferId, BufferUpdate, Change, Edit, Event, NewTerminal, OpenFile, PaneTerminal, Request,
    Response, Resume, Size, TerminalId, View, ViewUpdate,
};
use clackatoo_text::transform;

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
            lsp_trace: None,
            server_idle: Duration::ZERO,
            rename_gate: None,
            trash: None,
            mcp: None,
            prompt_wait: clackatoo_daemon::DEFAULT_PROMPT_WAIT,
            probe_wait: clackatoo_daemon::DEFAULT_PROBE_WAIT,
            session_wait: clackatoo_daemon::DEFAULT_SESSION_WAIT,
            gone_wait: clackatoo_daemon::DEFAULT_GONE_WAIT,
            park_after: clackatoo_daemon::DEFAULT_PARK_AFTER,
            fit_lasts: clackatoo_daemon::DEFAULT_FIT_LASTS,
            boards: None,
            browser: None,
            web: None,
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

/// How long to wait for a connection to move onto UDP before giving up:
/// on a clean network it takes a few probes, but on a loaded machine the
/// threads passing them may wait seconds to run.
const ON_UDP_LIMIT: Duration = Duration::from_secs(30);

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
    wait_for_route(&mut session, Route::Udp, ON_UDP_LIMIT);
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
    wait_for_route(&mut session, Route::Udp, ON_UDP_LIMIT);
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
    wait_for_route(&mut session, Route::Udp, ON_UDP_LIMIT);
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
fn what_is_sent_once_the_daemon_has_stopped_says_so_too() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::clean());
    let mut session = Session::open(&daemon, &network);
    wait_for_route(&mut session, Route::Udp, ON_UDP_LIMIT);
    daemon.shutdown.shutdown();
    // Only sending: the end is heard of as the stream to the connection's
    // thread closes, once the thread has heard the daemon close it.
    let deadline = Instant::now() + Duration::from_secs(10);
    let err = loop {
        match session.client.frame_done(session.terminal) {
            Ok(()) => {
                assert!(Instant::now() < deadline, "never heard of the stop");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => break err,
        }
    };
    assert!(
        matches!(&err, ClientError::Lost(reason) if reason == "the daemon stopped"),
        "{err:?}"
    );
}

#[test]
fn a_nat_rebinding_moves_the_session_without_a_reconnect() {
    let daemon = TestDaemon::start();
    let network = Network::new(Conditions::clean());
    let mut session = Session::open(&daemon, &network);
    wait_for_route(&mut session, Route::Udp, ON_UDP_LIMIT);
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
    wait_for_route(&mut session, Route::Udp, ON_UDP_LIMIT);
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
    wait_for_route(&mut session, Route::Udp, ON_UDP_LIMIT);
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
    wait_for_route(&mut session, Route::Udp, ON_UDP_LIMIT);
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

#[test]
#[ignore = "slow: typing while a forwarded port downloads down a narrow link"]
fn typing_while_a_forwarded_port_downloads_down_a_narrow_link_stays_quick() {
    let measured = typing_beside_forwards("download", narrow_link(), &[Transfer::Download]);
    measured.assert_echoes(Duration::from_millis(30));
    measured.assert_rates(NARROW_RATE * 7 / 10, NARROW_RATE / 2);
}

#[test]
#[ignore = "slow: typing while a forwarded port uploads up a narrow link"]
fn typing_while_a_forwarded_port_uploads_up_a_narrow_link_stays_quick() {
    let measured = typing_beside_forwards("upload", narrow_link(), &[Transfer::Upload]);
    measured.assert_echoes(Duration::from_millis(30));
    measured.assert_rates(NARROW_RATE * 7 / 10, NARROW_RATE / 2);
}

#[test]
#[ignore = "slow: typing while forwarded ports download and upload at once"]
fn typing_while_forwarded_ports_download_and_upload_at_once_stays_quick() {
    // A download's acknowledgements wait in the upload's queue, and the
    // upload's in the download's: each end measures with the other's
    // queue in place, and keeps only its own short.
    let both = [Transfer::Download, Transfer::Upload];
    let measured = typing_beside_forwards("download and upload", narrow_link(), &both);
    // A key goes up through the upload's queue, and its echo comes down
    // through the download's: each end keeps its own to the target.
    measured.assert_echoes(Duration::from_millis(50));
    measured.assert_rates(NARROW_RATE * 2 / 5, NARROW_RATE * 2 / 5);
}

#[test]
#[ignore = "slow: a forwarded upload starting while a download goes"]
fn an_upload_starting_late_leaves_a_download_its_own_link() {
    // The download has the link to itself for 20 s, then an upload
    // starts: once each end has measured with the other's queue in
    // place, each direction has most of its own link again.
    let late = Setup {
        late: Duration::from_secs(20),
        settle: Duration::from_secs(15),
        ..Setup::default()
    };
    let both = [Transfer::Download, Transfer::Upload];
    let measured = typing_beside("upload starting late", narrow_link(), &both, &late);
    measured.assert_echoes(Duration::from_millis(50));
    measured.assert_rates(NARROW_RATE / 2, NARROW_RATE / 2);
}

#[test]
#[ignore = "slow: forwarded ports downloading and uploading over a lopsided link"]
fn a_lopsided_link_gives_each_way_its_own_speed() {
    // 20 Mbit/s down and 2 up, as many home links are: the upload mustn't
    // hold the download to its speed. The download's acknowledgements
    // take a fifth of the way up, and a packet takes 5 ms to go up, so
    // the round trip the download goes by jumps by that as the upload's
    // queue comes and goes: it went at 0.5 to 2 MB/s.
    let mut conditions = narrow_link();
    conditions.up.rate = Some(NARROW_RATE / 10);
    let both = [Transfer::Download, Transfer::Upload];
    let measured = typing_beside("lopsided", conditions, &both, &Setup::default());
    measured.assert_echoes(Duration::from_millis(60));
    let up = NARROW_RATE / 10;
    for rates in [&measured.quiet_rates, &measured.busy_rates] {
        assert!(
            rates[0] > up * 3 / 2 && rates[1] > up * 2 / 5,
            "down {} B/s, up {} B/s",
            rates[0],
            rates[1]
        );
    }
}

#[test]
#[ignore = "slow: a forwarded port downloading down a narrow link, unpaced"]
fn a_forwarded_download_told_not_to_pace_takes_the_whole_link() {
    // `:set noforwardpace`: the download goes as fast as it can, taking
    // the link whether keys wait behind it or not.
    let unpaced = Setup {
        paced: false,
        ..Setup::default()
    };
    let measured = typing_beside(
        "unpaced download",
        narrow_link(),
        &[Transfer::Download],
        &unpaced,
    );
    measured.assert_rates(NARROW_RATE * 85 / 100, NARROW_RATE * 85 / 100);
}

#[test]
#[ignore = "slow: a forwarded port downloading down a jittery link"]
fn a_forwarded_download_down_a_jittery_link_still_gets_through() {
    // The narrow link, with each packet held up 0 to 60 ms more, in
    // order, as bad Wi-Fi does: a delay that rises and falls with
    // nothing queued mustn't be taken for a queue, starving the download.
    let mut conditions = narrow_link();
    for way in [&mut conditions.up, &mut conditions.down] {
        way.jitter = Duration::from_millis(60);
        way.in_order = true;
    }
    let measured = typing_beside_forwards("jittery download", conditions, &[Transfer::Download]);
    // Busy, the link alone holds each packet up about 30 ms more than
    // when keys go one at a time, as each waits for the slowest before
    // it: that's not the download's queue, and keys wait in it whatever
    // goes.
    measured.assert_echoes(Duration::from_millis(60));
    // Unpaced, the download went at about 1.7 MB/s either way, as the
    // link held up in order carries a third less. Paced: busier, the link
    // holds packets up longer, which reads as a queue, so each slowdown
    // finds a little more of that and allows a little more. It went at
    // 0.25 to 0.35 MB/s in its first six seconds, with nobody typing, as
    // it climbed, and at 1.3 to 1.95 MB/s by the time typing started.
    measured.assert_rates(NARROW_RATE / 20, NARROW_RATE / 5);
}

/// 20 Mbit/s with 20 ms each way, and a router's queue of 100 ms.
fn narrow_link() -> Conditions {
    Conditions::both(Impairment {
        delay: Duration::from_millis(20),
        rate: Some(NARROW_RATE),
        queue: Duration::from_millis(100),
        ..Impairment::default()
    })
}

/// How fast [`narrow_link`] goes each way, in bytes a second.
const NARROW_RATE: u64 = 20_000_000 / 8;

/// Which way a forwarded connection's bytes go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transfer {
    /// From a server on the daemon's machine to a browser on the window's.
    Download,
    /// From the browser to the server.
    Upload,
}

/// What [`typing_beside_forwards`] measured.
struct Beside {
    /// The echoes before anything was forwarded.
    before: Latencies,
    /// How fast each transfer went while nobody typed, in bytes a second.
    quiet_rates: Vec<u64>,
    /// The first echo once typing started again, after that pause.
    first: Duration,
    /// The echoes while the transfers went on.
    during: Latencies,
    /// How fast each went meanwhile.
    busy_rates: Vec<u64>,
}

impl Beside {
    /// Asserts that keys were held up no more than `most` by the
    /// transfers, at the middle, and not much more at the 95th percentile
    /// or straight after a pause.
    fn assert_echoes(&self, most: Duration) {
        let before = self.before.percentile(0.5);
        let during = self.during.percentile(0.5);
        assert!(
            during < before + most,
            "echoes went from {before:?} to {during:?}"
        );
        let slow = self.during.percentile(0.95);
        let slow_before = self.before.percentile(0.95);
        assert!(
            slow < slow_before + most + most / 2,
            "the 95th percentile went from {slow_before:?} to {slow:?}"
        );
        assert!(
            self.first < before + most + Duration::from_millis(10),
            "the first echo after a pause took {:?}",
            self.first
        );
    }

    /// Asserts that each transfer went faster than `quiet` bytes a second
    /// with nobody typing, and `busy` while typing.
    fn assert_rates(&self, quiet: u64, busy: u64) {
        for (&alone, &typing) in self.quiet_rates.iter().zip(&self.busy_rates) {
            assert!(
                alone > quiet && typing > busy,
                "a transfer went at only {alone} B/s with nobody typing, {typing} B/s while \
                 typing"
            );
        }
    }
}

/// Types into a terminal over a network of `conditions`, then forwards a
/// port for each of `transfers` and moves bytes through each as fast as
/// they go, the way it says: first with nobody typing, then while typing
/// goes on.
fn typing_beside_forwards(name: &str, conditions: Conditions, transfers: &[Transfer]) -> Beside {
    typing_beside(name, conditions, transfers, &Setup::default())
}

/// How [`typing_beside`] goes, beyond what it moves.
struct Setup {
    /// Whether the forwards go as background traffic.
    paced: bool,
    /// How long after the first transfer the others start.
    late: Duration,
    /// How long after the last starts it's measured.
    settle: Duration,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            paced: true,
            late: Duration::ZERO,
            settle: Duration::from_secs(1),
        }
    }
}

/// [`typing_beside_forwards`], as `setup` says.
fn typing_beside(
    name: &str,
    conditions: Conditions,
    transfers: &[Transfer],
    setup: &Setup,
) -> Beside {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicU64, Ordering};

    let daemon = TestDaemon::start();
    let network = Network::new(conditions);
    let mut session = Session::open(&daemon, &network);
    wait_for_route(&mut session, Route::Udp, ON_UDP_LIMIT);
    session.type_for(Duration::from_secs(5), TYPING);
    session.report(&format!("{name}, before"));
    let before = std::mem::take(&mut session.echoes);

    // For each, a server on the daemon's machine and a browser on the
    // window's, through a forward of its own: one sends as fast as it
    // can, and the other counts what it reads.
    let send = |mut stream: TcpStream| {
        let block = vec![b'x'; 64 * 1024];
        while stream.write_all(&block).is_ok() {}
    };
    let count = |mut stream: TcpStream, moved: &AtomicU64| {
        let mut buffer = vec![0; 64 * 1024];
        while let Ok(n @ 1..) = stream.read(&mut buffer) {
            moved.fetch_add(n as u64, Ordering::Relaxed);
        }
    };
    let mut forwards = clackatoo_daemon::forward::Forwards::new(|| {});
    forwards.set_paced(setup.paced);
    assert!(forwards.attach(session.client.forwarder().unwrap()));
    let mut moved = Vec::new();
    for (at, &transfer) in transfers.iter().enumerate() {
        if at == 1 {
            std::thread::sleep(setup.late);
        }
        let tally = Arc::new(AtomicU64::new(0));
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let counted = Arc::clone(&tally);
        std::thread::spawn(move || {
            if let Ok((stream, _)) = server.accept() {
                match transfer {
                    Transfer::Download => send(stream),
                    Transfer::Upload => count(stream, &counted),
                }
            }
        });
        let spec = clackatoo_daemon::forward::Spec {
            local: 0,
            host: "localhost".into(),
            port,
        };
        let local = forwards.add(spec).unwrap().local;
        let browser = TcpStream::connect(("127.0.0.1", local)).unwrap();
        let counted = Arc::clone(&tally);
        std::thread::spawn(move || match transfer {
            Transfer::Download => count(browser, &counted),
            Transfer::Upload => send(browser),
        });
        moved.push(tally);
    }
    // How much each had moved, and when.
    let mark = || {
        let counts: Vec<u64> = moved.iter().map(|c| c.load(Ordering::Relaxed)).collect();
        (counts, Instant::now())
    };
    // How fast each went since a mark.
    let rates_since = |(from, started): (Vec<u64>, Instant)| -> Vec<u64> {
        let millis = u64::try_from(started.elapsed().as_millis()).unwrap();
        (moved.iter().zip(from))
            .map(|(c, from)| (c.load(Ordering::Relaxed) - from) * 1000 / millis)
            .collect()
    };

    // Under way, then alone.
    std::thread::sleep(setup.settle);
    let started = mark();
    std::thread::sleep(Duration::from_secs(5));
    let quiet_rates = rates_since(started);
    let first = session.echo();
    session.echoes = Latencies::new();
    let started = mark();
    session.type_for(Duration::from_secs(10), TYPING);
    let busy_rates = rates_since(started);
    session.report(&format!("{name}, while typing"));
    eprintln!(
        "NETEM   {name}: {quiet_rates:?} B/s with nobody typing, {busy_rates:?} B/s while \
         typing; the first echo after the pause took {first:?}"
    );
    Beside {
        before,
        quiet_rates,
        first,
        during: std::mem::take(&mut session.echoes),
        busy_rates,
    }
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

/// jack's laptop on a caravan site's Wi-Fi, through a Tailscale tunnel: a
/// round trip of 90 to 230 ms, 2% lost, and nothing over 1280 bytes. The
/// bursts of loss and the outages are made by [`Caravan`].
fn caravan() -> Conditions {
    Conditions::lossy(0.02, Duration::from_millis(160), Duration::from_millis(140)).with_mtu(1280)
}

/// Makes the network worse now and then, on a thread, until dropped:
/// every 3 to 8 s, up to a second of losing 40%; every 45 to 75 s, an
/// outage of 5 to 10 s.
struct Caravan {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Caravan {
    fn start(network: &Network, seed: u64) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (udp, ssh) = (network.udp.clone(), network.ssh.clone());
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut state = seed | 1;
            let mut between = move |range: std::ops::Range<u64>| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                Duration::from_millis(range.start + state % (range.end - range.start))
            };
            let mut next_outage = Instant::now() + between(45_000..75_000);
            let bursty = {
                let mut conditions = caravan();
                conditions.up.loss = 0.4;
                conditions.down.loss = 0.4;
                conditions
            };
            let pause = |duration: Duration| {
                let until = Instant::now() + duration;
                while Instant::now() < until {
                    if stopped.load(std::sync::atomic::Ordering::Relaxed) {
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                true
            };
            loop {
                if !pause(between(3_000..8_000)) {
                    return;
                }
                if Instant::now() > next_outage {
                    let outage = between(5_000..10_000);
                    eprintln!("NETEM   an outage of {outage:?}");
                    udp.black_out(outage);
                    ssh.black_out(outage);
                    next_outage = Instant::now() + between(45_000..75_000);
                    continue;
                }
                udp.set(bursty.clone());
                let burst = pause(between(300..1_000));
                udp.set(caravan());
                if !burst {
                    return;
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Caravan {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("the caravan's thread panicked");
        }
    }
}

/// How long a caravan soak types for: `CLACKATOO_SOAK_SECONDS`, or two
/// minutes.
fn soak_length() -> Duration {
    let seconds = std::env::var("CLACKATOO_SOAK_SECONDS")
        .ok()
        .and_then(|seconds| seconds.parse().ok())
        .unwrap_or(120);
    Duration::from_secs(seconds)
}

#[test]
#[ignore = "slow: minutes of typing over a caravan's Wi-Fi"]
fn typing_over_a_caravan_link_keeps_the_session() {
    let daemon = TestDaemon::start();
    let network = Network::new(caravan());
    let caravan = Caravan::start(&network, 7);
    let mut session = Session::open(&daemon, &network);
    session.type_for(soak_length(), TYPING);
    drop(caravan);
    session.report("caravan");
    // Outages of 10 s are well inside the 2 minutes a connection waits.
    assert_eq!(session.reconnects, 0);
}

/// A window's copy of a buffer, kept as a window keeps it: the text, the
/// version of the daemon's text it's of, and the one change it has sent
/// and not heard back about, already made to the copy.
struct Copy {
    buffer: BufferId,
    window: u64,
    daemon: u64,
    text: String,
    version: u64,
    seq: u64,
    sent: Option<(u64, Edit)>,
}

impl Copy {
    /// Attaches to `buffer` as `window`, starting from its snapshot.
    fn attach(client: &mut Client, buffer: BufferId, window: u64) -> Result<Self, ClientError> {
        client.attach_buffer_as(buffer, window)?;
        loop {
            if let BufferUpdate::Snapshot(snapshot) = next_buffer_update(client, buffer)? {
                return Ok(Self {
                    buffer,
                    window,
                    daemon: snapshot.daemon,
                    text: snapshot.text,
                    version: snapshot.version,
                    seq: 0,
                    sent: None,
                });
            }
        }
    }

    /// Sends `edit`, already made to the copy, numbered afresh.
    fn send(&mut self, client: &mut Client, edit: Edit) -> Result<(), ClientError> {
        self.seq += 1;
        self.sent = Some((self.seq, edit.clone()));
        let change = Change::Edit {
            edit,
            base: self.version,
            step: true,
        };
        client.edit(self.buffer, self.seq, change)
    }

    /// Types `text` at byte `at` of the copy, and sends it.
    fn type_at(&mut self, client: &mut Client, at: usize, text: &str) -> Result<(), ClientError> {
        assert!(self.sent.is_none(), "one change at a time");
        let edit = Edit::replace(self.text.len(), at, "", text);
        self.text = edit.apply_to(&self.text).expect("an edit of the copy");
        self.send(client, edit)
    }

    /// Types `text` at byte `at` of the copy as the connection is lost:
    /// it's numbered as sent, but never arrives.
    fn type_lost(&mut self, at: usize, text: &str) {
        assert!(self.sent.is_none(), "one change at a time");
        let edit = Edit::replace(self.text.len(), at, "", text);
        self.text = edit.apply_to(&self.text).expect("an edit of the copy");
        self.seq += 1;
        self.sent = Some((self.seq, edit));
    }

    /// Takes in what the daemon said: its own change made, or someone
    /// else's, which goes past the change it sent.
    fn hear(&mut self, update: BufferUpdate) {
        match update {
            BufferUpdate::Acked { version, seq } => {
                assert_eq!(self.sent.as_ref().map(|(sent, _)| *sent), Some(seq));
                assert_eq!(version, self.version + 1, "a version skipped");
                self.sent = None;
                self.version = version;
            }
            BufferUpdate::Edited { version, edit, .. } => self.edited(&edit, version),
            BufferUpdate::Snapshot(_) | BufferUpdate::Resumed { .. } => {
                panic!("only sent on attaching")
            }
            BufferUpdate::NoChange(seq) => panic!("change {seq} changed nothing"),
            _ => {}
        }
    }

    fn edited(&mut self, edit: &Edit, version: u64) {
        assert_eq!(version, self.version + 1, "a version skipped");
        let mut theirs = edit.clone();
        if let Some((_, mine)) = &mut self.sent {
            (*mine, theirs) = transform(mine, &theirs).expect("edits of the same text");
        }
        self.text = theirs.apply_to(&self.text).expect("an edit of the copy");
        self.version = version;
    }

    /// Attaches again, on a new connection, catching up as a window does:
    /// the edits since what it knew, its own change among them if it was
    /// made, and sends its change again if it never arrived.
    fn resume(&mut self, client: &mut Client) -> Result<Resumed, ClientError> {
        let resume = Some(Resume {
            daemon: self.daemon,
            version: self.version,
        });
        let request = Request::AttachBuffer {
            buffer: self.buffer,
            window: self.window,
            resume,
        };
        match client.request(&request)? {
            Response::Done => {}
            other => panic!("attaching again: {other:?}"),
        }
        let sent = self.sent.as_ref().map(|(seq, _)| *seq);
        let arrived = |applied: Option<u64>| sent.is_some_and(|sent| applied >= Some(sent));
        loop {
            match next_buffer_update(client, self.buffer)? {
                BufferUpdate::Resumed {
                    edits,
                    applied,
                    landed,
                    ..
                } => {
                    let arrived = arrived(applied);
                    for edit in edits {
                        if arrived && landed == Some(self.version + 1) {
                            // Its own, already in the copy.
                            self.sent = None;
                            self.version += 1;
                        } else {
                            self.edited(&edit, self.version + 1);
                        }
                    }
                    assert!(
                        !arrived || self.sent.is_none(),
                        "a change that couldn't be made"
                    );
                    if let Some((_, edit)) = self.sent.take() {
                        self.send(client, edit)?;
                        return Ok(Resumed::Again);
                    }
                    return Ok(Resumed::Caught);
                }
                BufferUpdate::Snapshot(_) => panic!("the daemon kept its edits"),
                _ => {}
            }
        }
    }
}

/// How a copy attaching again caught up.
#[derive(Debug, PartialEq, Eq)]
enum Resumed {
    /// From the edits since, its change among them or none sent.
    Caught,
    /// From the edits since, and its change, which never arrived, sent
    /// again.
    Again,
}

/// The next update about `buffer`, or an error if the connection failed.
fn next_buffer_update(client: &mut Client, buffer: BufferId) -> Result<BufferUpdate, ClientError> {
    loop {
        if let Event::Buffer {
            buffer: updated,
            update,
        } = client.next_event()?
            && updated == buffer
        {
            return Ok(update);
        }
    }
}

/// The buffer a new workspace's pane shows a file in.
fn open_buffer(client: &mut Client, path: &std::path::Path) -> BufferId {
    client.watch_workspaces().expect("watch");
    let file = PaneTerminal::File(OpenFile {
        path: path.display().to_string(),
        cwd: None,
    });
    let pane = client.new_workspace(None, file).expect("a workspace");
    loop {
        if let Ok(Event::Workspaces(workspaces)) = client.next_event()
            && let Some((_, _, pane)) = workspaces.find_pane(pane)
        {
            return pane.content.buffer().expect("a buffer");
        }
    }
}

/// Types an `L` at the start of `buffer` every 700 ms, on a thread, from a
/// window on the daemon's own machine, until `stop`. The thread ends with
/// its client, its copy and how many it typed.
fn type_at_the_desk(
    mut local: Client,
    buffer: BufferId,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<(Client, Copy, usize)> {
    std::thread::spawn(move || {
        local
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("a timeout");
        let mut copy = Copy::attach(&mut local, buffer, 1).expect("attach");
        let mut typed = 0;
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            copy.type_at(&mut local, 0, "L").expect("type");
            typed += 1;
            while copy.sent.is_some() {
                copy.hear(next_buffer_update(&mut local, buffer).expect("an update"));
            }
            std::thread::sleep(Duration::from_millis(700));
        }
        (local, copy, typed)
    })
}

#[test]
#[ignore = "slow: minutes of typing in an editor over a caravan's Wi-Fi"]
fn typing_in_an_editor_over_a_caravan_link_lands_once_each() {
    let daemon = TestDaemon::start();
    let path = daemon.dir.path().join("notes.txt");
    std::fs::write(&path, "start\n").expect("write the file");
    let mut local = Client::connect(&daemon.socket()).expect("connect");
    let buffer = open_buffer(&mut local, &path);

    // Someone at the desktop types at the start of the file meanwhile.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let desk = type_at_the_desk(local, buffer, Arc::clone(&stop));

    // The laptop types at the end, as a window does, over the caravan's
    // network, and now and then loses its connection with a key on its
    // way, and attaches again on a new one.
    let network = Network::new(caravan());
    let caravan = Caravan::start(&network, 11);
    let udp = UdpRelay::start(daemon.udp, network.udp.clone()).expect("UDP relay");
    let carriers = Carriers::default();
    let open = || loop {
        let (relay, client) = connect(&daemon, &network, &udp, &carriers);
        match client {
            Ok(mut client) => {
                client
                    .set_read_timeout(Some(ECHO_LIMIT))
                    .expect("a timeout");
                break (relay, client);
            }
            Err(err) => {
                eprintln!("NETEM   connecting failed: {err}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    };
    let (mut relay, mut client) = open();
    let window = clackatoo_daemon::random_window();
    let mut copy = Copy::attach(&mut client, buffer, window).expect("attach");
    let mut typed = String::new();
    let mut keys = Latencies::new();
    let (mut reconnects, mut sent_again) = (0, 0);
    let started = Instant::now();
    let mut next_drop = started + Duration::from_secs(20);
    for n in 0_u32.. {
        if started.elapsed() > soak_length() {
            break;
        }
        let key = char::from_digit(n % 36, 36).expect("a digit").to_string();
        let pressed = Instant::now();
        let at = copy.text.len();
        typed.push_str(&key);
        let mut result = if Instant::now() < next_drop {
            copy.type_at(&mut client, at, &key)
        } else {
            // The window loses its connection with the key on its way,
            // before it's sent or just after.
            next_drop = Instant::now() + Duration::from_secs(20);
            if reconnects % 2 == 0 {
                copy.type_lost(at, &key);
            } else {
                copy.type_at(&mut client, at, &key).expect("sent");
            }
            Err(ClientError::Disconnected)
        };
        loop {
            if let Err(err) = result {
                eprintln!("NETEM   connecting again: {err}");
                reconnects += 1;
                (relay, client) = open();
                match copy.resume(&mut client) {
                    Ok(Resumed::Again) => sent_again += 1,
                    Ok(Resumed::Caught) => {}
                    Err(err) => {
                        result = Err(err);
                        continue;
                    }
                }
            }
            if copy.sent.is_none() {
                break;
            }
            result = next_buffer_update(&mut client, buffer).map(|update| copy.hear(update));
        }
        keys.push(pressed.elapsed());
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(caravan);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let (mut local, mut desk_copy, desk_typed) = desk.join().expect("the desk's thread");

    // Everyone ends up with the same text, each key in it once.
    let mut check = Client::connect(&daemon.socket()).expect("connect");
    let daemon_copy = Copy::attach(&mut check, buffer, 2).expect("attach");
    for (client, copy) in [(&mut client, &mut copy), (&mut local, &mut desk_copy)] {
        while copy.version < daemon_copy.version {
            copy.hear(next_buffer_update(client, buffer).expect("an update"));
        }
    }
    eprintln!(
        "NETEM caravan editor: keys {keys}; desk keys {desk_typed}; reconnects {reconnects}, \
         changes sent again {sent_again}; UDP {:?}, ssh {:?}",
        network.udp.stats(),
        network.ssh.stats(),
    );
    let expected = format!("{}start\n{typed}", "L".repeat(desk_typed));
    assert_eq!(daemon_copy.text, expected);
    assert_eq!(copy.text, expected);
    assert_eq!(desk_copy.text, expected);
    drop(relay);
}
