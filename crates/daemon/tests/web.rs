//! The web app, end to end: a real daemon, listening as it would behind
//! `tailscale serve`, on its unix socket, asked over HTTP and WebSocket
//! connections as a phone would ask it, with someone else's WebSocket
//! client (tungstenite).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_daemon::web::{WebConfig, WebMode};
use clackatoo_daemon::{AgentConfig, Client, CustomAgent, Daemon, DaemonConfig, ShutdownHandle};
use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    AgentTools, ChatAction, ChatSummary, ClientMessage, Content, Error, Event, GridNode, Item,
    NewAgent, NewTerminal, PROTOCOL_VERSION, PaneTerminal, Request, Response, ServerMessage, Size,
    TerminalId, View, ViewUpdate,
};
use tungstenite::client::IntoClientRequest as _;
use tungstenite::http::HeaderValue;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message, WebSocket};

/// The password the test daemons have.
const PASSWORD: &str = "correct horse battery staple";

/// The Tailscale login the tests' requests come from.
const LOGIN: &str = "jack@github";

/// Where a test daemon's web app listens.
#[derive(Clone, Debug)]
enum Target {
    /// Its unix socket, for `tailscale serve`.
    Unix(PathBuf),
    /// The network, for public.
    Tcp(SocketAddr),
}

/// A connection to a test daemon's web app.
#[derive(Debug)]
enum Conn {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Conn {
    fn open(target: &Target) -> Self {
        let conn = match target {
            Target::Unix(path) => Self::Unix(UnixStream::connect(path).unwrap()),
            Target::Tcp(address) => Self::Tcp(TcpStream::connect(address).unwrap()),
        };
        conn.set_read_timeout(Some(Duration::from_secs(10)));
        conn
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) {
        match self {
            Self::Unix(stream) => stream.set_read_timeout(timeout).unwrap(),
            Self::Tcp(stream) => stream.set_read_timeout(timeout).unwrap(),
        }
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) {
        match self {
            Self::Unix(stream) => stream.set_write_timeout(timeout).unwrap(),
            Self::Tcp(stream) => stream.set_write_timeout(timeout).unwrap(),
        }
    }
}

impl Read for Conn {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Unix(stream) => stream.read(buffer),
            Self::Tcp(stream) => stream.read(buffer),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Unix(stream) => stream.write(bytes),
            Self::Tcp(stream) => stream.write(bytes),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A daemon with the web app on, on its own thread, in its own temporary
/// directory.
struct TestDaemon {
    dir: tempfile::TempDir,
    web: Target,
    shutdown: ShutdownHandle,
    thread: Option<JoinHandle<()>>,
}

impl TestDaemon {
    /// A daemon reached through Tailscale, letting in `LOGIN`, with a
    /// password.
    fn start() -> Self {
        Self::start_with(|_| {})
    }

    /// A daemon reached through Tailscale, letting in `LOGIN`, with a
    /// password, and the web app's settings changed by `change`.
    fn start_with(change: impl FnOnce(&mut WebConfig)) -> Self {
        Self::start_daemon(change, |_| {})
    }

    /// As [`start_with`](Self::start_with), with the daemon's own settings
    /// changed by `daemon`.
    fn start_daemon(
        change: impl FnOnce(&mut WebConfig),
        daemon: impl FnOnce(&mut DaemonConfig),
    ) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let agent = vec![
            example("fake_agent").into(),
            "--log".into(),
            dir.path().join("agent.log").into(),
        ];
        let mut web = WebConfig::new(WebMode::Tailscale, dir.path().join("web"));
        web.address = "127.0.0.1:0".parse().unwrap();
        web.login = Some(LOGIN.to_owned());
        web.password = Some(clackatoo_daemon::web::hash_password(PASSWORD));
        change(&mut web);
        let socket = web.socket.clone();
        let tailscale = web.mode == WebMode::Tailscale;
        let mut config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: dir.path().to_owned(),
            shell_integration: None,
            agents: AgentConfig {
                custom: vec![CustomAgent {
                    name: "claude".to_owned(),
                    title: None,
                    command: agent,
                    starting_mode: None,
                }],
                ..AgentConfig::default()
            },
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
            browser: None,
            web: Some(web),
        };
        daemon(&mut config);
        let mut daemon = Daemon::bind(&dir.path().join("daemon.sock"), config).expect("bind");
        let web = if tailscale {
            Target::Unix(socket)
        } else {
            Target::Tcp(daemon.web_address().expect("the web app listens"))
        };
        let shutdown = daemon.shutdown_handle();
        let thread = std::thread::spawn(move || drop(daemon.run().expect("daemon failed")));
        Self {
            dir,
            web,
            shutdown,
            thread: Some(thread),
        }
    }

    /// A client on the daemon's socket, as a window is.
    fn window(&self) -> Client {
        Client::connect(&self.dir.path().join("daemon.sock")).expect("connect")
    }

    /// The headers a page of the app sends, through `tailscale serve`.
    fn app(&self) -> Vec<(String, String)> {
        let host = match &self.web {
            Target::Unix(_) => "desk.tail1.ts.net".to_owned(),
            Target::Tcp(address) => format!("localhost:{}", address.port()),
        };
        vec![
            ("Origin".into(), format!("https://{host}")),
            ("Host".into(), host),
            ("Tailscale-User-Login".into(), LOGIN.into()),
        ]
    }

    /// Asks the web app `path` with `body`, with the app's headers changed
    /// by `headers`.
    fn post(
        &self,
        path: &str,
        body: &str,
        headers: impl FnOnce(&mut Vec<(String, String)>),
    ) -> Http {
        let mut sent = self.app();
        headers(&mut sent);
        http(&self.web, "POST", path, &sent, body)
    }

    /// Pairs a device with the password, returning its cookie.
    fn pair(&self) -> String {
        let body = format!(r#"{{"name":"phone","password":"{PASSWORD}"}}"#);
        let answer = self.post("/pair", &body, |_| {});
        assert_eq!(answer.status, 200, "{}", answer.text());
        answer.cookie().expect("pairing gives a cookie")
    }

    /// Pairs a device by asking a window, returning its cookie.
    fn pair_from_a_window(&self) -> String {
        let asked = self
            .post("/pair", r#"{"name":"phone","ask":true}"#, |_| {})
            .json();
        let (wait, code) = (
            asked["wait"].as_str().unwrap(),
            asked["code"].as_str().unwrap(),
        );
        self.window()
            .request(&Request::AnswerPairing {
                code: Some(code.to_owned()),
            })
            .unwrap();
        let paired = self.post("/pair/wait", &format!(r#"{{"wait":"{wait}"}}"#), |_| {});
        assert_eq!(paired.status, 200, "{}", paired.text());
        paired.cookie().expect("pairing gives a cookie")
    }

    /// Opens a WebSocket carrying `cookie`, with the app's headers changed
    /// by `headers`.
    fn socket(
        &self,
        cookie: &str,
        headers: impl FnOnce(&mut Vec<(String, String)>),
    ) -> Result<Phone, u16> {
        self.raw_socket(cookie, headers).map(Phone::new)
    }

    /// Opens a WebSocket as [`socket`](Self::socket) does, without saying
    /// hello.
    fn raw_socket(
        &self,
        cookie: &str,
        headers: impl FnOnce(&mut Vec<(String, String)>),
    ) -> Result<WebSocket<Conn>, u16> {
        let mut sent = self.app();
        sent.push(("Cookie".into(), cookie.to_owned()));
        headers(&mut sent);
        let mut request = "ws://desk.tail1.ts.net/ws".into_client_request().unwrap();
        for (name, value) in sent {
            let name: tungstenite::http::HeaderName = name.parse().unwrap();
            request
                .headers_mut()
                .insert(name, HeaderValue::from_str(&value).unwrap());
        }
        match tungstenite::client(request, Conn::open(&self.web)) {
            Ok((socket, _)) => Ok(socket),
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response))) => {
                Err(response.status().as_u16())
            }
            Err(err) => panic!("{err}"),
        }
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

/// The example called `name`, which `cargo test` builds with the tests.
fn example(name: &str) -> PathBuf {
    let test = std::env::current_exe().expect("the test's path");
    let examples = test
        .parent()
        .and_then(Path::parent)
        .expect("tests are in the profile's deps")
        .join("examples");
    std::fs::read_dir(&examples)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let file = entry.file_name();
            let file = file.to_string_lossy();
            file.starts_with(name) && !file.ends_with(".d")
        })
        .max_by_key(|entry| entry.metadata().and_then(|meta| meta.modified()).ok())
        .map_or_else(
            || panic!("no {name} in {}: run cargo test", examples.display()),
            |entry| entry.path(),
        )
}

/// An answer over HTTP.
#[derive(Debug)]
struct Http {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Http {
    fn header(&self, name: &str) -> Option<&str> {
        (self.headers.iter())
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The device's cookie the answer sets, as a request sends it back.
    fn cookie(&self) -> Option<String> {
        let set = self.header("set-cookie")?;
        Some(set.split(';').next()?.to_owned())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("JSON")
    }
}

/// A request, as bytes: `method`, `path`, `headers` and `body`.
fn request_bytes(method: &str, path: &str, headers: &[(String, String)], body: &str) -> String {
    let mut request = format!("{method} {path} HTTP/1.1\r\n");
    for (name, value) in headers {
        let _ = write!(request, "{name}: {value}\r\n");
    }
    let _ = write!(request, "Content-Length: {}\r\n\r\n{body}", body.len());
    request
}

/// Asks the web app at `target` with `method`, `path`, `headers` and
/// `body`, on a connection of its own.
fn http(
    target: &Target,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &str,
) -> Http {
    let mut conn = Conn::open(target);
    conn.set_read_timeout(Some(Duration::from_secs(150)));
    let mut headers = headers.to_vec();
    headers.push(("Connection".into(), "close".into()));
    conn.write_all(request_bytes(method, path, &headers, body).as_bytes())
        .unwrap();
    let mut answer = Vec::new();
    conn.read_to_end(&mut answer).unwrap();
    let split = (answer.windows(4))
        .position(|window| window == b"\r\n\r\n")
        .expect("a head");
    let head = String::from_utf8(answer[..split].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|line| line.split_once(": "))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    Http {
        status,
        headers,
        body: answer[split + 4..].to_vec(),
    }
}

/// Whether the daemon closes `conn` within `limit`: reading ends, or
/// fails as a reset does.
fn closed_within(conn: &mut Conn, limit: Duration) -> bool {
    conn.set_read_timeout(Some(limit));
    let deadline = Instant::now() + limit;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match conn.read(&mut buffer) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return false;
            }
            Err(_) => return true,
        }
        if Instant::now() >= deadline {
            return false;
        }
    }
}

/// A device's WebSocket, speaking the protocol, as the app does.
struct Phone {
    socket: WebSocket<Conn>,
    decoder: FrameDecoder,
    events: VecDeque<Event>,
    /// The view of the chat it follows, kept current.
    view: Option<View>,
}

impl Phone {
    /// A device that has said hello.
    fn new(socket: WebSocket<Conn>) -> Self {
        let mut phone = Self {
            socket,
            decoder: FrameDecoder::new(),
            events: VecDeque::new(),
            view: None,
        };
        phone.send(&ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        });
        assert_eq!(
            phone.receive().unwrap(),
            ServerMessage::Hello {
                protocol_version: PROTOCOL_VERSION
            }
        );
        phone
    }

    fn send(&mut self, message: &ClientMessage) {
        let frame = frame::encode(PROTOCOL_VERSION, message).unwrap();
        self.socket.send(Message::binary(frame)).unwrap();
    }

    /// The next message, or the close that came instead.
    fn receive(&mut self) -> Result<ServerMessage, Option<CloseCode>> {
        loop {
            if let Some(message) = self.decoder.decode(PROTOCOL_VERSION).unwrap() {
                return Ok(message);
            }
            match self.socket.read() {
                Ok(Message::Binary(bytes)) => self.decoder.push(&bytes),
                Ok(Message::Close(close)) => return Err(close.map(|close| close.code)),
                Ok(_) => {}
                Err(tungstenite::Error::ConnectionClosed) => return Err(None),
                Err(err) => panic!("{err}"),
            }
        }
    }

    fn request(&mut self, request: Request) -> Response {
        self.send(&ClientMessage::Request(request));
        loop {
            match self.receive().expect("an answer") {
                ServerMessage::Response(response) => return response,
                ServerMessage::Event(event) => self.events.push_back(event),
                other => panic!("{other:?}"),
            }
        }
    }

    /// The next event, acknowledging each view update, as the app does.
    fn event(&mut self) -> Event {
        let event = match self.events.pop_front() {
            Some(event) => event,
            None => match self.receive().expect("an event") {
                ServerMessage::Event(event) => event,
                other => panic!("{other:?}"),
            },
        };
        if let Event::View { terminal, update } = &event {
            match (&mut self.view, update) {
                (_, ViewUpdate::Snapshot(snapshot)) => self.view = Some(snapshot.clone()),
                (Some(view), update) => view.apply(update.clone()),
                (None, ViewUpdate::Patch(_)) => panic!("a snapshot first"),
            }
            self.send(&ClientMessage::FrameDone {
                terminal: *terminal,
            });
        }
        event
    }

    /// Waits for the chat it follows to be as `done` wants.
    fn chat_until<T>(&mut self, done: impl Fn(&clackatoo_protocol::ChatNode) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(found) = (self.view.as_ref()).and_then(|view| done(view.chat()?)) {
                return found;
            }
            assert!(Instant::now() < deadline, "the chat never got there");
            self.event();
        }
    }

    /// Waits for the terminal it follows to show what `done` wants.
    fn grid_until<T>(&mut self, done: impl Fn(&GridNode) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(found) = (self.view.as_ref()).and_then(|view| done(view.grid()?)) {
                return found;
            }
            assert!(Instant::now() < deadline, "the terminal never got there");
            self.event();
        }
    }

    /// Waits for the chats to be as `done` wants.
    fn chats_until(&mut self, done: impl Fn(&[ChatSummary]) -> bool) -> Vec<ChatSummary> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "the chats never got there");
            if let Event::Chats(chats) = self.event()
                && done(&chats)
            {
                return chats;
            }
        }
    }
}

#[test]
fn the_app_is_served_with_a_strict_policy() {
    let daemon = TestDaemon::start();
    let page = http(&daemon.web, "GET", "/", &daemon.app(), "");
    assert_eq!(page.status, 200);
    let policy = page.header("content-security-policy").unwrap();
    assert!(policy.contains("default-src 'none'"), "{policy}");
    assert!(policy.contains("frame-ancestors 'none'"), "{policy}");
    assert_eq!(page.header("x-content-type-options"), Some("nosniff"));
    assert!(
        page.text().contains("without its web app"),
        "no app in tests"
    );
    let missing = http(&daemon.web, "GET", "/nothing.js", &daemon.app(), "");
    assert_eq!(missing.status, 404);
}

#[test]
fn a_device_pairs_with_the_password_and_connects() {
    let daemon = TestDaemon::start();
    let session = http(&daemon.web, "GET", "/session", &daemon.app(), "").json();
    assert_eq!(session["state"], "unpaired");
    assert_eq!(session["password"], true);

    let cookie = daemon.pair();
    assert!(cookie.starts_with("__Host-clackatoo="), "{cookie}");
    let mut headers = daemon.app();
    headers.push(("Cookie".into(), cookie.clone()));
    let session = http(&daemon.web, "GET", "/session", &headers, "").json();
    assert_eq!(session["state"], "paired");
    assert_eq!(session["name"], "phone");

    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    assert_eq!(
        phone.request(Request::WatchChats),
        Response::Chats(Vec::new())
    );
    let mut window = daemon.window();
    let Response::Devices(devices) = window.request(&Request::Devices).unwrap() else {
        panic!("devices");
    };
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "phone");
    assert_eq!(
        devices[0].login.as_deref(),
        Some(LOGIN),
        "let in as no one else"
    );
    assert!(devices[0].connected);
}

#[test]
fn wrong_passwords_are_limited() {
    let daemon = TestDaemon::start();
    let wrong = r#"{"name":"phone","password":"hunter2"}"#;
    for _ in 0..5 {
        let answer = daemon.post("/pair", wrong, |_| {});
        assert_eq!(answer.status, 403, "{}", answer.text());
        assert!(answer.cookie().is_none());
    }
    let answer = daemon.post("/pair", wrong, |_| {});
    assert_eq!(answer.status, 429);
    assert!(answer.header("retry-after").is_some());
    // Even the right one waits its turn now.
    let right = format!(r#"{{"name":"phone","password":"{PASSWORD}"}}"#);
    assert_eq!(daemon.post("/pair", &right, |_| {}).status, 429);
}

#[test]
fn a_revoked_device_is_cut_off_and_refused() {
    let daemon = TestDaemon::start();
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    let mut window = daemon.window();
    let Response::Devices(devices) = window.request(&Request::Devices).unwrap() else {
        panic!("devices");
    };
    assert_eq!(
        window
            .request(&Request::RevokeDevice {
                device: devices[0].id
            })
            .unwrap(),
        Response::Done
    );
    assert_eq!(phone.receive(), Err(Some(CloseCode::from(4001))));
    assert_eq!(daemon.socket(&cookie, |_| {}).err(), Some(401));
    assert_eq!(
        window
            .request(&Request::RevokeDevice {
                device: devices[0].id
            })
            .unwrap(),
        Response::Error(Error::NoSuchDevice(devices[0].id))
    );
    let mut headers = daemon.app();
    headers.push(("Cookie".into(), cookie));
    let session = http(&daemon.web, "GET", "/session", &headers, "").json();
    assert_eq!(session["state"], "unpaired");
}

#[test]
fn a_page_elsewhere_is_refused() {
    let daemon =
        TestDaemon::start_with(|web| web.origin = Some("https://desk.tail1.ts.net".into()));
    let cookie = daemon.pair();
    // Sent to another name, as when a page's name is pointed at this
    // machine: refused, whatever it asks.
    let mut headers = daemon.app();
    headers.retain(|(name, _)| name != "Host");
    headers.push(("Host".into(), "evil.example".into()));
    assert_eq!(
        http(&daemon.web, "GET", "/session", &headers, "").status,
        421
    );
    let elsewhere = |headers: &mut Vec<(String, String)>| {
        headers.retain(|(name, _)| name != "Origin");
        headers.push(("Origin".into(), "https://evil.example".into()));
    };
    assert_eq!(daemon.socket(&cookie, elsewhere).err(), Some(403));
    let no_origin = |headers: &mut Vec<(String, String)>| {
        headers.retain(|(name, _)| name != "Origin");
    };
    assert_eq!(daemon.socket(&cookie, no_origin).err(), Some(403));
    let plain_http = |headers: &mut Vec<(String, String)>| {
        headers.retain(|(name, _)| name != "Origin");
        headers.push(("Origin".into(), "http://localhost".into()));
    };
    assert_eq!(daemon.socket(&cookie, plain_http).err(), Some(403));
    let body = format!(r#"{{"name":"phone","password":"{PASSWORD}"}}"#);
    let answer = daemon.post("/pair", &body, elsewhere);
    assert_eq!(answer.status, 403);
    assert!(answer.cookie().is_none());
    // The right page still gets in.
    assert!(daemon.socket(&cookie, |_| {}).is_ok());
}

#[test]
fn only_tailscale_and_the_login_let_in_reach_the_app() {
    let daemon = TestDaemon::start_with(|web| web.login = Some(LOGIN.to_owned()));
    let anonymous = |headers: &mut Vec<(String, String)>| {
        headers.retain(|(name, _)| name != "Tailscale-User-Login");
    };
    let mut headers = daemon.app();
    anonymous(&mut headers);
    // Another program on this machine, not `tailscale serve`.
    assert_eq!(http(&daemon.web, "GET", "/", &headers, "").status, 403);
    let body = format!(r#"{{"name":"phone","password":"{PASSWORD}"}}"#);
    assert_eq!(daemon.post("/pair", &body, anonymous).status, 403);
    let someone_else = |headers: &mut Vec<(String, String)>| {
        headers.retain(|(name, _)| name != "Tailscale-User-Login");
        headers.push(("Tailscale-User-Login".into(), "eve@github".into()));
    };
    assert_eq!(daemon.post("/pair", &body, someone_else).status, 403);
    let cookie = daemon.pair();
    assert_eq!(daemon.socket(&cookie, someone_else).err(), Some(403));
    assert_eq!(daemon.socket(&cookie, anonymous).err(), Some(403));
    assert!(daemon.socket(&cookie, |_| {}).is_ok());
}

#[test]
fn a_device_is_allowed_from_a_window_by_its_code() {
    let daemon = TestDaemon::start();
    let mut window = daemon.window();
    window.watch_workspaces().unwrap();
    let asked = daemon.post("/pair", r#"{"name":"jack's iPhone","ask":true}"#, |_| {});
    assert_eq!(asked.status, 200, "{}", asked.text());
    assert!(asked.cookie().is_none(), "asking gives nothing yet");
    let asked = asked.json();
    let code = asked["code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 4);
    let wait = asked["wait"].as_str().unwrap().to_owned();

    let Event::Pairing(pairing) = window.next_event().unwrap() else {
        panic!("the window is asked");
    };
    assert!(!pairing.locked);
    let asking = pairing.asking;
    assert_eq!(asking.len(), 1);
    assert_eq!(asking[0].name, "jack's iPhone");
    assert_eq!(asking[0].from, LOGIN);

    // The device waits for the answer meanwhile.
    let waiting = {
        let daemon_web = daemon.web.clone();
        let headers = daemon.app();
        let body = format!(r#"{{"wait":"{wait}"}}"#);
        std::thread::spawn(move || http(&daemon_web, "POST", "/pair/wait", &headers, &body))
    };
    let wrong = if code == "0000" { "1111" } else { "0000" };
    assert!(matches!(
        window.request(&Request::AnswerPairing {
            code: Some(wrong.into())
        }),
        Ok(Response::Error(Error::Pairing(_)))
    ));
    // A device can't let another in.
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    assert!(matches!(
        phone.request(Request::AnswerPairing {
            code: Some(code.clone())
        }),
        Response::Error(Error::Pairing(_))
    ));
    assert_eq!(
        window
            .request(&Request::AnswerPairing { code: Some(code) })
            .unwrap(),
        Response::Text("Allowed jack's iPhone to pair".into())
    );
    let paired = waiting.join().unwrap();
    assert_eq!(paired.status, 200, "{}", paired.text());
    let cookie = paired.cookie().expect("a cookie once allowed");
    assert!(daemon.socket(&cookie, |_| {}).is_ok());
    let Response::Devices(devices) = window.request(&Request::Devices).unwrap() else {
        panic!("devices");
    };
    let allowed = devices.iter().find(|device| device.name == "jack's iPhone");
    assert_eq!(allowed.unwrap().login.as_deref(), Some(LOGIN));
    // Collected once: the wait is spent.
    let again = daemon.post("/pair/wait", &format!(r#"{{"wait":"{wait}"}}"#), |_| {});
    assert_eq!(again.status, 403);
}

#[test]
fn a_device_turned_down_gets_nothing() {
    let daemon = TestDaemon::start();
    let mut window = daemon.window();
    let asked = daemon
        .post("/pair", r#"{"name":"stranger","ask":true}"#, |_| {})
        .json();
    let wait = asked["wait"].as_str().unwrap().to_owned();
    assert_eq!(
        window
            .request(&Request::AnswerPairing { code: None })
            .unwrap(),
        Response::Text("Turned down the device asking to pair".into())
    );
    let answer = daemon.post("/pair/wait", &format!(r#"{{"wait":"{wait}"}}"#), |_| {});
    assert_eq!(answer.status, 403);
    assert!(answer.cookie().is_none());
    let forged = daemon.post(
        "/pair/wait",
        &format!(r#"{{"wait":"{}"}}"#, "0".repeat(32)),
        |_| {},
    );
    assert_eq!(forged.status, 403);
}

#[test]
fn a_device_answers_a_chats_question_over_its_websocket() {
    let daemon = TestDaemon::start();
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    assert_eq!(
        phone.request(Request::WatchChats),
        Response::Chats(Vec::new())
    );
    assert!(matches!(
        phone.request(Request::WatchWorkspaces),
        Response::Workspaces(_)
    ));
    let chat = PaneTerminal::Agent(NewAgent {
        size: Size::new(40, 10).unwrap(),
        cwd: None,
        agent: None,
        tools: AgentTools::Ask,
        task: true,
        browser: false,
        model: None,
        resume: None,
    });
    assert!(matches!(
        phone.request(Request::NewWorkspace {
            name: None,
            terminal: chat,
        }),
        Response::PaneCreated(_)
    ));
    let chats = phone.chats_until(|chats| {
        chats.len() == 1 && chats[0].state == clackatoo_protocol::ChatState::Ready
    });
    let id = chats[0].chat;
    assert!(chats[0].workspace.is_some());
    assert_eq!(
        phone.request(Request::Attach { terminal: id }),
        Response::Done
    );
    assert_eq!(
        phone.request(Request::Chat {
            terminal: id,
            action: ChatAction::Send("ask".into()),
        }),
        Response::Done
    );
    let chats = phone.chats_until(|chats| chats[0].asking);
    assert!(chats[0].needs_you());
    // The question, as the chat's view shows it, with all its choices.
    let question = phone.chat_until(|chat| chat.question.clone());
    let names: Vec<&str> = question.choices.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Allow", "Reject"]);
    assert_eq!(
        phone.request(Request::Chat {
            terminal: id,
            action: ChatAction::Answer {
                question: question.id,
                option: Some("allow".into()),
            },
        }),
        Response::Done
    );
    let words = phone.chat_until(|chat| {
        let turn = chat.turns.last()?;
        turn.end.as_ref()?;
        Some(
            (turn.items.iter())
                .filter_map(|item| match item {
                    Item::Words(words) => Some(words.clone()),
                    _ => None,
                })
                .collect::<String>(),
        )
    });
    assert_eq!(words.trim(), "picked allow");
    let Response::Chats(chats) = phone.request(Request::WatchChats) else {
        panic!("chats");
    };
    assert!(!chats[0].asking);
}

#[test]
fn a_turn_ending_while_no_one_looks_is_unseen_until_someone_does() {
    let daemon = TestDaemon::start();
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    phone.request(Request::WatchChats);
    let chat = PaneTerminal::Agent(NewAgent {
        size: Size::new(40, 10).unwrap(),
        cwd: None,
        agent: None,
        tools: AgentTools::Ask,
        task: true,
        browser: false,
        model: None,
        resume: None,
    });
    phone.request(Request::NewWorkspace {
        name: None,
        terminal: chat,
    });
    let chats = phone.chats_until(|chats| {
        chats.len() == 1 && chats[0].state == clackatoo_protocol::ChatState::Ready
    });
    let id = chats[0].chat;
    // Followed, but by no one present: the phone's app is in the
    // background.
    phone.request(Request::Attach { terminal: id });
    phone.send(&ClientMessage::Present { here: false });
    phone.request(Request::Chat {
        terminal: id,
        action: ChatAction::Send("hello".into()),
    });
    let chats = phone.chats_until(|chats| chats[0].unseen);
    assert!(chats[0].needs_you());
    // The app comes to the front, showing the chat.
    phone.send(&ClientMessage::Present { here: true });
    phone.chats_until(|chats| !chats[0].unseen);
    // Seen as it ends, it's never unseen.
    phone.request(Request::Chat {
        terminal: id,
        action: ChatAction::Send("again".into()),
    });
    let mut window = daemon.window();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let view = window.capture(id).unwrap();
        let ended =
            view.chat().unwrap().turns.len() == 2 && view.chat().unwrap().turns[1].end.is_some();
        if ended {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let Response::Chats(chats) = phone.request(Request::WatchChats) else {
        panic!("chats");
    };
    assert!(!chats[0].unseen);
}

/// A self-signed certificate for `localhost`, written into `dir` as PEM
/// files, as `cert.pem` and `key.pem`, renamed into place. Returns its
/// paths and the certificate.
fn make_certificate(dir: &Path) -> (PathBuf, PathBuf, rustls::pki_types::CertificateDer<'static>) {
    let certified = rcgen::generate_simple_self_signed(["localhost".to_owned()]).unwrap();
    let pem = |label: &str, der: &[u8]| {
        use base64::Engine as _;
        let text = base64::engine::general_purpose::STANDARD.encode(der);
        let lines: Vec<&str> = (text.as_bytes().chunks(64))
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect();
        format!(
            "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
            lines.join("\n")
        )
    };
    let (certificate, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    let write = |path: &Path, text: String| {
        let partial = path.with_extension("partial");
        std::fs::write(&partial, text).unwrap();
        std::fs::rename(&partial, path).unwrap();
    };
    write(&certificate, pem("CERTIFICATE", certified.cert.der()));
    write(
        &key,
        pem("PRIVATE KEY", &certified.signing_key.serialize_der()),
    );
    (certificate, key, certified.cert.der().clone())
}

/// `GET /session` from the web app at `address` over TLS, trusting only
/// `trusted`.
fn session_over_tls(
    address: SocketAddr,
    trusted: &rustls::pki_types::CertificateDer<'static>,
) -> std::io::Result<String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(trusted.clone()).unwrap();
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connection =
        rustls::ClientConnection::new(std::sync::Arc::new(config), "localhost".try_into().unwrap())
            .unwrap();
    let stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut tls = rustls::StreamOwned::new(connection, stream);
    let port = address.port();
    write!(
        tls,
        "GET /session HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut answer = String::new();
    tls.read_to_string(&mut answer)?;
    Ok(answer)
}

#[test]
fn the_public_app_is_served_over_tls_and_its_certificate_read_again_when_renewed() {
    let pems = tempfile::tempdir().unwrap();
    let (certificate, key, first) = make_certificate(pems.path());
    let daemon = TestDaemon::start_with(|web| {
        web.mode = WebMode::Public { certificate, key };
        web.reload_every = Duration::from_millis(100);
    });
    let Target::Tcp(address) = daemon.web else {
        panic!("public listens on the network");
    };
    let answer = session_over_tls(address, &first).expect("the whole answer, then TLS's close");
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(answer.contains("strict-transport-security"), "{answer}");
    assert!(answer.contains("\"state\":\"unpaired\""), "{answer}");
    // Tailscale's word means nothing here: it's anyone's to write.
    assert!(answer.contains("\"tailscale\":false"), "{answer}");

    // Renewed: new connections are shown the new certificate.
    let (_, _, renewed) = make_certificate(pems.path());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if session_over_tls(address, &renewed).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the certificate wasn't read again"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        session_over_tls(address, &first).is_err(),
        "the old one's gone"
    );
}

#[test]
fn a_connection_waiting_for_a_window_cant_send_much() {
    let daemon = TestDaemon::start();
    let asked = daemon
        .post("/pair", r#"{"name":"phone","ask":true}"#, |_| {})
        .json();
    let wait = asked["wait"].as_str().unwrap().to_owned();
    let mut conn = Conn::open(&daemon.web);
    let body = format!(r#"{{"wait":"{wait}"}}"#);
    conn.write_all(request_bytes("POST", "/pair/wait", &daemon.app(), &body).as_bytes())
        .unwrap();
    // While it waits, a flood: it's cut off, not held.
    conn.set_write_timeout(Some(Duration::from_secs(5)));
    let _ = conn.write_all(&vec![b'x'; 1 << 20]);
    assert!(closed_within(&mut conn, Duration::from_secs(3)));
}

#[test]
fn a_request_trickling_in_is_given_up_on() {
    let daemon = TestDaemon::start_with(|web| web.head_wait = Duration::from_millis(300));
    let mut conn = Conn::open(&daemon.web);
    conn.write_all(b"GET / HTTP/1.1\r\n").unwrap();
    let started = Instant::now();
    let mut closed = false;
    while started.elapsed() < Duration::from_secs(3) {
        if conn.write_all(b"X-Slow: y\r\n").is_err() {
            closed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        conn.set_read_timeout(Some(Duration::from_millis(10)));
        let mut byte = [0; 1];
        if matches!(conn.read(&mut byte), Ok(0)) {
            closed = true;
            break;
        }
    }
    assert!(closed, "a trickled request was waited for");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn each_address_has_password_tries_of_its_own() {
    let daemon = TestDaemon::start();
    let from = |address: &'static str| {
        move |headers: &mut Vec<(String, String)>| {
            headers.push(("X-Forwarded-For".into(), address.into()));
        }
    };
    let wrong = r#"{"name":"phone","password":"hunter2"}"#;
    for _ in 0..5 {
        assert_eq!(daemon.post("/pair", wrong, from("100.64.0.9")).status, 403);
    }
    assert_eq!(daemon.post("/pair", wrong, from("100.64.0.9")).status, 429);
    // Someone else, elsewhere, isn't locked out by it.
    let right = format!(r#"{{"name":"phone","password":"{PASSWORD}"}}"#);
    let answer = daemon.post("/pair", &right, from("100.64.0.10"));
    assert_eq!(answer.status, 200, "{}", answer.text());
}

#[test]
fn the_first_login_is_let_in_only_from_a_window() {
    let daemon = TestDaemon::start_with(|web| web.login = None);
    let mut window = daemon.window();
    let right = format!(r#"{{"name":"phone","password":"{PASSWORD}"}}"#);
    let as_eve = |headers: &mut Vec<(String, String)>| {
        headers.retain(|(name, _)| name != "Tailscale-User-Login");
        headers.push(("Tailscale-User-Login".into(), "eve@github".into()));
    };
    // Whoever has the password first doesn't become the only login.
    let grab = daemon.post("/pair", &right, as_eve);
    assert_eq!(grab.status, 403, "{}", grab.text());
    assert!(grab.cookie().is_none());
    let asked = daemon
        .post("/pair", r#"{"name":"phone","ask":true}"#, |_| {})
        .json();
    let (wait, code) = (
        asked["wait"].as_str().unwrap(),
        asked["code"].as_str().unwrap(),
    );
    window
        .request(&Request::AnswerPairing {
            code: Some(code.to_owned()),
        })
        .unwrap();
    let paired = daemon.post("/pair/wait", &format!(r#"{{"wait":"{wait}"}}"#), |_| {});
    assert_eq!(paired.status, 200, "{}", paired.text());
    // Now the login a window let in pairs more with the password, and no
    // one else does.
    assert_eq!(daemon.post("/pair", &right, |_| {}).status, 200);
    assert_eq!(daemon.post("/pair", &right, as_eve).status, 403);
}

#[test]
fn a_device_pinging_without_reading_is_cut_off() {
    let daemon = TestDaemon::start();
    let cookie = daemon.pair();
    let mut socket = daemon.raw_socket(&cookie, |_| {}).expect("let in");
    socket
        .get_mut()
        .set_write_timeout(Some(Duration::from_millis(500)));
    // Its pongs pile up unread, a few kilobytes at most before it's cut
    // off: not as many as the daemon lets an unread client have of what
    // it asked for.
    let mut pings = 0;
    loop {
        assert!(
            pings < 16_000,
            "{pings} pings were taken while their pongs went unread"
        );
        let ping = Message::Ping(vec![7; 125].into());
        match socket.send(ping) {
            Ok(()) => pings += 1,
            // Its own sending backs up, unread: it goes on trying.
            Err(tungstenite::Error::Io(err))
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
}

#[test]
fn a_device_whose_password_lapses_is_cut_off_until_it_gives_it_again() {
    let daemon = TestDaemon::start_with(|web| web.password_every = Some(Duration::from_secs(1)));
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    let started = Instant::now();
    assert_eq!(phone.receive(), Err(Some(CloseCode::from(4002))));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(daemon.socket(&cookie, |_| {}).err(), Some(401));
    let mut headers = daemon.app();
    headers.push(("Cookie".into(), cookie.clone()));
    assert_eq!(
        http(&daemon.web, "GET", "/session", &headers, "").json()["state"],
        "renew"
    );
    let renew = format!(r#"{{"password":"{PASSWORD}","renew":true}}"#);
    let cookie_too = cookie.clone();
    let answer = daemon.post("/pair", &renew, move |headers| {
        headers.push(("Cookie".into(), cookie_too));
    });
    assert_eq!(answer.status, 200, "{}", answer.text());
    assert!(daemon.socket(&cookie, |_| {}).is_ok());
}

#[test]
fn a_device_unused_too_long_is_cut_off_and_unpaired() {
    let daemon = TestDaemon::start_with(|web| web.expire_after = Duration::from_secs(1));
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    let started = Instant::now();
    assert_eq!(phone.receive(), Err(Some(CloseCode::from(4001))));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(daemon.socket(&cookie, |_| {}).err(), Some(401));
    let Response::Devices(devices) = daemon.window().request(&Request::Devices).unwrap() else {
        panic!("devices");
    };
    assert!(devices.is_empty(), "unpaired");
}

#[test]
fn a_device_cant_upgrade_the_daemon_carry_quic_or_serve_tools() {
    let daemon = TestDaemon::start_daemon(|_| {}, |config| config.mcp = Some(vec!["true".into()]));
    let cookie = daemon.pair();
    // Asking to upgrade in place of hello.
    let mut socket = daemon.raw_socket(&cookie, |_| {}).expect("let in");
    let upgrade = ClientMessage::Upgrade {
        protocol_version: PROTOCOL_VERSION,
        executable: b"/bin/true".to_vec(),
    };
    socket
        .send(Message::binary(
            frame::encode(PROTOCOL_VERSION, &upgrade).unwrap(),
        ))
        .unwrap();
    let mut decoder = FrameDecoder::new();
    let refused = loop {
        if let Some(message) = decoder.decode::<ServerMessage>(PROTOCOL_VERSION).unwrap() {
            break message;
        }
        if let Message::Binary(bytes) = socket.read().unwrap() {
            decoder.push(&bytes);
        }
    };
    assert!(
        matches!(&refused, ServerMessage::NotUpgrading { reason } if reason.contains("machine")),
        "{refused:?}"
    );
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    assert!(matches!(
        phone.request(Request::CarryQuic),
        Response::Error(Error::CantCarry(_))
    ));
    let chat = PaneTerminal::Agent(NewAgent {
        size: Size::new(40, 10).unwrap(),
        cwd: None,
        agent: None,
        tools: AgentTools::Ask,
        task: true,
        browser: false,
        model: None,
        resume: None,
    });
    phone.request(Request::NewWorkspace {
        name: None,
        terminal: chat,
    });
    let Response::Chats(chats) = phone.request(Request::WatchChats) else {
        panic!("chats");
    };
    let refused = phone.request(Request::CarryMcp {
        chat: chats[0].chat,
    });
    assert!(
        matches!(&refused, Response::Error(Error::NoTools(why)) if why.contains("socket")),
        "{refused:?}"
    );
}

#[test]
fn a_device_isnt_given_colours_for_chats_code() {
    let daemon = TestDaemon::start();
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    phone.send(&ClientMessage::Colour {
        id: 1,
        code: vec![clackatoo_protocol::Code {
            language: "json".into(),
            text: "[1]".into(),
        }],
    });
    // Answered in order: by the time a request after it is, it was
    // refused, and nothing came for it.
    assert!(matches!(phone.request(Request::Version), Response::Text(_)));
    assert!(
        !(phone.events.iter()).any(|event| matches!(event, Event::Coloured { .. })),
        "{:?}",
        phone.events
    );
}

#[test]
fn one_address_opens_only_so_many_connections() {
    let pems = tempfile::tempdir().unwrap();
    let (certificate, key, _) = make_certificate(pems.path());
    let daemon = TestDaemon::start_with(|web| {
        web.mode = WebMode::Public { certificate, key };
    });
    let Target::Tcp(address) = daemon.web else {
        panic!("public listens on the network");
    };
    // Each starts TLS's handshake and goes no further.
    let mut open: Vec<Conn> = (0..8)
        .map(|_| Conn::Tcp(TcpStream::connect(address).unwrap()))
        .collect();
    let mut one_more = Conn::Tcp(TcpStream::connect(address).unwrap());
    assert!(
        closed_within(&mut one_more, Duration::from_secs(2)),
        "a ninth from one address is closed at once"
    );
    assert!(!closed_within(&mut open[0], Duration::from_millis(200)));
    // Once one closes, another may open.
    drop(open.pop());
    std::thread::sleep(Duration::from_millis(200));
    let mut another = Conn::Tcp(TcpStream::connect(address).unwrap());
    assert!(!closed_within(&mut another, Duration::from_millis(300)));
}

/// Sets where a request says, as `tailscale serve` does, it came from.
fn forwarded_for(address: &str) -> impl FnOnce(&mut Vec<(String, String)>) + use<> {
    let address = address.to_owned();
    move |headers| headers.push(("X-Forwarded-For".into(), address))
}

#[test]
fn addresses_in_one_ipv6_64_share_their_tries() {
    let daemon = TestDaemon::start();
    let wrong = r#"{"name":"phone","password":"hunter2"}"#;
    for n in 1..=5 {
        let from = forwarded_for(&format!("2001:db8:1:2::{n}"));
        assert_eq!(daemon.post("/pair", wrong, from).status, 403);
    }
    let hopped = daemon.post("/pair", wrong, forwarded_for("2001:db8:1:2:ffff::99"));
    assert_eq!(
        hopped.status, 429,
        "a new address in the same /64 gains nothing"
    );
    let right = format!(r#"{{"name":"phone","password":"{PASSWORD}"}}"#);
    let elsewhere = daemon.post("/pair", &right, forwarded_for("2001:db8:9::1"));
    assert_eq!(elsewhere.status, 200, "{}", elsewhere.text());
}

#[test]
fn tries_from_everywhere_at_once_are_limited_together() {
    let daemon = TestDaemon::start();
    let wrong = r#"{"name":"phone","password":"hunter2"}"#;
    for n in 0..20 {
        let from = forwarded_for(&format!("198.51.100.{n}"));
        assert_eq!(daemon.post("/pair", wrong, from).status, 403);
    }
    let one_more = daemon.post("/pair", wrong, forwarded_for("203.0.113.1"));
    assert_eq!(
        one_more.status, 429,
        "a fresh address is over the daemon's ceiling"
    );
}

#[test]
fn too_many_wrong_passwords_lock_pairing_until_a_window_unlocks_it() {
    let daemon = TestDaemon::start_with(|web| web.lock_after = 3);
    let mut window = daemon.window();
    window.watch_workspaces().unwrap();
    let wrong = r#"{"name":"phone","password":"hunter2"}"#;
    for n in 0..3 {
        let from = forwarded_for(&format!("198.51.100.{n}"));
        assert_eq!(daemon.post("/pair", wrong, from).status, 403);
    }
    let right = format!(r#"{{"name":"phone","password":"{PASSWORD}"}}"#);
    let locked = daemon.post("/pair", &right, forwarded_for("203.0.113.1"));
    assert_eq!(locked.status, 423, "{}", locked.text());
    assert!(locked.cookie().is_none());
    let session = http(&daemon.web, "GET", "/session", &daemon.app(), "").json();
    assert_eq!(
        session["password"], false,
        "the app offers no password while locked"
    );
    // Every window's bar says so.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "the window wasn't told");
        if let Ok(Event::Pairing(pairing)) = window.next_event()
            && pairing.locked
        {
            break;
        }
    }
    // Only a window unlocks it.
    let cookie = daemon.pair_from_a_window();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    assert!(matches!(
        phone.request(Request::UnlockPairing),
        Response::Error(Error::Pairing(_))
    ));
    assert_eq!(
        window.request(&Request::UnlockPairing).unwrap(),
        Response::Text("Devices can pair with the password again".into())
    );
    let answer = daemon.post("/pair", &right, forwarded_for("203.0.113.2"));
    assert_eq!(answer.status, 200, "{}", answer.text());
}

#[test]
fn the_socket_is_kept_to_the_user_and_never_put_where_others_can_write() {
    use std::os::unix::fs::PermissionsExt as _;
    let daemon = TestDaemon::start();
    let Target::Unix(socket) = &daemon.web else {
        panic!("tailscale listens on a socket");
    };
    let mode = std::fs::metadata(socket).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");

    let shared = tempfile::tempdir().unwrap();
    std::fs::set_permissions(shared.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let path = shared.path().join("web.sock");
    let refused = TestDaemon::start_with(|web| web.socket = path.clone());
    assert!(
        !path.exists(),
        "a socket in a directory anyone can write in"
    );
    drop(refused);
}

/// A workspace a window makes, with a shell in it 40 columns by 10 rows,
/// and the shell's terminal.
fn shell_in_a_workspace(window: &mut Client) -> TerminalId {
    let shell = PaneTerminal::New(NewTerminal {
        size: Size::new(40, 10).unwrap(),
        command: Some(vec!["/bin/sh".to_owned()]),
        cwd: None,
        record_to: None,
    });
    let Response::PaneCreated(pane) = (window.request(&Request::NewWorkspace {
        name: None,
        terminal: shell,
    }))
    .unwrap() else {
        panic!("a pane");
    };
    let Response::Workspaces(workspaces) = window.request(&Request::WatchWorkspaces).unwrap()
    else {
        panic!("the workspaces");
    };
    let (_, _, found) = workspaces.find_pane(pane).expect("the pane is there");
    let Content::Terminal(terminal) = found.content else {
        panic!("a terminal");
    };
    terminal
}

/// The text of `grid`'s lines, one after the other.
fn grid_text(grid: &GridNode) -> String {
    let lines: Vec<String> = (grid.lines.iter())
        .map(clackatoo_protocol::Line::text)
        .collect();
    lines.join("\n")
}

/// The size of terminal `terminal`, as a window captures it.
fn size_of(window: &mut Client, terminal: TerminalId) -> Size {
    window
        .capture(terminal)
        .unwrap()
        .grid()
        .expect("a grid")
        .size
}

#[test]
fn a_device_follows_a_terminal_types_into_it_and_reads_its_history() {
    let daemon = TestDaemon::start();
    let mut window = daemon.window();
    let terminal = shell_in_a_workspace(&mut window);
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");

    assert_eq!(phone.request(Request::Attach { terminal }), Response::Done);
    let size = phone.grid_until(|grid| Some(grid.size));
    assert_eq!(size, Size::new(40, 10).unwrap(), "the terminal's own size");
    // Typed as the app types, a key at a time, each its own request.
    for key in "printf 'Z%sZ\\n' typed\r".bytes() {
        assert_eq!(
            phone.request(Request::SendInput {
                terminal,
                bytes: vec![key],
            }),
            Response::Done
        );
    }
    phone.grid_until(|grid| grid_text(grid).contains("ZtypedZ").then_some(()));

    // Enough to scroll lines into history, read back as the app reads it.
    let typed = b"i=0; while [ $i -lt 30 ]; do echo line$i; i=$((i+1)); done\r";
    assert_eq!(
        phone.request(Request::SendInput {
            terminal,
            bytes: typed.to_vec(),
        }),
        Response::Done
    );
    let end = phone.grid_until(|grid| {
        let done = grid_text(grid).contains("line29") && grid.state.history_end > 10;
        done.then_some(grid.state.history_end)
    });
    let Response::History(history) = phone.request(Request::History {
        terminal,
        end,
        count: 200,
    }) else {
        panic!("history");
    };
    assert_eq!(history.first + history.lines.len() as u64, end);
    let text: Vec<String> = (history.lines.iter())
        .map(|line| line.line.text())
        .collect();
    assert!(text.iter().any(|line| line == "line0"), "{text:?}");
    assert!(history.lines.iter().all(|line| line.width == 40));
}

#[test]
fn the_list_says_where_each_terminal_is_and_what_it_runs() {
    let daemon = TestDaemon::start();
    let mut window = daemon.window();
    let terminal = shell_in_a_workspace(&mut window);
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    // As a shell with clackatoo's integration says where it is, and what
    // it starts.
    let typed = "printf '\\033]7;file://desk/tmp/place\\007\\033]133;C;cmdline_url=sleep%%2030\\007'; \
                 sleep 30\r";
    window.send_input(terminal, typed.as_bytes()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let info = loop {
        let Response::Terminals(terminals) = phone.request(Request::ListTerminals) else {
            panic!("terminals");
        };
        let info = (terminals.into_iter())
            .find(|info| info.id == terminal)
            .unwrap();
        if info.command.is_some() {
            break info;
        }
        assert!(Instant::now() < deadline, "the command never showed");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(info.cwd.as_deref(), Some("/tmp/place"));
    assert_eq!(info.command.as_deref(), Some("sleep 30"));
}

#[test]
fn a_phones_fit_holds_until_a_window_types_then_takes_the_latest_size_asked_for() {
    let daemon = TestDaemon::start();
    let mut window = daemon.window();
    let terminal = shell_in_a_workspace(&mut window);
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    assert_eq!(phone.request(Request::Attach { terminal }), Response::Done);
    phone.grid_until(|grid| Some(grid.size));

    let phone_size = Size::new(30, 8).unwrap();
    assert_eq!(
        phone.request(Request::Fit {
            terminal,
            size: Some(phone_size),
            renew: false,
        }),
        Response::Done
    );
    phone.grid_until(|grid| (grid.size == phone_size).then_some(()));
    // Windows ask for their size again whenever anything changes: kept
    // for later, not made.
    window.resize(terminal, Size::new(40, 10).unwrap()).unwrap();
    assert_eq!(size_of(&mut window, terminal), phone_size);
    window.resize(terminal, Size::new(50, 12).unwrap()).unwrap();
    assert_eq!(size_of(&mut window, terminal), phone_size);
    // The phone's own typing leaves it.
    assert_eq!(
        phone.request(Request::SendInput {
            terminal,
            bytes: b"x".to_vec(),
        }),
        Response::Done
    );
    assert_eq!(size_of(&mut window, terminal), phone_size);
    // A window's ends it, at the size last asked for.
    window.send_input(terminal, b"y").unwrap();
    let latest = Size::new(50, 12).unwrap();
    assert_eq!(size_of(&mut window, terminal), latest);
    phone.grid_until(|grid| (grid.size == latest).then_some(()));
    // Resizes are made again.
    window.resize(terminal, Size::new(44, 11).unwrap()).unwrap();
    assert_eq!(size_of(&mut window, terminal), Size::new(44, 11).unwrap());
}

#[test]
fn a_phones_fit_is_put_back_when_it_says_leaves_or_goes() {
    let daemon = TestDaemon::start();
    let mut window = daemon.window();
    let terminal = shell_in_a_workspace(&mut window);
    let own = Size::new(40, 10).unwrap();
    let phone_size = Size::new(30, 8).unwrap();
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    let fit = |phone: &mut Phone, size| {
        assert_eq!(
            phone.request(Request::Fit {
                terminal,
                size,
                renew: false,
            }),
            Response::Done
        );
    };

    // Put back by the phone.
    assert_eq!(phone.request(Request::Attach { terminal }), Response::Done);
    fit(&mut phone, Some(phone_size));
    assert_eq!(size_of(&mut window, terminal), phone_size);
    fit(&mut phone, None);
    assert_eq!(size_of(&mut window, terminal), own);

    // The phone stops following the terminal.
    fit(&mut phone, Some(phone_size));
    assert_eq!(phone.request(Request::Detach { terminal }), Response::Done);
    assert_eq!(size_of(&mut window, terminal), own);

    // Another client can't put back the phone's fit.
    assert_eq!(phone.request(Request::Attach { terminal }), Response::Done);
    fit(&mut phone, Some(phone_size));
    assert_eq!(
        window
            .request(&Request::Fit {
                terminal,
                size: None,
                renew: false,
            })
            .unwrap(),
        Response::Done
    );
    assert_eq!(size_of(&mut window, terminal), phone_size);

    // The phone goes.
    drop(phone);
    let deadline = Instant::now() + Duration::from_secs(10);
    while size_of(&mut window, terminal) != own {
        assert!(Instant::now() < deadline, "the fit was never put back");
        std::thread::sleep(Duration::from_millis(20));
    }

    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    assert_eq!(
        phone.request(Request::Fit {
            terminal: TerminalId(9999),
            size: Some(phone_size),
            renew: false,
        }),
        Response::Error(Error::NoSuchTerminal(TerminalId(9999)))
    );
}

#[test]
fn only_a_client_following_a_terminal_fits_it_and_another_phone_takes_a_fit_over() {
    let daemon = TestDaemon::start();
    let mut window = daemon.window();
    let terminal = shell_in_a_workspace(&mut window);
    let own = Size::new(40, 10).unwrap();
    let cookie = daemon.pair();
    let mut first = daemon.socket(&cookie, |_| {}).expect("let in");
    let mut second = daemon.socket(&cookie, |_| {}).expect("let in");
    let fit = |phone: &mut Phone, cols, rows| {
        phone.request(Request::Fit {
            terminal,
            size: Some(Size::new(cols, rows).unwrap()),
            renew: false,
        })
    };

    // Not following it, a phone can't fit it.
    assert_eq!(
        fit(&mut first, 30, 8),
        Response::Error(Error::NotFollowing(terminal))
    );
    assert_eq!(size_of(&mut window, terminal), own);

    for phone in [&mut first, &mut second] {
        assert_eq!(phone.request(Request::Attach { terminal }), Response::Done);
    }
    assert_eq!(fit(&mut first, 30, 8), Response::Done);
    assert_eq!(fit(&mut second, 36, 9), Response::Done);
    assert_eq!(size_of(&mut window, terminal), Size::new(36, 9).unwrap());
    // The first has no fit to put back now.
    assert_eq!(first.request(Request::Detach { terminal }), Response::Done);
    assert_eq!(size_of(&mut window, terminal), Size::new(36, 9).unwrap());
    // The second's goes back to the size before either.
    assert_eq!(second.request(Request::Detach { terminal }), Response::Done);
    assert_eq!(size_of(&mut window, terminal), own);
}

#[test]
fn an_unpaired_device_holding_a_fit_has_nothing_more_done_and_the_fit_goes_back() {
    let daemon = TestDaemon::start();
    let mut window = daemon.window();
    let terminal = shell_in_a_workspace(&mut window);
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    assert_eq!(phone.request(Request::Attach { terminal }), Response::Done);
    let fitted = Size::new(30, 8).unwrap();
    assert_eq!(
        phone.request(Request::Fit {
            terminal,
            size: Some(fitted),
            renew: false,
        }),
        Response::Done
    );
    let Response::Devices(devices) = phone.request(Request::Devices) else {
        panic!("devices");
    };
    // Unpaired as it asks, with keys sent in the same breath: what came
    // after the request that cut it off isn't acted on, though it was
    // sent before the device heard.
    let mut bytes = Vec::new();
    for request in [
        Request::RevokeDevice {
            device: devices[0].id,
        },
        Request::SendInput {
            terminal,
            bytes: b"printf 'Z%sZ\\n' cut\r".to_vec(),
        },
    ] {
        let message = ClientMessage::Request(request);
        bytes.extend(frame::encode(PROTOCOL_VERSION, &message).unwrap());
    }
    phone.socket.send(Message::binary(bytes)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while size_of(&mut window, terminal) != Size::new(40, 10).unwrap() {
        assert!(Instant::now() < deadline, "the fit never went back");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    let screen = window.capture(terminal).unwrap();
    let text = grid_text(screen.grid().unwrap());
    assert!(
        !text.contains("ZcutZ") && !text.contains("printf"),
        "{text}"
    );
}

#[test]
fn asking_to_keep_a_fit_never_brings_one_back_once_its_put_back() {
    let daemon = TestDaemon::start();
    let mut window = daemon.window();
    let terminal = shell_in_a_workspace(&mut window);
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    assert_eq!(phone.request(Request::Attach { terminal }), Response::Done);
    let fitted = Size::new(30, 8).unwrap();
    let ask = |phone: &mut Phone, renew| {
        phone.request(Request::Fit {
            terminal,
            size: Some(fitted),
            renew,
        })
    };
    assert_eq!(ask(&mut phone, false), Response::Done);
    assert_eq!(ask(&mut phone, true), Response::Done, "kept");
    assert_eq!(size_of(&mut window, terminal), fitted);
    // A window types: put back, and asking to keep it is told so.
    window.send_input(terminal, b"x").unwrap();
    assert_eq!(size_of(&mut window, terminal), Size::new(40, 10).unwrap());
    assert_eq!(
        ask(&mut phone, true),
        Response::Error(Error::NotFitted(terminal))
    );
    assert_eq!(size_of(&mut window, terminal), Size::new(40, 10).unwrap());

    // A window the fitted size: put back to the same size, which no
    // snapshot shows, and asking to keep it is still told it's gone, so
    // the phone stops asking rather than fit it again.
    assert_eq!(ask(&mut phone, false), Response::Done);
    window.resize(terminal, fitted).unwrap();
    window.send_input(terminal, b"y").unwrap();
    assert_eq!(
        ask(&mut phone, true),
        Response::Error(Error::NotFitted(terminal))
    );
    // Not fitted any more, a window's resize is made at once.
    window.resize(terminal, Size::new(44, 11).unwrap()).unwrap();
    assert_eq!(size_of(&mut window, terminal), Size::new(44, 11).unwrap());
}

#[test]
fn a_fit_not_asked_for_again_lapses() {
    let daemon = TestDaemon::start_daemon(
        |_| {},
        |config| config.fit_lasts = Duration::from_millis(300),
    );
    let mut window = daemon.window();
    let terminal = shell_in_a_workspace(&mut window);
    let cookie = daemon.pair();
    let mut phone = daemon.socket(&cookie, |_| {}).expect("let in");
    assert_eq!(phone.request(Request::Attach { terminal }), Response::Done);
    let fitted = Size::new(30, 8).unwrap();
    assert_eq!(
        phone.request(Request::Fit {
            terminal,
            size: Some(fitted),
            renew: false,
        }),
        Response::Done
    );
    assert_eq!(size_of(&mut window, terminal), fitted);
    // The phone is put away; its connection lingers, saying nothing.
    let deadline = Instant::now() + Duration::from_secs(10);
    while size_of(&mut window, terminal) != Size::new(40, 10).unwrap() {
        assert!(Instant::now() < deadline, "the fit never lapsed");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        phone.request(Request::Fit {
            terminal,
            size: Some(fitted),
            renew: true,
        }),
        Response::Error(Error::NotFitted(terminal))
    );
}
