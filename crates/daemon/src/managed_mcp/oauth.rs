//! SDK OAuth with PKCE, a loopback callback and optional remote completion.
use rmcp::transport::auth::{AuthorizationRequest, AuthorizationSession};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{config::Bound, credentials, transport};

pub(crate) enum Event {
    Url(String),
    Finished(Result<(), String>),
}
pub(crate) struct Login {
    pub(crate) events: mpsc::Receiver<Event>,
    pub(crate) callback: tokio::sync::mpsc::Sender<String>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Login {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

pub(crate) fn start(bound: Bound, waker: Arc<mio::Waker>) -> Result<Login, String> {
    if bound
        .definition
        .as_ref()
        .and_then(|d| d.url.as_ref())
        .is_none()
    {
        return Err("Sign in is available for HTTP servers; command servers receive credentials through their environment".into());
    }
    let (send, events) = mpsc::channel();
    let (callback, receive) = tokio::sync::mpsc::channel(4);
    let cancel = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::clone(&cancel);
    std::thread::Builder::new()
        .name("mcp-login".into())
        .spawn(move || {
            let result = transport::runtime().and_then(|runtime| {
                runtime.block_on(login(&bound, &send, receive, &cancelled, &waker))
            });
            let result = result.map_err(|e| credentials::redacted(&bound, &e));
            let _ = send.send(Event::Finished(result));
            let _ = waker.wake();
        })
        .map_err(|_| "Cannot start the MCP login worker")?;
    Ok(Login {
        events,
        callback,
        cancel,
    })
}

async fn login(
    bound: &Bound,
    send: &mpsc::Sender<Event>,
    mut callback: tokio::sync::mpsc::Receiver<String>,
    cancel: &AtomicBool,
    waker: &mio::Waker,
) -> Result<(), String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| "Cannot open the OAuth loopback callback")?;
    let redirect = format!(
        "http://127.0.0.1:{}/callback",
        listener
            .local_addr()
            .map_err(|_| "Cannot locate the OAuth callback")?
            .port()
    );
    let mut manager = transport::authorization(bound).await?;
    let _ = manager.initialize_from_store().await;
    let config = bound.definition.as_ref().and_then(|d| d.oauth.as_ref());
    let mut request = AuthorizationRequest::new(&redirect)
        .with_client_name("Clackatoo")
        .with_application_type("native");
    if let Some(client_id) = config.and_then(|c| c.client_id.as_deref()) {
        request = request.with_preregistered_client(client_id);
    }
    if let Some(config) = config {
        request = request.with_scopes(config.scopes.clone());
    }
    let session = tokio::time::timeout(
        Duration::from_secs(30),
        AuthorizationSession::new(manager, request),
    )
    .await
    .map_err(|_| "OAuth discovery timed out")?
    .map_err(|(_, e)| credentials::redacted(bound, &e.to_string()))?;
    send.send(Event::Url(session.get_authorization_url().into()))
        .map_err(|_| "The login requester closed")?;
    let _ = waker.wake();
    let deadline = tokio::time::sleep(Duration::from_mins(5));
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            ()=&mut deadline=>return Err("OAuth login expired. Start sign in again.".into()),
            _=tick.tick()=>{if cancel.load(Ordering::Acquire){return Err("OAuth login cancelled".into());}},
            manual=callback.recv()=>{
                let Some(url)=manual else{return Err("OAuth login cancelled".into());};
                validate_callback(&redirect,&url)?;
                session.handle_callback_url(&url).await.map_err(|e|credentials::redacted(bound,&e.to_string()))?;
                return Ok(());
            },
            accepted=listener.accept()=>{
                let (mut stream,_)=accepted.map_err(|_|"OAuth callback failed")?;
                let line=tokio::time::timeout(Duration::from_secs(3), callback_line(&mut stream)).await.map_err(|_|"OAuth callback timed out")??;
                let path=line.strip_prefix("GET ").and_then(|s|s.split_once(' ')).map(|(p,_)|p).unwrap_or_default();
                let base=redirect.trim_end_matches("/callback");let url=format!("{base}{path}");
                let result=validate_callback(&redirect,&url).and_then(|()|if path.starts_with("/callback?"){Ok(())}else{Err("Not an OAuth callback".into())});
                if result.is_err(){let _=stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;continue;}
                let result=session.handle_callback_url(&url).await;
                let body=if result.is_ok(){"Signed in to Clackatoo. You can close this tab."}else{"Clackatoo could not finish sign in. Return to MCP Servers in Settings."};
                let reply=format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Security-Policy: default-src 'none'\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                let _=stream.write_all(reply.as_bytes()).await;
                result.map_err(|e|credentials::redacted(bound,&e.to_string()))?;return Ok(());
            }
        }
    }
}

fn validate_callback(expected: &str, actual: &str) -> Result<(), String> {
    if actual.len() > 16384 {
        return Err("OAuth callback URL is too long".into());
    }
    let expected = url::Url::parse(expected).map_err(|_| "Invalid OAuth redirect")?;
    let actual = url::Url::parse(actual).map_err(|_| "Invalid OAuth callback URL")?;
    if actual.origin() != expected.origin()
        || actual.path() != expected.path()
        || actual.fragment().is_some()
        || !actual.username().is_empty()
        || actual.password().is_some()
    {
        return Err("The callback URL does not match this login attempt".into());
    }
    Ok(())
}

async fn callback_line(stream: &mut tokio::net::TcpStream) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|_| "Cannot read OAuth callback")?;
        if n == 0 {
            return Err("OAuth callback closed before its headers".into());
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() > 16384 {
            return Err("OAuth callback headers are too large".into());
        }
        if bytes.windows(4).any(|b| b == b"\r\n\r\n") {
            return std::str::from_utf8(&bytes)
                .ok()
                .and_then(|s| s.lines().next())
                .map(str::to_owned)
                .ok_or_else(|| "Invalid OAuth callback headers".into());
        }
    }
}
