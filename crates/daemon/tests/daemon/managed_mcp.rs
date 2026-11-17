use super::*;
use clackatoo_protocol::{McpAction, McpProxyAction, McpReply, McpScope};

fn admin(client: &mut Client, action: McpAction) -> McpReply {
    match client
        .request(&Request::Mcp { pane: None, action })
        .unwrap()
    {
        Response::Mcp(reply) => reply,
        other => panic!("unexpected MCP Settings response: {other:?}"),
    }
}
fn fixture(client: &mut Client, marker: &Path) {
    admin(
        client,
        McpAction::Secret {
            name: "fixture-key".into(),
            value: "isolated-private-key".into(),
        },
    );
    let script = r"import sys,json,os
assert os.environ['KEY'].endswith('-key')
assert 'CLACKATOO_MCP_TOKEN' not in os.environ
with open(sys.argv[1]+'.started','a') as f: f.write(str(os.getpid())+'\n')
deferred=None
for line in sys.stdin:
 m=json.loads(line); method=m.get('method')
 if method=='initialize': result={'protocolVersion':'2025-06-18','capabilities':{'tools':{},'resources':{},'prompts':{}},'serverInfo':{'name':'fixture','version':'1'}}
 elif method=='tools/list': result={'tools':[{'name':'write','description':'Fixture tool','inputSchema':{'type':'object'}}]}
 elif method=='resources/list': result={'resources':[]}
 elif method=='prompts/list': result={'prompts':[]}
 elif method=='tools/call':
  if m.get('params',{}).get('name')=='crash': sys.stderr.write('fixture crashed\n'); sys.exit(3)
  with open(sys.argv[1],'a') as f: f.write('executed\n')
  if m.get('params',{}).get('arguments',{}).get('callback'):
   deferred=m['id'];print(json.dumps({'jsonrpc':'2.0','id':'callback','method':'ping'}),flush=True);continue
  result={'content':[{'type':'text','text':'upstream answered'}]}
 elif method is None and m.get('id')=='callback':
  m['id']=deferred;result={'content':[{'type':'text','text':'callback answered'}]}
 elif method: continue
 else: continue
 print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':result}),flush=True)
";
    admin(client,McpAction::Save { name:"fixture".into(),scope:McpScope::Personal,expected:None,
        settings:serde_json::json!({"command":"python3","args":["-u","-c",script,marker],"env":{"KEY":{"secret":"fixture-key"}}}).to_string() });
}

struct Proxy {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    output: std::sync::mpsc::Receiver<serde_json::Value>,
}
impl Proxy {
    fn new(client: &mut Client, chat: TerminalId) -> Self {
        use std::io::BufRead as _;
        let entries: serde_json::Value =
            serde_json::from_str(&last_words(&ask(client, chat, "tools"))).unwrap();
        let server = entries
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "managed_fixture")
            .unwrap();
        let mut command = std::process::Command::new(server["command"].as_str().unwrap());
        command.args(
            server["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap()),
        );
        for env in server["env"].as_array().unwrap() {
            command.env(
                env["name"].as_str().unwrap(),
                env["value"].as_str().unwrap(),
            );
        }
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let (send, output) = std::sync::mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let value = serde_json::from_str(&line).unwrap();
                if send.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            input: child.stdin.take(),
            child,
            output,
        }
    }
    fn send(&mut self, value: &serde_json::Value) {
        writeln!(self.input.as_mut().unwrap(), "{value}").unwrap();
    }
    fn next(&self) -> serde_json::Value {
        self.output
            .recv_timeout(Duration::from_secs(8))
            .expect("proxy response")
    }
    fn reply(&self, id: u32) -> serde_json::Value {
        loop {
            let value = self.next();
            if value["id"] == id {
                return value;
            }
        }
    }
    /// Waits to be told the server's tools may have changed, as a proxy
    /// says once it has connected to its server again.
    fn reconnected(&self) {
        while self.next()["method"] != "notifications/tools/list_changed" {}
    }
    /// Sends the handshake an agent opens with, and waits for its answer.
    fn initialize(&mut self) -> serde_json::Value {
        use serde_json::json;
        self.send(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}));
        let answer = self.reply(0);
        self.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        answer
    }
    /// Whether the proxy's process has exited, waiting a few seconds
    /// for it to.
    fn exits(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(8);
        while self.child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn managed_mcp_reconnect_interrupts_without_replay_forwards_callbacks_and_revokes_pending_calls() {
    use serde_json::json;
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let mut proxy = Proxy::new(&mut client, chat);
    proxy.send(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}));
    assert!(proxy.reply(0).get("result").is_some());
    proxy.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    for (id, method, key) in [
        (1, "resources/list", "resources"),
        (2, "prompts/list", "prompts"),
    ] {
        proxy.send(&json!({"jsonrpc":"2.0","id":id,"method":method}));
        assert!(proxy.reply(id)["result"][key].is_array());
    }
    let call = |id| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"write","arguments":{"callback":true}}});
    proxy.send(&call(3));
    wait_for_chat(&mut client, chat, |n| n.question.is_some());
    assert!(!marker.exists());
    admin(
        &mut client,
        McpAction::Reconnect {
            name: "fixture".into(),
        },
    );
    assert!(
        proxy.reply(3)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("was not sent")
    );
    wait_for_chat(&mut client, chat, |n| n.question.is_none());
    assert!(!marker.exists());
    // The same proxy connects to a new server process: no test of the
    // connection started one besides.
    proxy.reconnected();
    assert_eq!(starts(&marker), 2);
    proxy.send(&call(4));
    wait_for_chat(&mut client, chat, |n| n.question.is_some());
    answer(&mut client, chat, Some("allow".into()));
    let callback = loop {
        let value = proxy.next();
        if value["id"] == "callback" {
            break value;
        }
    };
    assert_eq!(callback["method"], "ping");
    proxy.send(&json!({"jsonrpc":"2.0","id":"callback","result":{}}));
    assert_eq!(
        proxy.reply(4)["result"]["content"][0]["text"],
        "callback answered"
    );
    proxy.send(&call(5));
    wait_for_chat(&mut client, chat, |n| n.question.is_some());
    let info = admin(&mut client, McpAction::List).servers.remove(0);
    let mut settings: serde_json::Value = serde_json::from_str(&info.settings).unwrap();
    settings["enabled"] = json!(false);
    admin(
        &mut client,
        McpAction::Save {
            name: info.name,
            settings: settings.to_string(),
            scope: info.scope,
            expected: Some(info.revision),
        },
    );
    wait_for_chat(&mut client, chat, |n| n.question.is_none());
    // Turning the server off is a refusal: the call is told so, and the
    // proxy stops.
    let refused = proxy.reply(5);
    let why = refused["error"]["message"].as_str().unwrap();
    assert!(why.contains("turned off"), "{why}");
    assert!(proxy.exits());
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "executed\n");
}

/// How many times the fixture server started.
fn starts(marker: &Path) -> usize {
    let started = std::fs::read_to_string(marker.with_extension("started")).unwrap_or_default();
    started.lines().count()
}

#[test]
fn managed_mcp_approves_each_external_call_before_forwarding_and_preserves_builtin_permissions() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let tools = last_words(&ask(&mut client, chat, "tools"));
    assert!(tools.contains("managed_fixture"), "{tools}");
    assert!(tools.contains("clackatoo"));
    assert!(!tools.contains("isolated-private-key"));
    let prompt = "mcp-server managed_fixture\nmcp write {}";
    client.chat(chat, ChatAction::Send(prompt.into())).unwrap();
    let question = wait_for_chat(&mut client, chat, |node| node.question.is_some())
        .question
        .unwrap();
    assert!(
        question.title.contains("write") && question.title.contains("fixture"),
        "{question:?}"
    );
    let offered: Vec<&str> = question.choices.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(offered, ["allow", "server", "reject"]);
    assert!(
        !marker.exists(),
        "upstream must not receive the call before approval"
    );
    answer(&mut client, chat, Some("reject".into()));
    let node = wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    assert!(last_words(&node).contains("user refused"));
    assert!(!marker.exists());
    for _ in 0..2 {
        client.chat(chat, ChatAction::Send(prompt.into())).unwrap();
        wait_for_chat(&mut client, chat, |node| node.question.is_some());
        answer(&mut client, chat, Some("allow".into()));
        let node = wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
        assert!(last_words(&node).contains("upstream answered"), "{node:?}");
    }
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "executed\nexecuted\n"
    );
    assert!(last_words(&ask(&mut client, chat, "mcp list_panes")).contains("pane 1"));
    let forged = client
        .request(&Request::McpProxy {
            chat,
            server: "fixture".into(),
            token: "forged".into(),
            action: McpProxyAction::Open,
        })
        .unwrap();
    assert!(matches!(forged, Response::Error(_)), "{forged:?}");
}

#[test]
fn clackatoo_rules_review_managed_calls_even_when_tools_are_allowed() {
    use clackatoo_daemon::permissions::{Config, Engine, read_rules};
    let daemon = TestDaemon::start_with(|_, config| {
        config.agents.custom = vec![custom_agent("claude", vec![fake_agent().into()])];
        config.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
        config.permissions = Config {
            engine: Engine::Clackatoo,
            rules: read_rules(r#"[
                {"decision":"allow","kind":"tool","target":"managed_fixture/write","arguments":{}},
                {"decision":"deny","kind":"tool","target":"managed_fixture/write","arguments":{"refuse":true}}
            ]"#).unwrap(),
        };
    });
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let node = ask(
        &mut client,
        chat,
        "mcp-server managed_fixture\nmcp write {}",
    );
    assert!(last_words(&node).contains("upstream answered"), "{node:?}");
    let node = ask(
        &mut client,
        chat,
        r#"mcp-server managed_fixture
mcp write {"refuse":true}"#,
    );
    // A rule's verdict is told as the rules', never as the user's.
    let said = last_words(&node);
    assert!(
        said.contains("permissions don't allow") && !said.contains("user"),
        "{said}"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "executed\n");
    client
        .chat(
            chat,
            ChatAction::Send(
                r#"mcp-server managed_fixture
mcp write {"unmatched":true}"#
                    .into(),
            ),
        )
        .unwrap();
    let question = wait_for_chat(&mut client, chat, |n| n.question.is_some())
        .question
        .unwrap();
    assert_eq!(question.choices.len(), 2);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "executed\n");
    answer(&mut client, chat, Some("reject".into()));
    wait_for_chat(&mut client, chat, |n| last_outcome(n).is_some());
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "executed\n");
}

#[test]
fn managed_mcp_tests_do_not_call_tools_and_configuration_conflicts_do_not_overwrite() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    let reply = admin(
        &mut client,
        McpAction::Test {
            name: "fixture".into(),
        },
    );
    assert_eq!(reply.servers[0].state, clackatoo_protocol::McpState::Ready);
    assert_eq!(reply.servers[0].tools[0].name, "write");
    assert!(!marker.exists());
    assert!(!format!("{reply:?}").contains("isolated-private-key"));
    let wrong = client
        .request(&Request::Mcp {
            pane: None,
            action: McpAction::Save {
                name: "fixture".into(),
                settings: r#"{"command":"false"}"#.into(),
                scope: McpScope::Personal,
                expected: Some("stale".into()),
            },
        })
        .unwrap();
    assert!(matches!(wrong, Response::Error(_)));
    let reply = admin(&mut client, McpAction::List);
    assert!(reply.servers[0].settings.contains("python3"));
    let off = start_tooled_chat(&mut client, AgentTools::Off);
    assert!(!last_words(&ask(&mut client, off, "tools")).contains("managed_fixture"));
    let next = start_tooled_chat(&mut client, AgentTools::Ask);
    assert!(last_words(&ask(&mut client, next, "tools")).contains("managed_fixture"));
}

/// An HTTP server that answers every request as one that wants a
/// sign-in does: 401, saying where to get one. Returns its address.
fn unauthorized_server() -> String {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut request = [0; 8192];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    address
}

#[test]
fn a_server_refusing_the_agent_for_want_of_a_login_asks_the_chat_to_sign_in_and_stays_up() {
    use clackatoo_protocol::McpSignInState;
    use serde_json::json;
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    admin(
        &mut client,
        McpAction::Save {
            name: "fixture".into(),
            scope: McpScope::Personal,
            expected: None,
            settings: json!({"url": format!("{}/mcp", unauthorized_server())}).to_string(),
        },
    );
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let mut proxy = Proxy::new(&mut client, chat);
    // The agent is answered for the server, so it keeps it.
    let greeting = proxy.initialize();
    assert_eq!(greeting["result"]["serverInfo"]["name"], "fixture");
    let node = wait_for_chat(&mut client, chat, |node| !node.mcp_sign_in.is_empty());
    assert_eq!(node.mcp_sign_in[0].server, "fixture");
    assert_eq!(node.mcp_sign_in[0].state, McpSignInState::Needed);
    // Its calls say what's wrong, rather than the proxy having gone.
    proxy.send(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"write"}}));
    let why = proxy.reply(1)["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        why.contains("sign in") && why.contains("was not sent"),
        "{why}"
    );
    proxy.send(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    assert_eq!(proxy.reply(2)["result"]["tools"], json!([]));
    // Settings shows why for as long as it's so, not for eight seconds.
    let info = admin(&mut client, McpAction::List).servers.remove(0);
    assert_eq!(info.state, clackatoo_protocol::McpState::NeedsLogin);
    assert!(!info.detail.is_empty());
}

#[test]
fn always_allowing_a_server_asks_once_for_the_chat_until_tools_is_set_again() {
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let prompt = "mcp-server managed_fixture\nmcp write {}";
    client.chat(chat, ChatAction::Send(prompt.into())).unwrap();
    wait_for_chat(&mut client, chat, |node| node.question.is_some());
    answer(&mut client, chat, Some("server".into()));
    let node = wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    assert!(last_words(&node).contains("upstream answered"), "{node:?}");
    // From then on nothing is asked: `ask` waits for the turn to end,
    // which a question would hold up.
    for _ in 0..2 {
        let node = ask(&mut client, chat, prompt);
        assert!(node.question.is_none());
        assert!(last_words(&node).contains("upstream answered"), "{node:?}");
    }
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "executed\nexecuted\nexecuted\n"
    );
    // It allowed that server, not the agent's commands.
    let node = client.capture(chat).unwrap().chat().unwrap().clone();
    let tools = node
        .settings
        .iter()
        .find(|s| s.id == "clackatoo.tools")
        .unwrap();
    assert_eq!(tools.value, "ask");
    // Setting Tools again takes it back.
    let set = ChatAction::Set {
        setting: "clackatoo.tools".into(),
        value: "ask".into(),
    };
    client.chat(chat, set).unwrap();
    client.chat(chat, ChatAction::Send(prompt.into())).unwrap();
    wait_for_chat(&mut client, chat, |node| node.question.is_some());
    answer(&mut client, chat, Some("reject".into()));
    wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
}

#[test]
fn cancelling_a_call_stopping_the_turn_or_turning_tools_off_takes_its_question_back() {
    use serde_json::json;
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    // The user stops the turn instead of answering: the question goes
    // with it, and nothing is sent.
    let prompt = "mcp-server managed_fixture\nmcp write {}";
    client.chat(chat, ChatAction::Send(prompt.into())).unwrap();
    wait_for_chat(&mut client, chat, |n| n.question.is_some());
    client.chat(chat, ChatAction::Stop).unwrap();
    let node = wait_for_chat(&mut client, chat, |n| last_outcome(n).is_some());
    assert!(node.question.is_none(), "{node:?}");
    assert!(!marker.exists());

    let mut proxy = Proxy::new(&mut client, chat);
    proxy.initialize();
    let call = |id| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"write","arguments":{}}});
    proxy.send(&call(1));
    wait_for_chat(&mut client, chat, |n| n.question.is_some());
    // The agent no longer wants it.
    proxy.send(
        &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
    );
    wait_for_chat(&mut client, chat, |n| n.question.is_none());

    // Tools turned off is a refusal of the whole proxy, with its reason.
    proxy.send(&call(3));
    wait_for_chat(&mut client, chat, |n| n.question.is_some());
    let off = ChatAction::Set {
        setting: "clackatoo.tools".into(),
        value: "off".into(),
    };
    client.chat(chat, off).unwrap();
    let why = proxy.reply(3)["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(why.contains("tools are off"), "{why}");
    wait_for_chat(&mut client, chat, |n| n.question.is_none());
    assert!(proxy.exits());
    assert!(!marker.exists());
}

#[test]
fn a_newer_proxy_takes_a_chats_server_over_at_once() {
    use serde_json::json;
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let mut first = Proxy::new(&mut client, chat);
    first.initialize();
    // As an agent that starts the server again does, with the first
    // still running: once, the daemon turned it away for eight seconds.
    let mut second = Proxy::new(&mut client, chat);
    assert!(second.initialize().get("result").is_some());
    second.send(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"write","arguments":{}}}));
    assert_eq!(
        second.reply(1)["result"]["content"][0]["text"],
        "upstream answered"
    );
    assert!(first.exits(), "the one it took over from stops");
    let info = admin(&mut client, McpAction::List).servers.remove(0);
    assert_eq!(info.state, clackatoo_protocol::McpState::Connected);
}

#[test]
fn a_server_that_crashes_and_a_changed_definition_reconnect_the_same_proxy() {
    use serde_json::json;
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let mut proxy = Proxy::new(&mut client, chat);
    proxy.initialize();
    let call = |id, name: &str| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":{}}});
    proxy.send(&call(1, "crash"));
    let why = proxy.reply(1)["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        why.contains("isn't known") && why.contains("fixture crashed"),
        "{why}"
    );
    proxy.reconnected();
    proxy.send(&call(2, "write"));
    assert_eq!(
        proxy.reply(2)["result"]["content"][0]["text"],
        "upstream answered"
    );
    assert_eq!(starts(&marker), 2);

    // A credential no server reads leaves it be; the one it reads
    // starts it again.
    admin(
        &mut client,
        McpAction::Secret {
            name: "unrelated".into(),
            value: "another-key".into(),
        },
    );
    proxy.send(&call(3, "write"));
    assert_eq!(
        proxy.reply(3)["result"]["content"][0]["text"],
        "upstream answered"
    );
    assert_eq!(starts(&marker), 2);
    admin(
        &mut client,
        McpAction::Secret {
            name: "fixture-key".into(),
            value: "rotated-private-key".into(),
        },
    );
    proxy.reconnected();
    assert_eq!(starts(&marker), 3);

    // A saved change to its definition is taken in place too, where it
    // once ended the proxy until the chat was reopened.
    let info = admin(&mut client, McpAction::List).servers.remove(0);
    let mut settings: serde_json::Value = serde_json::from_str(&info.settings).unwrap();
    settings["call_timeout_seconds"] = json!(900);
    admin(
        &mut client,
        McpAction::Save {
            name: info.name,
            settings: settings.to_string(),
            scope: info.scope,
            expected: Some(info.revision),
        },
    );
    proxy.reconnected();
    proxy.send(&call(4, "write"));
    assert_eq!(
        proxy.reply(4)["result"]["content"][0]["text"],
        "upstream answered"
    );
    assert_eq!(starts(&marker), 4);
    let info = admin(&mut client, McpAction::List).servers.remove(0);
    assert!(
        info.settings.contains("\"call_timeout_seconds\": 900"),
        "{}",
        info.settings
    );
}

#[test]
fn an_unreadable_mcp_json_costs_only_its_own_servers_and_no_live_connection() {
    use serde_json::json;
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    // A project whose own file can't be read: once, that kept every
    // server from every chat there.
    let project = daemon.dir.path().join("project");
    std::fs::create_dir_all(project.join(".agents")).unwrap();
    std::fs::write(project.join(".git"), "fixture").unwrap();
    std::fs::write(project.join(".agents/mcp.json"), "{ half written").unwrap();
    let chat = start_tooled_chat_in(&mut client, AgentTools::Allow, &project);
    let mut proxy = Proxy::new(&mut client, chat);
    proxy.initialize();
    let call = |id| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"write","arguments":{}}});
    proxy.send(&call(1));
    assert_eq!(
        proxy.reply(1)["result"]["content"][0]["text"],
        "upstream answered"
    );

    // Now the file its own definition is in can't be read either, and
    // the catalogue is read again. The connection keeps the definition
    // it has.
    let personal = daemon.dir.path().join(".agents/mcp.json");
    let good = std::fs::read_to_string(&personal).unwrap();
    std::fs::write(&personal, "{ half written").unwrap();
    let reply = client
        .request(&Request::Mcp {
            pane: None,
            action: McpAction::Reconnect {
                name: "fixture".into(),
            },
        })
        .unwrap();
    assert!(
        matches!(reply, Response::Error(_)),
        "it isn't there to reconnect: {reply:?}"
    );
    ask(&mut client, chat, "mcp list_skills");
    proxy.send(&call(2));
    assert_eq!(
        proxy.reply(2)["result"]["content"][0]["text"],
        "upstream answered"
    );
    assert_eq!(starts(&marker), 1);
    std::fs::write(&personal, good).unwrap();
    ask(&mut client, chat, "mcp list_skills");
    proxy.send(&call(3));
    assert_eq!(
        proxy.reply(3)["result"]["content"][0]["text"],
        "upstream answered"
    );
    assert_eq!(starts(&marker), 1);
}

/// Sends the chat `asked`, the fake agent's own question about a call,
/// and returns the question the chat shows of it, if it shows one,
/// answering it with a refusal: `None` if clackatoo answered the agent
/// itself.
fn agent_asks(client: &mut Client, chat: TerminalId, asked: &str) -> Option<String> {
    let turns = client.capture(chat).unwrap().chat().unwrap().turns.len() + 1;
    client.chat(chat, ChatAction::Send(asked.into())).unwrap();
    let ended = |node: &ChatNode| node.turns.len() == turns && last_outcome(node).is_some();
    let node = wait_for_chat(client, chat, |node| node.question.is_some() || ended(node));
    let Some(question) = &node.question else {
        assert_eq!(last_words(&node), "picked allow\n");
        return None;
    };
    answer(client, chat, Some("reject".into()));
    let node = wait_for_chat(client, chat, ended);
    assert_eq!(last_words(&node), "picked reject\n");
    Some(question.title.clone())
}

/// A call to the fixture's tool through `proxy`, as the request `id`:
/// whether the chat asked about it before it was answered. One asked
/// about is allowed.
fn proxy_calls(client: &mut Client, chat: TerminalId, proxy: &mut Proxy, id: u32) -> bool {
    use serde_json::json;
    proxy.send(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"write","arguments":{}}}));
    let mut asked = false;
    loop {
        if let Ok(message) = proxy.output.recv_timeout(Duration::from_millis(20)) {
            if message["id"] == id {
                assert_eq!(message["result"]["content"][0]["text"], "upstream answered");
                return asked;
            }
            continue;
        }
        let node = client.capture(chat).unwrap().chat().unwrap().clone();
        if node.question.is_some() {
            assert!(!asked, "asked twice");
            asked = true;
            answer(client, chat, Some("allow".into()));
        }
    }
}

/// Whoever asks, the user is asked once. Claude Code's question about a
/// managed server's tool is `mcp-ask managed_fixture dynamic`, as its
/// manual mode asks it; in a mode that asks nothing, only the proxy's
/// call happens.
#[test]
fn a_managed_call_is_asked_about_once_by_clackatoo_or_by_the_agent_never_both_or_neither() {
    let asks = "mcp-ask managed_fixture dynamic";
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);

    // Tools: Ask. Clackatoo asks, at the proxy, so the agent's own
    // question is answered for the user: once in a manual mode, and once
    // in one where the agent asks nothing.
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    // Before the proxy is there, nothing says the server is clackatoo's
    // to ask about: the agent's question stands.
    assert_eq!(
        agent_asks(&mut client, chat, asks).as_deref(),
        Some("mcp__managed_fixture__note")
    );
    let mut proxy = Proxy::new(&mut client, chat);
    proxy.initialize();
    assert_eq!(agent_asks(&mut client, chat, asks), None);
    assert!(proxy_calls(&mut client, chat, &mut proxy, 1));
    // A server of that name the agent's own configuration has isn't
    // clackatoo's, whatever it's called.
    assert!(agent_asks(&mut client, chat, "mcp-ask managed_fixture project").is_some());

    // Tools: Allow. Clackatoo asks nothing, so the agent's question is
    // the one asked: "Always allow commands" must not have opened every
    // external server unasked. In a mode where the agent asks nothing,
    // nothing is asked, as that mode says.
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let mut proxy = Proxy::new(&mut client, chat);
    proxy.initialize();
    assert_eq!(
        agent_asks(&mut client, chat, asks).as_deref(),
        Some("mcp__managed_fixture__note")
    );
    assert!(!proxy_calls(&mut client, chat, &mut proxy, 1));
}

#[test]
fn under_clackatoo_rules_a_managed_call_is_ruled_on_once_under_its_managed_name() {
    use clackatoo_daemon::permissions::{Config, Engine, read_rules};
    let daemon = TestDaemon::start_with(|_, config| {
        config.agents.custom = vec![custom_agent("claude", vec![fake_agent().into()])];
        config.mcp = Some(vec![fake_agent().into(), "mcp".into()]);
        config.permissions = Config {
            engine: Engine::Clackatoo,
            // Against the agent's name for the tool, which isn't the one
            // a managed call is ruled under: it denies nothing here.
            rules: read_rules(
                r#"[{"decision":"deny","kind":"tool","target":"claude/mcp__managed_fixture__note"}]"#,
            )
            .unwrap(),
        };
    });
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let mut proxy = Proxy::new(&mut client, chat);
    proxy.initialize();
    // The agent's own question is answered: the rules decide, at the
    // proxy, where no rule covers the call and the user is asked.
    assert_eq!(
        agent_asks(&mut client, chat, "mcp-ask managed_fixture dynamic"),
        None
    );
    assert!(proxy_calls(&mut client, chat, &mut proxy, 1));
}

#[test]
fn a_server_allowed_always_asks_again_once_it_is_defined_otherwise() {
    use serde_json::json;
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let marker = daemon.dir.path().join("external-call");
    fixture(&mut client, &marker);
    let chat = start_tooled_chat(&mut client, AgentTools::Ask);
    let prompt = "mcp-server managed_fixture\nmcp write {}";
    client.chat(chat, ChatAction::Send(prompt.into())).unwrap();
    wait_for_chat(&mut client, chat, |node| node.question.is_some());
    answer(&mut client, chat, Some("server".into()));
    wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
    assert!(ask(&mut client, chat, prompt).question.is_none());

    // The same name, now another command line: not the server the user
    // allowed.
    let info = admin(&mut client, McpAction::List).servers.remove(0);
    let mut settings: serde_json::Value = serde_json::from_str(&info.settings).unwrap();
    settings["args"]
        .as_array_mut()
        .unwrap()
        .push(json!("--changed"));
    admin(
        &mut client,
        McpAction::Save {
            name: info.name,
            settings: settings.to_string(),
            scope: info.scope,
            expected: Some(info.revision),
        },
    );
    client.chat(chat, ChatAction::Send(prompt.into())).unwrap();
    let node = wait_for_chat(&mut client, chat, |node| {
        node.question.is_some() || node.turns.len() == 3 && last_outcome(node).is_some()
    });
    assert!(node.question.is_some(), "it asks again: {node:?}");
    answer(&mut client, chat, Some("reject".into()));
    wait_for_chat(&mut client, chat, |node| last_outcome(node).is_some());
}

#[test]
fn a_server_refusing_the_credential_in_its_headers_is_an_error_not_a_sign_in() {
    use serde_json::json;
    let daemon = TestDaemon::with_tools();
    let mut client = daemon.client();
    let settings = json!({
        "url": format!("{}/mcp", unauthorized_server()),
        "headers": {"Authorization": "Bearer wrong"},
    });
    admin(
        &mut client,
        McpAction::Save {
            name: "fixture".into(),
            scope: McpScope::Personal,
            expected: None,
            settings: settings.to_string(),
        },
    );
    let chat = start_tooled_chat(&mut client, AgentTools::Allow);
    let mut proxy = Proxy::new(&mut client, chat);
    proxy.initialize();
    proxy.send(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"write"}}));
    let why = proxy.reply(1)["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        why.contains("configured headers") && !why.contains("sign in"),
        "{why}"
    );
    // No button that couldn't help.
    let node = client.capture(chat).unwrap().chat().unwrap().clone();
    assert!(node.mcp_sign_in.is_empty(), "{:?}", node.mcp_sign_in);
    // The proxy's word of it reaches Settings a moment after the call's.
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let info = admin(&mut client, McpAction::List).servers.remove(0);
        if info.state == clackatoo_protocol::McpState::Error {
            assert!(
                info.detail.contains("configured headers"),
                "{}",
                info.detail
            );
            break;
        }
        assert!(Instant::now() < deadline, "{info:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
