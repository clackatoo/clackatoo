//! A client asking the web app for much more than it reads, alone in a
//! process of its own, so the memory the daemon holds for it can be told
//! apart from any other test's.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use clackatoo_daemon::web::{Asset, WebConfig, WebMode};
use clackatoo_daemon::{AgentConfig, Daemon, DaemonConfig};

/// A file of the app's big enough that a client asking for it many times
/// without reading would cost the daemon a lot, if it answered them all.
static BIG: [u8; 1 << 20] = [b'x'; 1 << 20];

static BIG_APP: [Asset; 1] = [Asset {
    path: "/big",
    content_type: "application/octet-stream",
    body: &BIG,
    gzip: None,
    etag: "big",
}];

/// How much memory this process holds, in bytes: the daemon's among it.
fn resident() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .unwrap();
    let kib: u64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    kib * 1024
}

#[test]
fn pipelined_requests_arent_answered_past_what_the_client_reads() {
    let dir = tempfile::tempdir().unwrap();
    let mut web = WebConfig::new(WebMode::Tailscale, dir.path().join("web"));
    web.login = Some("jack@github".to_owned());
    web.assets = &BIG_APP;
    let socket = web.socket.clone();
    let config = DaemonConfig {
        terminal: clackatoo_term::Config::default(),
        shell: "/bin/sh".into(),
        home: dir.path().to_owned(),
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
        trash: None,
        boards: None,
        recovery: None,
        browser: None,
        web: Some(web),
    };
    let mut daemon = Daemon::bind(&dir.path().join("daemon.sock"), config).unwrap();
    let shutdown = daemon.shutdown_handle();
    let thread = std::thread::spawn(move || drop(daemon.run().unwrap()));

    let request = "GET /big HTTP/1.1\r\nHost: desk.tail1.ts.net\r\n\
                   Tailscale-User-Login: jack@github\r\n\r\n";
    let many = request.repeat(100);
    assert!(many.len() < 16 * 1024, "within what's held of a connection");
    let mut flood = UnixStream::connect(&socket).unwrap();
    let before = resident();
    flood.write_all(many.as_bytes()).unwrap();
    // Never read: answering them all would hold 100 MiB.
    std::thread::sleep(Duration::from_millis(500));
    let grown = resident().saturating_sub(before);
    assert!(
        grown < 24 << 20,
        "the daemon held {} MiB for a client that reads nothing",
        grown >> 20
    );
    // Meanwhile, everyone else is answered.
    let started = Instant::now();
    let mut other = UnixStream::connect(&socket).unwrap();
    other
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    other
        .write_all(
            b"GET /session HTTP/1.1\r\nHost: desk.tail1.ts.net\r\n\
              Tailscale-User-Login: jack@github\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
    let mut answer = String::new();
    other.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
    assert!(started.elapsed() < Duration::from_secs(2));
    // Read, they're answered in turn.
    flood
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut answers = vec![0; 3 << 20];
    flood.read_exact(&mut answers).unwrap();

    shutdown.shutdown();
    thread.join().unwrap();
}
