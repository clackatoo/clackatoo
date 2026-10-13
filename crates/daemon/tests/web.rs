//! The web app, end to end: a real daemon, listening as it would behind
//! `tailscale serve`, asked over HTTP and WebSocket connections as a phone would ask
//! it, with someone else's WebSocket client (tungstenite).

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_daemon::web::{WebConfig, WebMode};
use clackatoo_daemon::{AgentConfig, Client, CustomAgent, Daemon, DaemonConfig, ShutdownHandle};
use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    AgentTools, ChatAction, ChatSummary, ClientMessage, Error, Event, Item, NewAgent,
    PROTOCOL_VERSION, PaneTerminal, Request, Response, ServerMessage, Size, View, ViewUpdate,
};
use tungstenite::client::IntoClientRequest as _;
use tungstenite::http::HeaderValue;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message, WebSocket};

/// The password the test daemons have.
const PASSWORD: &str = "correct horse battery staple";

/// The Tailscale login the tests' requests come from.
const LOGIN: &str = "jack@github";

/// A daemon with the web app on, on its own thread, in its own temporary
/// directory.
struct TestDaemon {
    dir: tempfile::TempDir,
    web: SocketAddr,
    shutdown: ShutdownHandle,
    thread: Option<JoinHandle<()>>,
}

impl TestDaemon {
    /// A daemon reached through Tailscale, with a password.
    fn start() -> Self {
        Self::start_with(|_| {})
    }

    /// A daemon reached through Tailscale, with a password, and the web
    /// app's settings changed by `change`.
    fn start_with(change: impl FnOnce(&mut WebConfig)) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let agent = vec![
            example("fake_agent").into(),
            "--log".into(),
            dir.path().join("agent.log").into(),
        ];
        let mut web = WebConfig::new(WebMode::Tailscale, dir.path().join("web"));
        web.address = "127.0.0.1:0".parse().unwrap();
        web.password = Some(clackatoo_daemon::web::hash_password(PASSWORD));
        change(&mut web);
        let config = DaemonConfig {
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
            server_idle: Duration::ZERO,
            prompt_wait: clackatoo_daemon::DEFAULT_PROMPT_WAIT,
            probe_wait: clackatoo_daemon::DEFAULT_PROBE_WAIT,
            park_after: clackatoo_daemon::DEFAULT_PARK_AFTER,
            rename_gate: None,
            trash: None,
            boards: None,
            browser: None,
            web: Some(web),
        };
        let mut daemon = Daemon::bind(&dir.path().join("daemon.sock"), config).expect("bind");
        let web = daemon.web_address().expect("the web app listens");
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
        vec![
            ("Host".into(), format!("localhost:{}", self.web.port())),
            (
                "Origin".into(),
                format!("https://localhost:{}", self.web.port()),
            ),
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
        http(self.web, "POST", path, &sent, body)
    }

    /// Pairs a device with the password, returning its cookie.
    fn pair(&self) -> String {
        let body = format!(r#"{{"name":"phone","password":"{PASSWORD}"}}"#);
        let answer = self.post("/pair", &body, |_| {});
        assert_eq!(answer.status, 200, "{}", answer.text());
        answer.cookie().expect("pairing gives a cookie")
    }

    /// Opens a WebSocket carrying `cookie`, with the app's headers changed
    /// by `headers`.
    fn socket(
        &self,
        cookie: &str,
        headers: impl FnOnce(&mut Vec<(String, String)>),
    ) -> Result<Phone, u16> {
        let mut sent = self.app();
        sent.push(("Cookie".into(), cookie.to_owned()));
        headers(&mut sent);
        let mut request = format!("ws://localhost:{}/ws", self.web.port())
            .into_client_request()
            .unwrap();
        for (name, value) in sent {
            let name: tungstenite::http::HeaderName = name.parse().unwrap();
            request
                .headers_mut()
                .insert(name, HeaderValue::from_str(&value).unwrap());
        }
        let stream = TcpStream::connect(self.web).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        match tungstenite::client(request, stream) {
            Ok((socket, _)) => Ok(Phone::new(socket)),
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

/// Asks the web app at `address` with `method`, `path`, `headers` and
/// `body`, on a connection of its own.
fn http(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &str,
) -> Http {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(150)))
        .unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    for (name, value) in headers {
        let _ = write!(request, "{name}: {value}\r\n");
    }
    let _ = write!(request, "Content-Length: {}\r\n\r\n{body}", body.len());
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
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

/// A device's WebSocket, speaking the protocol, as the app does.
struct Phone {
    socket: WebSocket<TcpStream>,
    decoder: FrameDecoder,
    events: VecDeque<Event>,
    /// The view of the chat it follows, kept current.
    view: Option<View>,
}

impl Phone {
    /// A device that has said hello.
    fn new(socket: WebSocket<TcpStream>) -> Self {
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
        let frame = frame::encode(message).unwrap();
        self.socket.send(Message::binary(frame)).unwrap();
    }

    /// The next message, or the close that came instead.
    fn receive(&mut self) -> Result<ServerMessage, Option<CloseCode>> {
        loop {
            if let Some(message) = self.decoder.decode().unwrap() {
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
    let page = http(daemon.web, "GET", "/", &daemon.app(), "");
    assert_eq!(page.status, 200);
    let policy = page.header("content-security-policy").unwrap();
    assert!(policy.contains("default-src 'none'"), "{policy}");
    assert!(policy.contains("frame-ancestors 'none'"), "{policy}");
    assert_eq!(page.header("x-content-type-options"), Some("nosniff"));
    assert!(
        page.text().contains("without its web app"),
        "no app in tests"
    );
    let missing = http(daemon.web, "GET", "/nothing.js", &daemon.app(), "");
    assert_eq!(missing.status, 404);
}

#[test]
fn a_device_pairs_with_the_password_and_connects() {
    let daemon = TestDaemon::start();
    let session = http(daemon.web, "GET", "/session", &daemon.app(), "").json();
    assert_eq!(session["state"], "unpaired");
    assert_eq!(session["password"], true);

    let cookie = daemon.pair();
    assert!(cookie.starts_with("__Host-clackatoo="), "{cookie}");
    let mut headers = daemon.app();
    headers.push(("Cookie".into(), cookie.clone()));
    let session = http(daemon.web, "GET", "/session", &headers, "").json();
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
    let session = http(daemon.web, "GET", "/session", &headers, "").json();
    assert_eq!(session["state"], "unpaired");
}

#[test]
fn a_page_elsewhere_is_refused() {
    let daemon = TestDaemon::start();
    let cookie = daemon.pair();
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
    assert_eq!(http(daemon.web, "GET", "/", &headers, "").status, 403);
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

    let Event::Pairing(asking) = window.next_event().unwrap() else {
        panic!("the window is asked");
    };
    assert_eq!(asking.len(), 1);
    assert_eq!(asking[0].name, "jack's iPhone");
    assert_eq!(asking[0].from, LOGIN);

    // The device waits for the answer meanwhile.
    let waiting = {
        let daemon_web = daemon.web;
        let headers = daemon.app();
        let body = format!(r#"{{"wait":"{wait}"}}"#);
        std::thread::spawn(move || http(daemon_web, "POST", "/pair/wait", &headers, &body))
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

#[test]
fn the_public_app_is_served_over_tls_with_its_own_certificate() {
    let pems = tempfile::tempdir().unwrap();
    let certified = rcgen::generate_simple_self_signed(["localhost".to_owned()]).unwrap();
    let (certificate, key) = (pems.path().join("cert.pem"), pems.path().join("key.pem"));
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
    std::fs::write(&certificate, pem("CERTIFICATE", certified.cert.der())).unwrap();
    let der = certified.signing_key.serialize_der();
    std::fs::write(&key, pem("PRIVATE KEY", &der)).unwrap();
    let daemon = TestDaemon::start_with(|web| {
        web.mode = WebMode::Public { certificate, key };
    });

    let mut roots = rustls::RootCertStore::empty();
    roots.add(certified.cert.der().clone()).unwrap();
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connection =
        rustls::ClientConnection::new(std::sync::Arc::new(config), "localhost".try_into().unwrap())
            .unwrap();
    let stream = TcpStream::connect(daemon.web).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut tls = rustls::StreamOwned::new(connection, stream);
    let port = daemon.web.port();
    write!(
        tls,
        "GET /session HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    tls.read_to_string(&mut answer)
        .expect("the whole answer, then TLS's close");
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(answer.contains("strict-transport-security"), "{answer}");
    assert!(answer.contains("\"state\":\"unpaired\""), "{answer}");
    // Tailscale's word means nothing here: it's anyone's to write.
    assert!(answer.contains("\"tailscale\":false"), "{answer}");
}
