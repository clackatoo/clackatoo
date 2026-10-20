//! Forwarding ports: a real daemon, a window's connection to it over
//! QUIC, as from another machine, and the window's forwards, with servers
//! on "the daemon's machine" and browsers on "the window's", both here.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_daemon::forward::{Forwards, Problem, Report, Spec};
use clackatoo_daemon::{AgentConfig, Client, Daemon, DaemonConfig, ShutdownHandle};
use clackatoo_protocol::ForwardFailure;

/// How long anything may take before a test gives up on it.
const PATIENCE: Duration = Duration::from_secs(20);

/// A daemon on its own thread, in its own directory, which can be stopped
/// and started again at the same socket, as an upgrade would.
struct TestDaemon {
    dir: tempfile::TempDir,
    running: Option<(ShutdownHandle, JoinHandle<()>)>,
}

impl TestDaemon {
    fn start() -> Self {
        let mut daemon = Self {
            dir: tempfile::tempdir().expect("temp dir"),
            running: None,
        };
        daemon.run();
        daemon
    }

    fn run(&mut self) {
        let config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: self.dir.path().to_owned(),
            shell_integration: None,
            agents: AgentConfig::default(),
            mcp: None,
            udp_ports: None,
            languages: None,
            servers: None,
            declined: None,
            lsp_trace: None,
            server_idle: Duration::ZERO,
            prompt_wait: clackatoo_daemon::DEFAULT_PROMPT_WAIT,
            probe_wait: clackatoo_daemon::DEFAULT_PROBE_WAIT,
            session_wait: clackatoo_daemon::DEFAULT_SESSION_WAIT,
            gone_wait: clackatoo_daemon::DEFAULT_GONE_WAIT,
            park_after: clackatoo_daemon::DEFAULT_PARK_AFTER,
            fit_lasts: clackatoo_daemon::DEFAULT_FIT_LASTS,
            rename_gate: None,
            workspaces: None,
            keep_gate: None,
            boards: None,
            recovery: None,
            browser: None,
            web: None,
            trash: None,
            links: None,
            machine: None,
            machine_name: None,
            display: None,
        };
        let mut daemon = Daemon::bind(&self.socket(), config).expect("bind");
        let shutdown = daemon.shutdown_handle();
        let thread = std::thread::spawn(move || drop(daemon.run().expect("daemon failed")));
        self.running = Some((shutdown, thread));
    }

    /// Stops the daemon, closing its remote connections with a reason, as
    /// an upgrade does.
    fn stop(&mut self) {
        if let Some((shutdown, thread)) = self.running.take() {
            shutdown.shutdown();
            thread.join().expect("daemon thread panicked");
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("daemon.sock")
    }

    /// A client over QUIC, as a window on another machine connects.
    fn remote(&self) -> Client {
        let stream = UnixStream::connect(self.socket()).expect("connect to the socket");
        Client::over_quic(stream).expect("connect over QUIC")
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A window's forwards, and word of each report.
struct Window {
    forwards: Forwards,
    told: mpsc::Receiver<()>,
}

impl Window {
    fn new() -> Self {
        let (tell, told) = mpsc::channel();
        Self {
            forwards: Forwards::new(move || {
                let _ = tell.send(());
            }),
            told,
        }
    }

    /// Forwards a free port here to `port` on `host` there, returning the
    /// port here.
    fn forward(&mut self, host: &str, port: u16) -> u16 {
        let spec = Spec {
            local: 0,
            host: host.into(),
            port,
        };
        self.forwards.add(spec).expect("forward").local
    }

    /// Hands the forwards to `client`'s connection.
    fn attach(&mut self, client: &Client) {
        let forwarder = client.forwarder().expect("a remote client forwards");
        assert!(self.forwards.attach(forwarder));
    }

    /// The reports the forwards make within `wait`.
    fn reports(&self, wait: Duration) -> Vec<Report> {
        let until = Instant::now() + wait;
        let mut reports = Vec::new();
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if self.told.recv_timeout(left).is_err() {
                break;
            }
            reports.extend(self.forwards.take_reports());
        }
        reports.extend(self.forwards.take_reports());
        reports
    }

    /// Waits for the forward at `local` to have `count` connections open.
    fn wait_for_connections(&self, local: u16, count: usize) {
        let until = Instant::now() + PATIENCE;
        loop {
            let open = (self.forwards.list().into_iter())
                .find(|(spec, _)| spec.local == local)
                .map(|(_, open)| open);
            if open == Some(count) {
                return;
            }
            assert!(Instant::now() < until, "{local} has {open:?} connections");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A server on "the daemon's machine" that echoes what each connection
/// sends, finishing its side when the connection finishes its own. Each
/// connection's end goes on `ended`.
struct Echo {
    port: u16,
    ended: mpsc::Receiver<()>,
}

fn echo() -> Echo {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (end, ended) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let end = end.clone();
            std::thread::spawn(move || {
                let mut buffer = [0; 16 * 1024];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if stream.write_all(&buffer[..n]).is_err() {
                                break;
                            }
                        }
                    }
                }
                let _ = stream.shutdown(Shutdown::Write);
                let _ = end.send(());
            });
        }
    });
    Echo { port, ended }
}

/// Connects to `port` here, as a browser would.
fn browse(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect here");
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    stream
}

/// A port nothing listens on, a moment ago.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// What reading `stream` comes to: its bytes until it finishes, or the
/// error that cut it short.
fn read_to_end(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut read = Vec::new();
    stream.read_to_end(&mut read).map(|_| read)
}

/// Whether reading `stream` ends without everything having been sent:
/// it's reset, or finished early.
fn is_cut_off(stream: &mut TcpStream) -> bool {
    let mut buffer = [0; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return false,
            Err(err) if err.kind() == io::ErrorKind::TimedOut => return false,
            Err(_) => return true,
        }
    }
}

/// Whether reading `stream` ends in its being reset, as a connection
/// cut short is, rather than finished.
fn is_reset(stream: &mut TcpStream) -> bool {
    let mut buffer = [0; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return false,
            Ok(_) => {}
            Err(err) => return err.kind() == io::ErrorKind::ConnectionReset,
        }
    }
}

/// The byte at `at` of the pattern big transfers send.
fn pattern(at: usize) -> u8 {
    u8::try_from((at * 31 + at / 4093) % 251).unwrap()
}

#[test]
fn bytes_go_both_ways_and_each_side_finishing_is_passed_on() {
    let daemon = TestDaemon::start();
    let client = daemon.remote();
    let mut window = Window::new();
    let server = echo();
    let local = window.forward("localhost", server.port);
    window.attach(&client);

    let mut browser = browse(local);
    browser.write_all(b"hello there").unwrap();
    let mut back = [0; 11];
    browser.read_exact(&mut back).unwrap();
    assert_eq!(&back, b"hello there");
    window.wait_for_connections(local, 1);

    // The browser finishing its side finishes the server's reading; the
    // server's finishing then finishes the browser's.
    browser.write_all(b" and goodbye").unwrap();
    browser.shutdown(Shutdown::Write).unwrap();
    assert_eq!(read_to_end(&mut browser).unwrap(), b" and goodbye");
    server
        .ended
        .recv_timeout(PATIENCE)
        .expect("the server's end closed");
    window.wait_for_connections(local, 0);
    assert!(window.reports(Duration::ZERO).is_empty());
}

#[test]
fn big_transfers_go_both_ways_whole() {
    const SIZE: usize = 8 << 20;
    let daemon = TestDaemon::start();
    let client = daemon.remote();
    let mut window = Window::new();
    // Reads all it's sent, checking it, while sending as much back.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut sending = stream.try_clone().unwrap();
        let sender = std::thread::spawn(move || {
            let data: Vec<u8> = (0..SIZE).map(pattern).collect();
            sending.write_all(&data).unwrap();
            sending.shutdown(Shutdown::Write).unwrap();
        });
        let mut reading = stream;
        let mut got = Vec::new();
        reading.read_to_end(&mut got).unwrap();
        sender.join().unwrap();
        got
    });
    let local = window.forward("127.0.0.1", port);
    window.attach(&client);

    let browser = browse(local);
    let mut sending = browser.try_clone().unwrap();
    let upload = std::thread::spawn(move || {
        let data: Vec<u8> = (0..SIZE).map(|at| pattern(at + 7)).collect();
        sending.write_all(&data).unwrap();
        sending.shutdown(Shutdown::Write).unwrap();
    });
    let mut reading = browser;
    let downloaded = read_to_end(&mut reading).unwrap();
    upload.join().unwrap();
    let uploaded = server.join().unwrap();
    assert_eq!(downloaded.len(), SIZE);
    assert!(
        downloaded
            .iter()
            .enumerate()
            .all(|(at, &b)| b == pattern(at))
    );
    assert_eq!(uploaded.len(), SIZE);
    assert!(
        uploaded
            .iter()
            .enumerate()
            .all(|(at, &b)| b == pattern(at + 7))
    );
}

#[test]
fn a_browser_that_reads_nothing_holds_the_server_back_and_then_gets_everything() {
    const SIZE: usize = 64 << 20;
    let daemon = TestDaemon::start();
    let client = daemon.remote();
    let mut window = Window::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let written = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&written);
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut at = 0;
        while at < SIZE {
            let block: Vec<u8> = (at..(at + 64 * 1024).min(SIZE)).map(pattern).collect();
            if stream.write_all(&block).is_err() {
                return;
            }
            at += block.len();
            counted.store(at, Ordering::Relaxed);
        }
    });
    let local = window.forward("localhost", port);
    window.attach(&client);
    let mut browser = browse(local);

    // The server writes until everything between it and the browser is
    // full, then waits.
    let mut last = 0;
    let mut steady_since = Instant::now();
    let until = Instant::now() + PATIENCE;
    while steady_since.elapsed() < Duration::from_millis(1500) {
        assert!(Instant::now() < until, "it never stopped");
        std::thread::sleep(Duration::from_millis(50));
        let now = written.load(Ordering::Relaxed);
        if now != last {
            last = now;
            steady_since = Instant::now();
        }
    }
    // What's held is bounded: the kernel's buffers at each socket, and at
    // most a stream's window at each end of QUIC, and a chunk each side.
    assert!(last > 0, "nothing was sent at all");
    assert!(last < 24 << 20, "{last} bytes went out with nobody reading");

    let got = read_to_end(&mut browser).unwrap();
    assert_eq!(got.len(), SIZE);
    assert!(got.iter().enumerate().all(|(at, &b)| b == pattern(at)));
}

#[test]
fn several_connections_go_at_once() {
    let daemon = TestDaemon::start();
    let client = daemon.remote();
    let mut window = Window::new();
    let server = echo();
    let local = window.forward("localhost", server.port);
    window.attach(&client);

    let browsers: Vec<_> = (0..8)
        .map(|n: usize| {
            std::thread::spawn(move || {
                let data: Vec<u8> = (0..256 * 1024).map(|at| pattern(at + n)).collect();
                let browser = browse(local);
                let mut sending = browser.try_clone().unwrap();
                let sent = data.clone();
                let sender = std::thread::spawn(move || {
                    sending.write_all(&sent).unwrap();
                    sending.shutdown(Shutdown::Write).unwrap();
                });
                let mut reading = browser;
                let back = read_to_end(&mut reading).unwrap();
                sender.join().unwrap();
                back == data
            })
        })
        .collect();
    for browser in browsers {
        assert!(browser.join().unwrap(), "a connection's echo differed");
    }
}

#[test]
fn a_connection_nothing_answers_is_cut_off_and_told_once() {
    let daemon = TestDaemon::start();
    let client = daemon.remote();
    let mut window = Window::new();
    let port = closed_port();
    let local = window.forward("localhost", port);
    window.attach(&client);

    // A browser sends its request at once, before any answer.
    for _ in 0..3 {
        let mut browser = browse(local);
        let _ = browser.write_all(b"GET / HTTP/1.1\r\n\r\n");
        assert!(is_cut_off(&mut browser));
    }
    let reports = window.reports(Duration::from_millis(300));
    assert_eq!(
        reports,
        vec![Report {
            spec: Spec {
                local,
                host: "localhost".into(),
                port,
            },
            problem: Problem::Failed(ForwardFailure::Refused),
        }],
    );
    assert!(reports[0].describe("desk").contains("nothing is listening"));

    // Once one works, the next failure is told again.
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let mut browser = browse(local);
    let (mut served, _) = listener.accept().unwrap();
    served.write_all(b"up").unwrap();
    let mut up = [0; 2];
    browser.read_exact(&mut up).unwrap();
    drop((served, browser, listener));
    let mut browser = browse(local);
    assert!(is_cut_off(&mut browser));
    assert_eq!(window.reports(Duration::from_millis(300)).len(), 1);
}

#[test]
fn a_name_the_daemons_machine_cant_find_is_told() {
    let daemon = TestDaemon::start();
    let client = daemon.remote();
    let mut window = Window::new();
    // Never a machine's name (RFC 6761).
    let local = window.forward("nowhere.invalid", 80);
    window.attach(&client);
    let mut browser = browse(local);
    let _ = browser.write_all(b"GET / HTTP/1.1\r\n\r\n");
    assert!(is_cut_off(&mut browser));
    let reports = window.reports(Duration::from_millis(300));
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert_eq!(
        reports[0].problem,
        Problem::Failed(ForwardFailure::NoSuchHost)
    );
}

#[test]
fn stopping_a_forward_closes_its_port_and_its_connections() {
    let daemon = TestDaemon::start();
    let client = daemon.remote();
    let mut window = Window::new();
    let server = echo();
    let local = window.forward("localhost", server.port);
    window.attach(&client);
    let mut browser = browse(local);
    browser.write_all(b"x").unwrap();
    let mut back = [0; 1];
    browser.read_exact(&mut back).unwrap();

    assert!(window.forwards.remove(local).is_some());
    assert!(is_reset(&mut browser));
    server
        .ended
        .recv_timeout(PATIENCE)
        .expect("the server's end closed");
    let refused = TcpStream::connect(("127.0.0.1", local)).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
}

#[test]
fn forwards_come_back_when_the_window_connects_again_after_the_daemon_restarts() {
    let mut daemon = TestDaemon::start();
    let client = daemon.remote();
    let mut window = Window::new();
    let server = echo();
    let local = window.forward("localhost", server.port);
    window.attach(&client);
    let mut browser = browse(local);
    browser.write_all(b"before").unwrap();
    let mut back = [0; 6];
    browser.read_exact(&mut back).unwrap();

    // As an upgrade does: the connection closes, saying why, and what was
    // forwarded through it is cut off, at both ends.
    daemon.stop();
    assert!(is_reset(&mut browser));
    server
        .ended
        .recv_timeout(PATIENCE)
        .expect("the server's end closed");
    window.forwards.detach();
    drop(client);

    // A browser that connects meanwhile waits, and is taken once the
    // window is back.
    let mut waiting = browse(local);
    waiting.write_all(b"meanwhile").unwrap();
    daemon.run();
    let client = daemon.remote();
    window.attach(&client);
    let mut back = [0; 9];
    waiting.read_exact(&mut back).unwrap();
    assert_eq!(&back, b"meanwhile");
    let mut browser = browse(local);
    browser.write_all(b"after").unwrap();
    let mut back = [0; 5];
    browser.read_exact(&mut back).unwrap();
    assert_eq!(&back, b"after");
}

#[test]
fn a_window_that_closes_resets_what_it_forwarded() {
    let daemon = TestDaemon::start();
    let client = daemon.remote();
    let mut window = Window::new();
    let server = echo();
    let local = window.forward("localhost", server.port);
    window.attach(&client);
    let mut browser = browse(local);
    browser.write_all(b"x").unwrap();
    let mut back = [0; 1];
    browser.read_exact(&mut back).unwrap();

    // The window's forwards go first, the QUIC connection's thread still
    // holding its copies of their sockets.
    drop(window);
    assert!(is_reset(&mut browser));
    server
        .ended
        .recv_timeout(PATIENCE)
        .expect("the server's end closed");
    let refused = TcpStream::connect(("127.0.0.1", local)).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);
    drop(client);
}

#[test]
fn a_browser_reset_after_finishing_its_side_cuts_off_a_quiet_server() {
    let daemon = TestDaemon::start();
    let client = daemon.remote();
    let mut window = Window::new();
    // A server that never says anything, nor closes.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (cut, cut_off) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        stream.read_to_end(&mut request).unwrap();
        let until = Instant::now() + PATIENCE;
        while Instant::now() < until {
            if stream.take_error().unwrap().is_some() {
                let _ = cut.send(request);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let local = window.forward("localhost", port);
    window.attach(&client);
    let browser = browse(local);
    (&browser).write_all(b"GET /").unwrap();
    browser.shutdown(Shutdown::Write).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    // Reset, rather than closed.
    let linger = rustix::net::sockopt::set_socket_linger(&browser, Some(Duration::ZERO));
    linger.unwrap();
    drop(browser);
    let request = cut_off
        .recv_timeout(PATIENCE)
        .expect("the server was cut off");
    assert_eq!(request, b"GET /");
}
