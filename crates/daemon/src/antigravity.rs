//! Google's official ACP runtime. Its private profile belongs to the machine
//! running the agent; login works from a terminal on a federated workspace too.
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use url::Url;

const PREFIX: &str = "Open the following link to authenticate the ACP server: ";
const TIMEOUT: Duration = Duration::from_mins(10);
const MAX_LINE: u64 = 1024 * 1024;

pub(crate) fn authorization_line(line: &str) -> Option<&str> {
    line.trim().strip_prefix(PREFIX)
}

fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}

pub(crate) fn profile(dir: &Path) -> PathBuf {
    dir.join("antigravity-profile")
}

/// Reads subscription limits without starting the agent or a model turn.
/// Only the sanitized report goes to stdout; credentials stay in this process.
///
/// # Errors
/// Returns an error if the report cannot be written to stdout.
pub fn usage(profile: &Path) -> io::Result<()> {
    serde_json::to_writer(
        io::stdout().lock(),
        &crate::provider_usage::antigravity::read(profile),
    )
    .map_err(io::Error::other)
}

pub(crate) fn launch_command(
    command: &[OsString],
    dir: Option<&Path>,
) -> io::Result<Vec<OsString>> {
    let dir = dir.ok_or_else(|| invalid("Antigravity needs an agent data directory"))?;
    let program = command
        .first()
        .ok_or_else(|| invalid("No Antigravity runtime"))?;
    Ok(vec![
        std::env::current_exe()?.into_os_string(),
        "agent-runtime".into(),
        program.clone(),
        profile(dir).into_os_string(),
    ])
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid("Antigravity's profile must not be a symbolic link"));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn command(program: &Path, profile: &Path) -> io::Result<Command> {
    private_dir(profile)?;
    let acp = profile.join("antigravity-acp");
    private_dir(&acp)?;
    let tmp = acp.join("tmp");
    private_dir(&tmp)?;
    // No secrets here. Do not rewrite a user's settings on every start.
    let settings = acp.join("settings.json");
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(settings)
    {
        Ok(mut file) => file.write_all(b"{\"auth\":{\"type\":\"oauth-personal\"}}\n")?,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    let mut command = Command::new(program);
    command.arg("--uid=");
    for key in [
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GOOGLE_CLOUD_PROJECT",
        "GOOGLE_CLOUD_LOCATION",
        "GOOGLE_CLOUD_QUOTA_PROJECT",
        "GOOGLE_GENAI_USE_VERTEXAI",
        "GCLOUD_PROJECT",
        "CLOUDSDK_CORE_PROJECT",
        "AGY_ACP_CCPA_PROJECT",
        "AGY_ACP_ENABLE_OAUTH",
        "ANTIGRAVITY_HARNESS_PATH",
    ] {
        command.env_remove(key);
    }
    command
        .env("GEMINI_HOME", profile)
        .env("AGY_ACP_FORCE_FILE_STORAGE", "1")
        .env("BROWSER", "/bin/true")
        .env("PYTHONUNBUFFERED", "1")
        .env("TMPDIR", tmp);
    for candidate in [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/ssl/certs/ca-bundle.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
    ] {
        if Path::new(candidate).is_file() {
            command.env("SSL_CERT_FILE", candidate);
            break;
        }
    }
    Ok(command)
}

/// Internal subprocess entry point. Preparation happens here, never on the
/// daemon's event loop. The runtime inherits its process group and ACP pipes.
///
/// # Errors
/// Returns if the private profile cannot be prepared or the runtime cannot run.
pub fn run(program: &Path, profile: &Path) -> io::Result<()> {
    Err(command(program, profile)?.exec())
}

#[derive(Debug)]
struct Authorization {
    url: Url,
    redirect: Url,
    state: String,
    started: Instant,
    sent: bool,
}
impl Authorization {
    fn parse(text: &str) -> io::Result<Self> {
        let bad = || invalid("Antigravity returned an invalid Google sign-in URL");
        if text.len() > 16384 || text.chars().any(char::is_whitespace) {
            return Err(bad());
        }
        let url = Url::parse(text).map_err(|_| bad())?;
        if url.scheme() != "https"
            || url.host_str() != Some("accounts.google.com")
            || url.port().is_some()
            || url.path() != "/o/oauth2/v2/auth"
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(bad());
        }
        let state = one(&url, "state").ok_or_else(bad)?;
        let redirect = one(&url, "redirect_uri").ok_or_else(bad)?;
        if state.is_empty()
            || state.len() > 512
            || state.chars().any(char::is_whitespace)
            || one(&url, "response_type").as_deref() != Some("code")
        {
            return Err(bad());
        }
        let redirect = Url::parse(&redirect).map_err(|_| bad())?;
        if redirect.scheme() != "http"
            || redirect.host_str() != Some("127.0.0.1")
            || redirect.port().is_none_or(|p| p < 1024)
            || redirect.path() != "/"
            || redirect.query().is_some()
            || redirect.fragment().is_some()
            || !redirect.username().is_empty()
            || redirect.password().is_some()
        {
            return Err(bad());
        }
        Ok(Self {
            url,
            redirect,
            state,
            started: Instant::now(),
            sent: false,
        })
    }
    fn callback(&self, text: &str) -> io::Result<Url> {
        let bad = || {
            invalid(
                "That return URL does not belong to this sign-in. Copy the complete URL from the browser's address bar.",
            )
        };
        if self.sent {
            return Err(invalid(
                "The return URL was already sent; waiting for Google.",
            ));
        }
        if self.started.elapsed() >= TIMEOUT {
            return Err(invalid("Sign-in expired. Run the login command again."));
        }
        if text.len() > 16384 || text.chars().any(char::is_whitespace) {
            return Err(bad());
        }
        let url = Url::parse(text).map_err(|_| bad())?;
        if url.origin() != self.redirect.origin()
            || url.path() != self.redirect.path()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || one(&url, "state").as_deref() != Some(&self.state)
            || one(&url, "code").is_none_or(|code| code.is_empty())
        {
            return Err(bad());
        }
        Ok(url)
    }
    fn forward(&mut self, text: &str) -> io::Result<()> {
        let url = self.callback(text)?;
        let port = self
            .redirect
            .port()
            .ok_or_else(|| invalid("Missing callback port"))?;
        // Direct loopback TCP: no DNS, proxies, redirects or arbitrary URL fetch.
        let mut socket = TcpStream::connect_timeout(
            &SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(),
            Duration::from_secs(5),
        )?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        self.sent = true;
        write!(
            socket,
            "GET /?{} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n",
            url.query().unwrap_or_default()
        )?;
        let mut status = String::new();
        io::BufReader::new(socket)
            .take(1024)
            .read_line(&mut status)?;
        if !status.starts_with("HTTP/1.1 200")
            && !status.starts_with("HTTP/1.0 200")
            && !status.starts_with("HTTP/1.1 302")
        {
            return Err(invalid(
                "The local sign-in callback was not accepted. Run the login command again.",
            ));
        }
        Ok(())
    }
}
fn one(url: &Url, key: &str) -> Option<String> {
    let mut found = url.query_pairs().filter(|(k, _)| k == key);
    let value = found.next()?.1.into_owned();
    found.next().is_none().then_some(value)
}

enum Event {
    Output(String),
    Error(String),
    Input(String),
    End,
}
fn lines(reader: impl Read + Send + 'static, send: mpsc::SyncSender<Event>, stderr: bool) {
    std::thread::spawn(move || {
        let mut reader = io::BufReader::new(reader);
        loop {
            let mut line = String::new();
            let read = reader.by_ref().take(MAX_LINE + 1).read_line(&mut line);
            if !matches!(read, Ok(1..)) || line.len() as u64 > MAX_LINE {
                break;
            }
            let event = if stderr {
                Event::Error(line)
            } else {
                Event::Output(line)
            };
            if send.send(event).is_err() {
                return;
            }
        }
        let _ = send.send(Event::End);
    });
}

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id().cast_signed()) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Echo(Option<rustix::termios::Termios>);
impl Echo {
    fn hide() -> Self {
        let saved = rustix::termios::tcgetattr(io::stdin()).ok();
        if let Some(saved) = &saved {
            let mut hidden = saved.clone();
            hidden.local_modes.remove(
                rustix::termios::LocalModes::ECHO
                    | rustix::termios::LocalModes::ICANON
                    | rustix::termios::LocalModes::ISIG,
            );
            let _ = rustix::termios::tcsetattr(
                io::stdin(),
                rustix::termios::OptionalActions::Now,
                &hidden,
            );
        }
        Self(saved)
    }
}
impl Drop for Echo {
    fn drop(&mut self) {
        if let Some(saved) = &self.0 {
            let _ = rustix::termios::tcsetattr(
                io::stdin(),
                rustix::termios::OptionalActions::Now,
                saved,
            );
        }
    }
}
fn send(child: &mut Child, id: u32, method: &str, params: &Value) -> io::Result<()> {
    let input = child
        .0
        .stdin
        .as_mut()
        .ok_or_else(|| invalid("Antigravity input closed"))?;
    serde_json::to_writer(
        &mut *input,
        &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
    )?;
    input.write_all(b"\n")?;
    input.flush()
}

/// Sign in to the managed Antigravity runtime without sending a model prompt.
/// Run this in a terminal on the machine hosting the agent. Remote browsers can
/// paste their loopback return URL; only this flow's exact callback is relayed.
///
/// # Errors
/// Installation, initialization, sign-in or account model discovery can fail.
pub fn login() -> io::Result<()> {
    let config = crate::DaemonConfig::from_env().agents;
    let dir = config
        .dir
        .ok_or_else(|| invalid("No agent data directory"))?;
    let known = crate::agents::known("antigravity")
        .ok_or_else(|| invalid("Missing Antigravity provider"))?;
    let installed = crate::agents::install_dir(&dir, known);
    let program = known.adapter.installed_program(&installed);
    if !program.is_file() {
        println!("Installing Google's Antigravity ACP runtime (about 1.1 GB)…");
        crate::agents::install(&dir, known, &[]).map_err(|err| invalid(&err))?;
    }
    let profile = profile(&dir);
    private_dir(&profile)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(profile.join("login.lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| invalid("Another Antigravity login is already running"))?;
    let mut child = Child(
        command(&program, &profile)?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?,
    );
    let (tx, rx) = mpsc::sync_channel(32);
    lines(
        child
            .0
            .stdout
            .take()
            .ok_or_else(|| invalid("Missing runtime output"))?,
        tx.clone(),
        false,
    );
    lines(
        child
            .0
            .stderr
            .take()
            .ok_or_else(|| invalid("Missing runtime diagnostics"))?,
        tx.clone(),
        true,
    );
    let _echo = Echo::hide();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut line = Vec::new();
        let mut byte = [0];
        while stdin.read_exact(&mut byte).is_ok() {
            match byte[0] {
                3 | 4 => {
                    let _ = tx.send(Event::Input("cancel".into()));
                    break;
                }
                b'\n' | b'\r' => {
                    let text = String::from_utf8_lossy(&line).into_owned();
                    line.clear();
                    if tx.send(Event::Input(text)).is_err() {
                        break;
                    }
                }
                8 | 127 => {
                    line.pop();
                }
                byte if line.len() < 16385 => line.push(byte),
                _ => {}
            }
        }
    });
    send(
        &mut child,
        1,
        "initialize",
        &json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false},"clientInfo":{"name":"clackatoo","version":env!("CARGO_PKG_VERSION")}}),
    )?;
    finish_login(&mut child, &rx)
}

fn finish_login(child: &mut Child, rx: &mpsc::Receiver<Event>) -> io::Result<()> {
    println!("Starting Antigravity sign-in… (type cancel and Enter to stop)");
    let deadline = Instant::now() + TIMEOUT;
    let mut authorization: Option<Authorization> = None;
    loop {
        let event = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| invalid("Antigravity sign-in timed out"))?;
        match event {
            Event::Output(line) | Event::Error(line) if authorization_line(&line).is_some() => {
                let next = Authorization::parse(authorization_line(&line).unwrap_or_default())?;
                if authorization.as_ref().is_some_and(|a| a.url == next.url) {
                    continue;
                }
                if authorization.is_some() {
                    return Err(invalid(
                        "Google restarted sign-in. Run the login command again.",
                    ));
                }
                println!(
                    "\nOpen this link in your browser:\n{}\n\nIf the browser returns to an unreachable 127.0.0.1 page, paste its complete URL here and press Enter. Input is hidden.\n",
                    next.url
                );
                authorization = Some(next);
            }
            Event::Input(line) => {
                let text = line.trim();
                if text.eq_ignore_ascii_case("cancel") {
                    return Err(invalid("Sign-in cancelled"));
                }
                if text.is_empty() {
                    continue;
                }
                if let Some(auth) = &mut authorization {
                    match auth.forward(text) {
                        Ok(()) => println!("Return URL sent. Waiting for Google…"),
                        Err(err) => eprintln!("{err}"),
                    }
                } else {
                    eprintln!("Wait for the sign-in link first.");
                }
            }
            Event::Output(line) => {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("error").is_some() {
                    return Err(invalid(
                        "Antigravity could not sign in or load this account's models. Please retry.",
                    ));
                }
                match message.get("id").and_then(Value::as_u64) {
                    Some(1) => send(
                        child,
                        2,
                        "authenticate",
                        &json!({"methodId":"oauth-personal"}),
                    )?,
                    Some(2) => send(
                        child,
                        3,
                        "session/new",
                        &json!({"cwd":std::env::current_dir()?,"mcpServers":[]}),
                    )?,
                    Some(3) => {
                        println!(
                            "Signed in. Select Antigravity in Clackatoo and send a message; the model picker will load your account's models."
                        );
                        return Ok(());
                    }
                    _ => {}
                }
            }
            Event::Error(_) => {} // Provider diagnostics may contain tokens/account details.
            Event::End => return Err(invalid("Antigravity stopped before sign-in completed")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn flow() -> Authorization {
        Authorization::parse("https://accounts.google.com/o/oauth2/v2/auth?state=abc&response_type=code&redirect_uri=http%3A%2F%2F127.0.0.1%3A45678%2F").unwrap()
    }
    #[test]
    fn profile_is_private_and_runtime_never_uses_tmpfs_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        let command = command(Path::new("/runtime"), &profile).unwrap();
        assert_eq!(
            fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(profile.join("antigravity-acp/settings.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(env[std::ffi::OsStr::new("GEMINI_API_KEY")], None);
        assert_eq!(
            env[std::ffi::OsStr::new("TMPDIR")],
            Some(profile.join("antigravity-acp/tmp").as_os_str())
        );
    }

    #[test]
    fn callback_forwarding_contacts_only_the_validated_loopback_listener() {
        use std::net::TcpListener;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut auth = Authorization::parse(&format!("https://accounts.google.com/o/oauth2/v2/auth?state=abc&response_type=code&redirect_uri=http%3A%2F%2F127.0.0.1%3A{port}%2F")).unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = io::BufReader::new(socket.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            assert_eq!(request, "GET /?code=secret&state=abc HTTP/1.1\r\n");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        auth.forward(&format!("http://127.0.0.1:{port}/?code=secret&state=abc"))
            .unwrap();
        assert!(auth.sent);
        server.join().unwrap();
    }

    #[test]
    fn callback_is_bound_to_exact_loopback_flow_and_is_single_use() {
        let mut auth = flow();
        let good = "http://127.0.0.1:45678/?code=secret&state=abc";
        assert!(auth.callback(good).is_ok());
        for bad in [
            "http://127.0.0.1:45679/?code=secret&state=abc",
            "http://localhost:45678/?code=secret&state=abc",
            "http://127.0.0.1:45678/else?code=secret&state=abc",
            "http://127.0.0.1:45678/?code=secret&state=wrong",
            "http://127.0.0.1:45678/?code=secret&state=abc&state=abc",
            "http://user@127.0.0.1:45678/?code=secret&state=abc",
            "http://127.0.0.1:45678/?code=secret&state=abc#fragment",
            "https://evil.test/?code=secret&state=abc",
        ] {
            assert!(auth.callback(bad).is_err(), "{bad}");
        }
        auth.sent = true;
        assert!(auth.callback(good).is_err());
        auth.sent = false;
        auth.started = Instant::now().checked_sub(TIMEOUT).unwrap();
        assert!(auth.callback(good).is_err());
    }
    #[test]
    fn authorization_rejects_other_hosts_and_non_loopback_callbacks() {
        for text in [
            flow()
                .url
                .to_string()
                .replace("accounts.google.com", "evil.test"),
            flow().url.to_string().replace("127.0.0.1", "192.168.1.1"),
            flow().url.to_string().replace("45678", "80"),
            format!("{}&state=other", flow().url),
        ] {
            assert!(Authorization::parse(&text).is_err());
        }
    }
}
