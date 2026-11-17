//! A whole proxy, between a real command server and a stand-in for the
//! daemon that answers as each test says: what the agent sees when the
//! server or the daemon goes, and comes back.
use std::io::{BufRead as _, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    ClientMessage, Error, McpProxyAction, McpState, PROTOCOL_VERSION, Request, Response,
    ServerMessage, TerminalId,
    frame::{self, FrameDecoder},
};
use serde_json::{Value, json};

use super::super::config::{self, Registry};
use super::super::proxy::{
    self, Grant,
    control::{Daemon, Pace},
};
use super::{roots, write};

/// How long a test waits for anything before it fails.
const PATIENCE: Duration = Duration::from_secs(20);

/// The server every test here runs: it appends its process ID to the
/// file it's given as it starts, and has three tools. `echo` answers with
/// the request as it got it; `big` with five megabytes; `crash` exits.
const SERVER: &str = r"import sys, json, os
with open(sys.argv[1], 'a') as started: started.write(str(os.getpid()) + '\n')
for line in sys.stdin:
    message = json.loads(line)
    method = message.get('method')
    if method == 'initialize':
        result = {'protocolVersion': '2025-06-18', 'capabilities': {'tools': {}, 'vendor': {'a': 1}},
                  'serverInfo': {'name': 'fixture', 'version': '1', 'vendorInfo': [1, 2]}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'echo', 'description': 'd', 'inputSchema': {'type': 'object'},
                             'vendorField': {'k': 1}}], 'vendorTop': True}
    elif method == 'tools/call':
        name = message['params']['name']
        if name == 'crash':
            sys.stderr.write('boom\n')
            sys.exit(3)
        elif name == 'big':
            result = {'content': [{'type': 'text', 'text': 'x' * (5 << 20)}]}
        else:
            result = {'content': [{'type': 'text', 'text': 'echo'}], 'got': message, 'vendorResult': {'deep': [None]}}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': message['id'], 'result': result}), flush=True)
";

/// What the stand-in daemon answers a call with.
#[derive(Clone)]
enum Leave {
    Allow,
    Refuse(String),
}

/// What the stand-in daemon is told, and is to say.
struct Told {
    grant: Grant,
    /// Why the proxy must stop, once it must.
    refusal: Option<String>,
    leave: Leave,
    /// How many times a watch was opened.
    opens: usize,
    reports: Vec<(McpState, String)>,
    /// Whether it's there: taking connections, and keeping those it has.
    up: bool,
    /// Whether its loop is stuck: it takes connections and says nothing
    /// on them.
    stuck: bool,
    /// Whether it drops each watch as soon as it has granted it.
    dropping: bool,
}

/// A stand-in for the daemon, at a socket of its own.
struct FakeDaemon {
    socket: PathBuf,
    told: Arc<(Mutex<Told>, Condvar)>,
    listener: Option<std::thread::JoinHandle<()>>,
}

impl FakeDaemon {
    fn new(socket: PathBuf, grant: Grant) -> Self {
        let told = Told {
            grant,
            refusal: None,
            leave: Leave::Allow,
            opens: 0,
            reports: Vec::new(),
            up: false,
            stuck: false,
            dropping: false,
        };
        let mut daemon = Self {
            socket,
            told: Arc::new((Mutex::new(told), Condvar::new())),
            listener: None,
        };
        daemon.start();
        daemon
    }

    /// Changes what it's told, and wakes whatever waits on that.
    fn change(&self, change: impl FnOnce(&mut Told)) {
        change(&mut self.told.0.lock().unwrap());
        self.told.1.notify_all();
    }

    /// Waits until what it's told is as `done` says.
    fn wait_until(&self, what: &str, done: impl Fn(&Told) -> bool) {
        let told = self.told.0.lock().unwrap();
        let (told, timed_out) = (self.told.1)
            .wait_timeout_while(told, PATIENCE, |told| !done(told))
            .unwrap();
        assert!(!timed_out.timed_out(), "{what}: {:?}", told.reports);
    }

    /// Starts taking connections.
    fn start(&mut self) {
        let listener = UnixListener::bind(&self.socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        self.change(|told| told.up = true);
        let told = Arc::clone(&self.told);
        self.listener = Some(std::thread::spawn(move || {
            loop {
                if !told.0.lock().unwrap().up {
                    return;
                }
                if let Ok((stream, _)) = listener.accept() {
                    stream.set_nonblocking(false).unwrap();
                    let told = Arc::clone(&told);
                    std::thread::spawn(move || serve(stream, &told));
                } else {
                    let (lock, changed) = &*told;
                    let wait = Duration::from_millis(5);
                    drop(changed.wait_timeout(lock.lock().unwrap(), wait).unwrap());
                }
            }
        }));
    }

    /// Goes away, as a daemon that stops or upgrades does: every
    /// connection closes, and the socket takes no more.
    fn stop(&mut self) {
        self.change(|told| told.up = false);
        self.listener.take().unwrap().join().unwrap();
        // Gone already if the test's directory was dropped first.
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        if self.listener.is_some() {
            self.stop();
        }
    }
}

/// Answers one connection's requests until it closes or the daemon goes.
fn serve(mut stream: UnixStream, told: &(Mutex<Told>, Condvar)) {
    // Read in short waits, so a daemon that goes drops the connection
    // even while nothing is being said on it.
    stream
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let mut decoder = FrameDecoder::new();
    let mut bytes = [0; 16384];
    loop {
        let message = loop {
            let (up, stuck) = {
                let told = told.0.lock().unwrap();
                (told.up, told.stuck)
            };
            if !up {
                return;
            }
            // Stuck, it reads nothing: what was sent waits in the socket.
            if stuck {
                std::thread::park_timeout(Duration::from_millis(5));
                continue;
            }
            if let Some(message) = decoder.decode::<ClientMessage>(PROTOCOL_VERSION).unwrap() {
                break message;
            }
            match stream.read(&mut bytes) {
                Ok(0) => return,
                Ok(n) => decoder.push(&bytes[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            }
        };
        let answer = match message {
            ClientMessage::Hello { .. } => ServerMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
            ClientMessage::Request(Request::McpProxy { action, .. }) => {
                let Some(response) = answer(action, told) else {
                    return;
                };
                ServerMessage::Response(response)
            }
            other => panic!("a proxy never says {other:?}"),
        };
        let granted = matches!(answer, ServerMessage::Response(Response::McpConnection(_)));
        let frame = frame::encode(PROTOCOL_VERSION, &answer).unwrap();
        if stream.write_all(&frame).is_err() {
            return;
        }
        if granted && told.0.lock().unwrap().dropping {
            return;
        }
    }
}

/// The daemon's answer to `action`, or none if it went while a
/// heartbeat waited.
fn answer(action: McpProxyAction, told: &(Mutex<Told>, Condvar)) -> Option<Response> {
    let (lock, changed) = told;
    let mut told = lock.lock().unwrap();
    let grant = |told: &Told| Response::McpConnection(serde_json::to_string(&told.grant).unwrap());
    let refused = |why: &str| Response::Error(Error::Other(why.to_owned()));
    match action {
        McpProxyAction::Open => {
            told.opens += 1;
            changed.notify_all();
            Some(match &told.refusal {
                Some(why) => refused(why),
                None => grant(&told),
            })
        }
        // Held until there's news, as the daemon holds it.
        McpProxyAction::Heartbeat { revision, .. } => loop {
            if !told.up {
                return None;
            }
            if let Some(why) = &told.refusal {
                return Some(refused(why));
            }
            if told.grant.revision() != revision {
                return Some(grant(&told));
            }
            told = changed
                .wait_timeout(told, Duration::from_millis(20))
                .unwrap()
                .0;
        },
        McpProxyAction::Check { .. } => Some(match &told.leave {
            Leave::Allow => Response::Done,
            Leave::Refuse(why) => refused(why),
        }),
        McpProxyAction::Report { state, detail, .. } => {
            told.reports.push((state, detail));
            changed.notify_all();
            Some(Response::Done)
        }
    }
}

/// A proxy under test, with the agent's ends of its pipes.
struct Running {
    /// Kept so the server's files outlive the test's use of them.
    _dir: tempfile::TempDir,
    daemon: FakeDaemon,
    /// Where the server writes the process ID of each start.
    started: PathBuf,
    input: Option<std::io::PipeWriter>,
    output: std::sync::mpsc::Receiver<Value>,
    proxy: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl Running {
    /// Starts a proxy for [`SERVER`], asking as `pace` says, with the
    /// agent's handshake done.
    fn start(pace: Pace) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        let started = dir.path().join("started");
        let definition = json!({"command":"python3","args":["-u","-c",SERVER,started]});
        write(
            &config::personal(&roots).join("mcp.json"),
            &json!({"servers":{"fixture":definition}}).to_string(),
        );
        let bound = (Registry::discover(&roots, &std::collections::BTreeSet::new()).servers)
            .remove("fixture")
            .unwrap();
        let grant = Grant {
            bound,
            generation: 0,
        };
        let socket = dir.path().join("daemon.sock");
        let daemon = FakeDaemon::new(socket.clone(), grant);
        let (agent_reads, proxy_writes) = std::io::pipe().unwrap();
        let (proxy_reads, agent_writes) = std::io::pipe().unwrap();
        let control = Daemon {
            socket,
            chat: TerminalId(1),
            server: "fixture".into(),
            token: "secret".into(),
            pace,
        };
        let proxy = std::thread::spawn(move || {
            let input = std::fs::File::from(std::os::fd::OwnedFd::from(proxy_reads));
            let output = std::fs::File::from(std::os::fd::OwnedFd::from(proxy_writes));
            let runtime = super::super::transport::runtime().unwrap();
            runtime.block_on(proxy::run(control, input, output))
        });
        let (said, output) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(agent_reads).lines() {
                let Ok(line) = line else { break };
                if said.send(serde_json::from_str(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        let mut running = Self {
            _dir: dir,
            daemon,
            started,
            input: Some(agent_writes),
            output,
            proxy: Some(proxy),
        };
        running.send(
            &json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"agent","version":"1"}}}),
        );
        let greeting = running.reply(&json!("init"));
        // The server's own answer, with what the SDK's model would drop.
        assert_eq!(greeting["result"]["capabilities"]["vendor"]["a"], 1);
        assert_eq!(
            greeting["result"]["serverInfo"]["vendorInfo"],
            json!([1, 2])
        );
        running.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        running
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.input.as_mut().unwrap(), "{message}").unwrap();
    }

    /// The proxy's next message.
    fn next(&self) -> Value {
        self.output
            .recv_timeout(PATIENCE)
            .expect("the proxy says something")
    }

    /// The answer to the request `id`, passing over what comes before.
    fn reply(&self, id: &Value) -> Value {
        loop {
            let message = self.next();
            if message.get("method").is_none() && message["id"] == *id {
                return message;
            }
        }
    }

    /// Waits to be told the notification `method`.
    fn notified(&self, method: &str) {
        while self.next()["method"] != method {}
    }

    /// Calls the tool `name` as the request `id`, and returns the answer.
    fn call(&mut self, id: u32, name: &str) -> Value {
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name}}));
        self.reply(&json!(id))
    }

    /// The process IDs of the server, one for each time it started.
    fn starts(&self) -> Vec<String> {
        let started = std::fs::read_to_string(&self.started).unwrap_or_default();
        started.lines().map(str::to_owned).collect()
    }

    /// Closes the agent's end, and returns how the proxy ended.
    fn finish(mut self) -> Result<(), String> {
        self.input.take();
        self.ended()
    }

    /// How the proxy ended, once it has.
    fn ended(&mut self) -> Result<(), String> {
        let proxy = self.proxy.take().unwrap();
        let deadline = Instant::now() + PATIENCE;
        while !proxy.is_finished() {
            assert!(Instant::now() < deadline, "the proxy is still running");
            std::thread::park_timeout(Duration::from_millis(5));
        }
        proxy.join().unwrap()
    }
}

/// How a proxy asks in these tests: often, giving up after `give_up`.
fn quick(give_up: Duration) -> Pace {
    Pace {
        beat: Duration::from_millis(50),
        retry: Duration::from_millis(10),
        longest_retry: Duration::from_millis(40),
        give_up,
        greeting: Duration::from_millis(200),
        asking: Duration::from_millis(600),
        opening: Duration::from_secs(5),
    }
}

fn exists(path: &Path) -> bool {
    path.exists()
}

#[test]
fn fields_the_sdks_model_doesnt_know_survive_the_proxy_both_ways() {
    let mut running = Running::start(quick(PATIENCE));
    // An ID the SDK has no type for, parameters it would drop, and
    // fields beside them it would refuse.
    let request = json!({"jsonrpc":"2.0","id":7.5,"method":"tools/call","vendorBeside":1,"params":{
        "name":"echo","arguments":{"a":[1,{"b":null}]},"vendorParam":{"x":true},
        "_meta":{"progressToken":"t","vendorMeta":2}}});
    running.send(&request);
    let answer = running.reply(&json!(7.5));
    assert_eq!(answer["result"]["got"], request, "what the server was sent");
    assert_eq!(answer["result"]["vendorResult"], json!({"deep":[null]}));
    running.send(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let listed = running.reply(&json!(2));
    assert_eq!(listed["result"]["tools"][0]["vendorField"], json!({"k":1}));
    assert_eq!(listed["result"]["vendorTop"], true);
    assert_eq!(running.finish(), Ok(()));
}

#[test]
fn an_answer_too_large_is_told_as_such_and_the_proxy_carries_on() {
    let mut running = Running::start(quick(PATIENCE));
    let answer = running.call(1, "big");
    let why = answer["error"]["message"].as_str().unwrap();
    assert!(
        why.contains("6 MiB") && why.contains("at most 4 MiB"),
        "{why}"
    );
    // Once this ended the proxy without a word. The same server answers.
    assert_eq!(
        running.call(2, "echo")["result"]["content"][0]["text"],
        "echo"
    );
    assert_eq!(running.starts().len(), 1);
    // A line that isn't JSON is told, and passed over.
    writeln!(running.input.as_mut().unwrap(), "not json").unwrap();
    assert_eq!(running.next()["error"]["code"], -32700);
    assert_eq!(
        running.call(3, "echo")["result"]["content"][0]["text"],
        "echo"
    );
}

#[test]
fn a_server_that_crashes_is_started_again_and_the_agent_goes_on_with_it() {
    let mut running = Running::start(quick(PATIENCE));
    let answer = running.call(1, "crash");
    let why = answer["error"]["message"].as_str().unwrap();
    assert!(
        why.contains("isn't known") && why.contains("exit status: 3") && why.contains("boom"),
        "{why}"
    );
    running.daemon.wait_until("the crash is reported", |told| {
        (told.reports.iter())
            .any(|(state, detail)| *state == McpState::Error && detail.contains("boom"))
    });
    // It's started again, initialized as the agent initialized the
    // first, and the agent told only that its tools may have changed.
    running.notified("notifications/tools/list_changed");
    assert_eq!(
        running.call(2, "echo")["result"]["content"][0]["text"],
        "echo"
    );
    let starts = running.starts();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_ne!(starts[0], starts[1]);
    running
        .daemon
        .wait_until("it's reported connected again", |told| {
            matches!(told.reports.last(), Some((McpState::Connected, _)))
        });
    assert_eq!(running.finish(), Ok(()));
}

#[test]
fn a_daemon_that_goes_and_comes_back_as_in_an_upgrade_leaves_the_server_running() {
    let mut running = Running::start(quick(PATIENCE));
    assert_eq!(
        running.call(1, "echo")["result"]["content"][0]["text"],
        "echo"
    );
    running.daemon.stop();
    // Calls can't be checked, and say so, in words that say to try
    // again; what needs no leave still reaches the server.
    let answer = running.call(2, "echo");
    let why = answer["error"]["message"].as_str().unwrap();
    assert!(
        why.contains("daemon isn't answering") && why.contains("Try again"),
        "{why}"
    );
    running.send(&json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}));
    assert_eq!(
        running.reply(&json!(3))["result"]["tools"][0]["name"],
        "echo"
    );

    running.daemon.start();
    running
        .daemon
        .wait_until("the watch is opened again", |told| told.opens == 2);
    // The watch is back before calls are let through again.
    let deadline = Instant::now() + PATIENCE;
    let mut id = 10;
    loop {
        let answer = running.call(id, "echo");
        if answer.get("result").is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "{answer}");
        id += 1;
    }
    // The same process all along: nothing was connected again.
    assert_eq!(running.starts().len(), 1);
    assert_eq!(running.finish(), Ok(()));
}

#[test]
fn a_daemon_gone_too_long_ends_the_proxy_saying_so() {
    let mut running = Running::start(quick(Duration::from_millis(200)));
    running.daemon.stop();
    let ended = running.ended().unwrap_err();
    assert!(ended.contains("unreachable"), "{ended}");
    assert!(!exists(&running.daemon.socket));
}

#[test]
fn a_new_grant_starts_the_server_again_in_place_and_only_a_refusal_ends_the_proxy() {
    let mut running = Running::start(quick(PATIENCE));
    // As the user asking to reconnect, signing in, or saving a change.
    running.daemon.change(|told| told.grant.generation = 1);
    running.notified("notifications/tools/list_changed");
    assert_eq!(
        running.call(1, "echo")["result"]["content"][0]["text"],
        "echo"
    );
    assert_eq!(running.starts().len(), 2);

    running
        .daemon
        .change(|told| told.leave = Leave::Refuse("The user refused this call.".into()));
    assert_eq!(
        running.call(2, "echo")["error"]["message"],
        "The user refused this call."
    );

    running
        .daemon
        .change(|told| told.refusal = Some("this chat's tools are off".into()));
    assert_eq!(running.ended(), Err("this chat's tools are off".into()));
}

#[test]
fn a_daemon_that_grants_a_watch_and_drops_it_every_time_is_given_up_on_too() {
    let mut running = Running::start(quick(Duration::from_millis(300)));
    // Each watch it grants it drops at once. Once, every grant started
    // the clock again, and the proxy asked for ever.
    running.daemon.change(|told| told.dropping = true);
    running.daemon.change(|told| told.grant.generation = 1);
    let ended = running.ended().unwrap_err();
    assert!(ended.contains("unreachable"), "{ended}");
    assert!(running.daemon.told.0.lock().unwrap().opens > 2);
}

#[test]
fn a_daemon_whose_loop_is_stuck_is_noticed_and_calls_say_to_try_again() {
    let mut running = Running::start(quick(PATIENCE));
    assert_eq!(
        running.call(1, "echo")["result"]["content"][0]["text"],
        "echo"
    );
    // It still takes connections, and says nothing on them: the watch
    // stays open, so only a deadline tells.
    running.daemon.change(|told| told.stuck = true);
    let asked = Instant::now();
    let answer = running.call(2, "echo");
    let why = answer["error"]["message"].as_str().unwrap();
    assert!(
        why.contains("daemon isn't answering") && why.contains("Try again"),
        "{why}"
    );
    assert!(
        asked.elapsed() < PATIENCE / 2,
        "refused soon, not after a day"
    );
    // Unstuck, calls go through again, on the same server.
    running.daemon.change(|told| told.stuck = false);
    assert_eq!(
        running.call(3, "echo")["result"]["content"][0]["text"],
        "echo"
    );
    assert_eq!(running.starts().len(), 1);
    assert_eq!(running.finish(), Ok(()));
}
