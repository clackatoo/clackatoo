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
deferred=None
for line in sys.stdin:
 m=json.loads(line); method=m.get('method')
 if method=='initialize': result={'protocolVersion':'2025-06-18','capabilities':{'tools':{},'resources':{},'prompts':{}},'serverInfo':{'name':'fixture','version':'1'}}
 elif method=='tools/list': result={'tools':[{'name':'write','description':'Fixture tool','inputSchema':{'type':'object'}}]}
 elif method=='resources/list': result={'resources':[]}
 elif method=='prompts/list': result={'prompts':[]}
 elif method=='tools/call':
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
            .contains("before this request was sent")
    );
    wait_for_chat(&mut client, chat, |n| n.question.is_none());
    assert!(!marker.exists());
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
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "executed\n");
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
    assert_eq!(
        question.choices.len(),
        2,
        "external approvals offer allow once or reject"
    );
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
