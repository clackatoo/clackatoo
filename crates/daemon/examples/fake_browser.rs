//! A browser for tests: speaks CDP, the protocol of Chrome's developer
//! tools, as Chromium does with `--remote-debugging-pipe`, reading
//! commands on its fd 3 and
//! writing answers and events on its fd 4, each JSON ending in a NUL, with
//! no pages behind it.
//!
//! It takes Chromium's `--user-data-dir=DIR`, and there:
//!
//! - links `SingletonLock` to `fake-PID`, as Chromium does while it uses a
//!   profile, unless told not to;
//! - writes its process ID to `pid`, and appends a line to `calls.log` for
//!   each command it's sent: the method, then its parameters as JSON;
//! - reads what it's to do from `fake.json`, if there is one:
//!
//! | Key | Does |
//! |---|---|
//! | `"lock": false` | Links no `SingletonLock`, as a browser using another profile wouldn't |
//! | `"fail": [METHOD…]` | Answers those methods with an error |
//! | `"hang": [METHOD…]` | Never answers those methods |
//! | `"title": TITLE` | Gives its pages that title, rather than `Fake page` |
//! | `"huge": [METHOD…]` | Answers those methods with a string of `huge_size` bytes |
//! | `"deep": N` | Gives an accessibility tree N nodes deep, one inside the next |
//! | `"cycle": true` | Gives an accessibility tree whose node is its own child |
//! | `"navigate_to": URL` | Says each page it's sent to went to URL instead |
//!
//! It exits when its pipe closes, or it's sent `Browser.close`.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

fn main() {
    let dir = std::env::args()
        .find_map(|arg| arg.strip_prefix("--user-data-dir=").map(PathBuf::from))
        .expect("--user-data-dir");
    std::fs::create_dir_all(&dir).unwrap();
    let plan: Value = std::fs::read_to_string(dir.join("fake.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let pid = std::process::id();
    std::fs::write(dir.join("pid"), pid.to_string()).unwrap();
    let lock = dir.join("SingletonLock");
    if plan["lock"] != false {
        let _ = std::fs::remove_file(&lock);
        std::os::unix::fs::symlink(format!("fake-{pid}"), &lock).unwrap();
    }
    // The pipe's ends, as Chromium finds them: opened again through /proc,
    // which needs nothing unsafe.
    let commands = BufReader::new(File::open("/proc/self/fd/3").expect("fd 3"));
    let events = OpenOptions::new()
        .write(true)
        .open("/proc/self/fd/4")
        .expect("fd 4");
    let mut browser = Browser {
        dir: dir.clone(),
        plan,
        events,
        urls: HashMap::new(),
        next: 1,
    };
    for message in commands.split(0) {
        let Ok(message) = message else { break };
        let Ok(message) = serde_json::from_slice::<Value>(&message) else {
            continue;
        };
        if !browser.answer(&message) {
            break;
        }
    }
    let _ = std::fs::remove_file(&lock);
}

/// The fake browser's state.
struct Browser {
    dir: PathBuf,
    plan: Value,
    events: File,
    /// Each page's address, by its target.
    urls: HashMap<String, String>,
    /// The number the next context, page or session gets.
    next: u64,
}

impl Browser {
    /// Answers `message`. Returns whether to go on.
    fn answer(&mut self, message: &Value) -> bool {
        let method = message["method"].as_str().unwrap_or_default().to_owned();
        let params = &message["params"];
        let session = message["sessionId"].as_str().map(str::to_owned);
        log(&self.dir, &format!("{method} {params}"));
        let listed = |key: &str| {
            (self.plan[key].as_array().into_iter().flatten()).any(|listed| listed == &method)
        };
        if listed("hang") {
            return true;
        }
        if listed("fail") {
            let error = json!({ "id": message["id"], "error": { "code": -32000, "message": "fake failure" } });
            self.send(&error);
            return true;
        }
        if listed("huge") {
            let size = self.plan["huge_size"].as_u64().unwrap_or(1 << 20);
            let text = "x".repeat(usize::try_from(size).unwrap());
            let answer = json!({ "id": message["id"], "result": { "result": { "value": text } } });
            self.send(&answer);
            return true;
        }
        let result = match method.as_str() {
            "Browser.getVersion" => json!({ "product": "Fake/1" }),
            "Browser.close" => {
                self.send(&json!({ "id": message["id"], "result": {} }));
                return false;
            }
            "Target.createBrowserContext" => json!({ "browserContextId": self.number("context") }),
            "Target.createTarget" => {
                let target = self.number("target");
                self.urls.insert(target.clone(), "about:blank".to_owned());
                json!({ "targetId": target })
            }
            "Target.attachToTarget" => {
                let target = params["targetId"].as_str().unwrap_or_default();
                json!({ "sessionId": format!("session-{target}") })
            }
            "Target.getTargetInfo" => {
                let target = params["targetId"].as_str().unwrap_or_default();
                let url = self.urls.get(target).cloned().unwrap_or_default();
                let title = self.plan["title"].as_str().unwrap_or("Fake page");
                json!({ "targetInfo": { "targetId": target, "url": url, "title": title } })
            }
            "Page.navigate" => {
                let target = session_target(session.as_deref());
                let url = (self.plan["navigate_to"].as_str())
                    .or(params["url"].as_str())
                    .unwrap_or_default()
                    .to_owned();
                self.urls.insert(target.clone(), url.clone());
                self.send(&json!({ "id": message["id"], "result": { "frameId": target } }));
                let frame = json!({ "frame": { "id": target, "url": url } });
                self.event("Page.frameNavigated", &frame, session.as_deref());
                self.event("Page.loadEventFired", &json!({}), session.as_deref());
                return true;
            }
            "Page.getFrameTree" => {
                let target = session_target(session.as_deref());
                let url = self.urls.get(&target).cloned().unwrap_or_default();
                json!({ "frameTree": { "frame": { "id": target, "url": url } } })
            }
            "Accessibility.getFullAXTree" => self.tree(),
            "Runtime.evaluate" => json!({ "result": { "type": "string", "value": "fake text" } }),
            "Page.captureScreenshot" => json!({ "data": "iVBORw0KGgo=" }),
            _ => json!({}),
        };
        self.send(&json!({ "id": message["id"], "result": result }));
        true
    }

    /// The page's accessibility tree, as the plan says.
    fn tree(&self) -> Value {
        if self.plan["cycle"] == true {
            let node = json!({ "nodeId": "1", "role": { "value": "list" }, "childIds": ["1"], "backendDOMNodeId": 1 });
            return json!({ "nodes": [node] });
        }
        let deep = self.plan["deep"].as_u64().unwrap_or(1);
        let nodes: Vec<Value> = (1..=deep)
            .map(|id| {
                let mut node = json!({
                    "nodeId": id.to_string(),
                    "role": { "value": if id == 1 { "RootWebArea" } else { "group" } },
                    "name": { "value": format!("level {id}") },
                    "backendDOMNodeId": id,
                    "childIds": if id < deep { vec![(id + 1).to_string()] } else { Vec::new() },
                });
                if id > 1 {
                    node["parentId"] = json!((id - 1).to_string());
                }
                node
            })
            .collect();
        json!({ "nodes": nodes })
    }

    /// A new name of `kind`.
    fn number(&mut self, kind: &str) -> String {
        self.next += 1;
        format!("{kind}-{}", self.next)
    }

    fn event(&mut self, method: &str, params: &Value, session: Option<&str>) {
        let mut event = json!({ "method": method, "params": params });
        if let Some(session) = session {
            event["sessionId"] = json!(session);
        }
        self.send(&event);
    }

    fn send(&mut self, message: &Value) {
        let mut bytes = serde_json::to_vec(message).unwrap();
        bytes.push(0);
        let _ = self.events.write_all(&bytes);
    }
}

/// The target a session is attached to, as this browser names sessions.
fn session_target(session: Option<&str>) -> String {
    (session.and_then(|session| session.strip_prefix("session-")))
        .unwrap_or_default()
        .to_owned()
}

/// Appends `line` to `calls.log` in `dir`.
fn log(dir: &Path, line: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("calls.log"))
    {
        let _ = writeln!(file, "{line}");
    }
}
