//! Local HTTP and OAuth fixtures: no model, account or external network.
use super::*;
use serde_json::{Value, json};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[derive(Default)]
struct State {
    requests: Vec<(String, String, String)>,
    oauth: bool,
    sse: bool,
}
struct Fixture {
    base: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn new(oauth: bool, sse: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            oauth,
            sse,
            ..State::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (worker_base, worker_state, worker_stop) =
            (base.clone(), Arc::clone(&state), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &worker_base, &worker_state),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("fixture accept: {e}"),
                }
            }
        });
        Self {
            base,
            state,
            stop,
            thread: Some(thread),
        }
    }
    fn bound(&self, roots: &Roots) -> super::super::config::Bound {
        let mut definition = json!({"url":format!("{}/mcp",self.base),"timeout_seconds":5});
        if self.state.lock().unwrap().oauth {
            definition["oauth"] = json!({});
        } else {
            definition["headers"] = json!({"X-Api-Key":{"secret":"http-key"}});
        }
        credentials::set(&config::private(roots), "http-key", "private-http-key").unwrap();
        let registry = Registry::discover(roots, &BTreeSet::new());
        config::save(
            roots,
            &registry,
            "http",
            Some(&definition.to_string()),
            McpScope::Personal,
            None,
        )
        .unwrap();
        Registry::discover(roots, &BTreeSet::new())
            .servers
            .remove("http")
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}
#[allow(
    clippy::too_many_lines,
    reason = "one small fixture router for the HTTP and OAuth protocol steps"
)]
fn serve(mut stream: TcpStream, base: &str, state: &Mutex<State>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    let (header_end, length) = loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..i]);
            let length = headers
                .lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (i + 4, length);
        }
        assert!(bytes.len() < 65536);
    };
    while bytes.len() < header_end + length {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        bytes.extend_from_slice(&chunk[..n]);
        assert!(bytes.len() < 65536);
    }
    let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let body = String::from_utf8_lossy(&bytes[header_end..header_end + length]).into_owned();
    let line = headers.lines().next().unwrap().to_owned();
    let mut state = state.lock().unwrap();
    state
        .requests
        .push((line.clone(), headers.clone(), body.clone()));
    let mut status = "200 OK";
    let mut extra = String::new();
    let mut sse = false;
    let response = if line.contains("/.well-known/oauth-protected-resource") {
        json!({"resource":format!("{base}/mcp"),"authorization_servers":[base],"scopes_supported":["read"]})
    } else if line.contains("/.well-known/oauth-authorization-server") {
        json!({"issuer":base,"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"registration_endpoint":format!("{base}/register"),"response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none"]})
    } else if line.starts_with("POST /register ") {
        json!({"client_id":"fixture-client","token_endpoint_auth_method":"none","redirect_uris":serde_json::from_str::<Value>(&body).unwrap()["redirect_uris"],"grant_types":["authorization_code","refresh_token"],"response_types":["code"]})
    } else if line.starts_with("POST /token ") {
        json!({"access_token":"fixture-access","refresh_token":"fixture-refresh","token_type":"Bearer","expires_in":3600,"scope":"read"})
    } else if line.starts_with("POST /mcp ")
        && (!state.oauth
            || headers
                .to_lowercase()
                .contains("authorization: bearer fixture-access"))
    {
        if !state.oauth {
            assert!(
                headers
                    .to_lowercase()
                    .contains("x-api-key: private-http-key")
            );
        }
        let message: Value = serde_json::from_str(&body).unwrap();
        let result = match message["method"].as_str().unwrap_or_default() {
            "initialize" => {
                json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
            }
            "tools/list" => {
                json!({"tools":[{"name":"http-search","description":"Search fixture","inputSchema":{"type":"object"}}]})
            }
            "notifications/initialized" => {
                status = "202 Accepted";
                Value::Null
            }
            other => panic!("unexpected HTTP method {other}"),
        };
        sse = state.sse && result != Value::Null;
        if result == Value::Null {
            Value::Null
        } else {
            json!({"jsonrpc":"2.0","id":message["id"],"result":result})
        }
    } else if state.oauth && line.contains(" /mcp ") && !line.starts_with("DELETE") {
        status = "401 Unauthorized";
        extra = format!(
            "WWW-Authenticate: Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\"\r\n"
        );
        json!({"error":"unauthorized"})
    } else {
        status = "405 Method Not Allowed";
        Value::Null
    };
    let body = if response == Value::Null {
        String::new()
    } else if sse {
        format!("event: message\ndata: {response}\n\n")
    } else {
        response.to_string()
    };
    let kind = if sse {
        "text/event-stream"
    } else {
        "application/json"
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
}

#[test]
fn streamable_http_supports_json_and_sse_and_resolves_private_headers() {
    for sse in [false, true] {
        let fixture = Fixture::new(false, sse);
        let dir = tempfile::tempdir().unwrap();
        let roots = roots(dir.path());
        let tools = transport::probe(&fixture.bound(&roots)).unwrap();
        assert_eq!(tools[0].name, "http-search");
        assert!(
            fixture
                .state
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|(_, _, body)| !body.contains("private-http-key"))
        );
    }
}

fn login_url(login: &super::super::oauth::Login) -> String {
    match login.events.recv_timeout(Duration::from_secs(8)).unwrap() {
        super::super::oauth::Event::Url(url) => url,
        super::super::oauth::Event::Finished(result) => {
            panic!("login ended before URL: {result:?}")
        }
    }
}
#[test]
#[allow(clippy::too_many_lines, reason = "one end-to-end credential lifecycle")]
fn oauth_discovery_pkce_manual_and_split_loopback_callbacks_refresh_and_revocation() {
    use rmcp::transport::auth::CredentialStore as _;
    let fixture = Fixture::new(true, false);
    let dir = tempfile::tempdir().unwrap();
    let roots = roots(dir.path());
    let bound = fixture.bound(&roots);
    let poll = mio::Poll::new().unwrap();
    let waker = Arc::new(mio::Waker::new(poll.registry(), mio::Token(1)).unwrap());
    let rejected = super::super::oauth::start(bound.clone(), Arc::clone(&waker)).unwrap();
    let url = url::Url::parse(&login_url(&rejected)).unwrap();
    let query: BTreeMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(!query["code_challenge"].is_empty());
    rejected
        .callback
        .try_send(format!(
            "{}?code=fixture&state=wrong",
            query["redirect_uri"]
        ))
        .unwrap();
    assert!(matches!(
        rejected
            .events
            .recv_timeout(Duration::from_secs(8))
            .unwrap(),
        super::super::oauth::Event::Finished(Err(_))
    ));
    assert!(!credentials::OAuthStore::of(&bound).unwrap().exists());
    for manual in [true, false] {
        let login = super::super::oauth::start(bound.clone(), Arc::clone(&waker)).unwrap();
        let url = url::Url::parse(&login_url(&login)).unwrap();
        let query: BTreeMap<_, _> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let callback = format!(
            "{}?code=fixture&state={}",
            query["redirect_uri"], query["state"]
        );
        if manual {
            login.callback.try_send(callback).unwrap();
        } else {
            let url = url::Url::parse(&callback).unwrap();
            let mut stream = TcpStream::connect(("127.0.0.1", url.port().unwrap())).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(8)))
                .unwrap();
            let request = format!(
                "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
                url.path(),
                url.query().unwrap()
            );
            stream.write_all(&request.as_bytes()[..12]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            stream.write_all(&request.as_bytes()[12..]).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(response.contains("Signed in to Clackatoo"), "{response}");
        }
        assert!(matches!(
            login.events.recv_timeout(Duration::from_secs(8)).unwrap(),
            super::super::oauth::Event::Finished(Ok(()))
        ));
        assert_eq!(transport::probe(&bound).unwrap()[0].name, "http-search");
        let runtime = transport::runtime().unwrap();
        runtime.block_on(async {
            let mut manager = transport::authorization(&bound).await.unwrap();
            assert!(manager.initialize_from_store().await.unwrap());
            manager.refresh_token().await.unwrap();
            assert!(
                credentials::OAuthStore::of(&bound)
                    .unwrap()
                    .load()
                    .await
                    .unwrap()
                    .is_some()
            );
        });
        assert_eq!(
            credentials::redacted(&bound, "fixture-access fixture-refresh"),
            "[redacted] [redacted]"
        );
        credentials::OAuthStore::of(&bound)
            .unwrap()
            .forget()
            .unwrap();
    }
    let state = fixture.state.lock().unwrap();
    let forms: Vec<_> = state
        .requests
        .iter()
        .filter(|(line, _, _)| line.starts_with("POST /token "))
        .map(|(_, _, body)| body)
        .collect();
    assert!(
        forms
            .iter()
            .any(|body| body.contains("code_verifier=") && body.contains("resource="))
    );
    assert!(
        forms
            .iter()
            .any(|body| body.contains("grant_type=refresh_token"))
    );
}
