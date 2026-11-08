//! Handing a window to the process of windows, which opens it without
//! setting up a GPU of its own, the most of the time a window takes to
//! open (see [`crate::app::host`]).
//!
//! The process of windows is started apart from whoever asked for its
//! first window, in a session of its own with no terminal (see
//! [`start`]), and listens on `window.sock` in the runtime directory,
//! which only the user can get into; both ends check the other is the
//! user's own. `clackatoo` connects and sends, on one line of JSON, what
//! it would open: the window's arguments, the desktop's tokens it was
//! launched with, its environment and directory, which whatever the
//! window starts is started with, what must match for the window to be the
//! one it would have opened ([`MATCH_ENV`] and clackatoo's directories),
//! and by when, on the system's monotonic clock, it must be taken on and
//! opened.
//!
//! The process answers `{"took":true}` once its event loop has taken the
//! window on, only before the deadline to, then `{"opened":true}` or
//! `{"failed":WHY}`. A window it opens after the deadline to open it is
//! closed again, as the asker has opened its own by then: there are never
//! two. One that wouldn't open exactly the window the asker would, being
//! another build or seeing another display or config, answers
//! `{"refused":WHY}`. The asker then starts a process of windows of its
//! own, for the windows after it too, unless the answer says `"keep":true`:
//! the asker is the odd one out, or an older build than the process asked,
//! and its process takes no others. Only if that fails does the asker open
//! the window itself.
//!
//! A build is the program's file, so a build put in place by `clackatoo
//! update` or a deploy is never asked to open a window by an older one, or
//! the other way round: the first window of the new build starts its
//! process, which takes the socket over. Between builds, only the
//! request's `build` and the `refused` answer are a contract: a process
//! refuses any other build before reading anything else.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clackatoo_daemon::RuntimeDir;
use clackatoo_daemon::launch::Origin;
use clackatoo_daemon::remote::Remote;
use serde_json::{Map, Value, json};

use crate::app::host::Spec;
use crate::launch::LaunchToken;
use crate::{FontFixed, FontOptions, GraphicsApi};

/// The socket of the process of windows, in the runtime directory.
const SOCKET: &str = "window.sock";

/// The command, of the `clackatoo` program, that runs a process of
/// windows: [`crate::host`].
pub const HOST_COMMAND: &str = "window-host";

/// The most a request or an answer may be: a request carries an
/// environment.
const MOST: usize = 1024 * 1024;

/// How long a request may take to arrive whole, once connected.
const READ_WAIT: Duration = Duration::from_millis(500);

/// How long past a deadline the asker reads for an answer sent just
/// before it.
const SLACK: Duration = Duration::from_millis(200);

/// What a window must match in the process of windows to be the window
/// its asker would have opened: the display it's shown on, and the GPU
/// it's drawn with, which the process shares. clackatoo's config and state
/// directories are compared too. Everything else in the asker's
/// environment goes with the window, for what it starts.
const MATCH_ENV: [&str; 5] = [
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "WGPU_POWER_PREF",
    "WGPU_BACKEND",
    "WGPU_ADAPTER_NAME",
];

/// Set to `off`, windows neither ask a process of windows for themselves
/// nor take others: every window has a process of its own, as before there
/// was a choice.
pub(crate) const OFF_VAR: &str = "CLACKATOO_WINDOW_HOST";

/// Whether windows may share a process, as they do unless [`OFF_VAR`]
/// says not.
pub(crate) fn on() -> bool {
    std::env::var_os(OFF_VAR).is_none_or(|value| value != "off")
}

/// Now, on the system's monotonic clock, in nanoseconds: one clock for
/// every process, so deadlines can be sent between them.
fn now() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let seconds = u64::try_from(now.tv_sec).unwrap_or_default();
    let nanos = u64::try_from(now.tv_nsec).unwrap_or_default();
    seconds.saturating_mul(1_000_000_000).saturating_add(nanos)
}

/// `wait`, in nanoseconds.
fn nanos(wait: Duration) -> u64 {
    u64::try_from(wait.as_nanos()).unwrap_or(u64::MAX)
}

/// `wait` from now, on [`now`]'s clock.
fn after(wait: Duration) -> u64 {
    now().saturating_add(nanos(wait))
}

/// How long until `deadline`, on [`now`]'s clock: none once it's passed.
fn until(deadline: u64) -> Duration {
    Duration::from_nanos(deadline.saturating_sub(now()))
}

/// A build of clackatoo, as the program's file: its device, inode, size
/// and when it was last changed. A version and commit wouldn't do: builds
/// from a tree with changes not committed share them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Build(String);

impl Build {
    /// The build this process runs, if it can be found: the file it was
    /// started from, even once another has been put in its place.
    pub(crate) fn running() -> Option<Self> {
        Self::of(Path::new("/proc/self/exe"))
    }

    /// The build at `path`, if there's a file there.
    pub(crate) fn of(path: &Path) -> Option<Self> {
        let file = std::fs::metadata(path).ok()?;
        Some(Self(format!(
            "{}:{}:{}:{}.{:09}",
            file.dev(),
            file.ino(),
            file.size(),
            file.mtime(),
            file.mtime_nsec()
        )))
    }

    /// When its file was last changed, as seconds and nanoseconds.
    fn changed(&self) -> Option<(i64, u32)> {
        let (seconds, nanos) = self.0.rsplit(':').next()?.split_once('.')?;
        Some((seconds.parse().ok()?, nanos.parse().ok()?))
    }
}

/// Where a process of windows listens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Socket {
    /// At `window.sock`, for every window asked for.
    Shared,
    /// At a path only the one asker knows: the process takes that window,
    /// and any its windows open, but no others.
    Private(PathBuf),
}

impl Socket {
    /// A private socket in `runtime`, for this process's window.
    pub(crate) fn private(runtime: &RuntimeDir) -> Self {
        let name = format!("window-for-{}.sock", std::process::id());
        Self::Private(runtime.path().join(name))
    }

    fn path(&self, runtime: &RuntimeDir) -> PathBuf {
        match self {
            Self::Shared => runtime.path().join(SOCKET),
            Self::Private(path) => path.clone(),
        }
    }
}

/// How long an asker waits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Waits {
    /// For its window to be taken on: longer than a busy process takes to
    /// handle what's before it, shorter than opening a window without
    /// its GPU.
    pub(crate) take: Duration,
    /// For a process just started to listen, and take the window on.
    pub(crate) start: Duration,
    /// For a window taken on to open: a first window waits for the GPU,
    /// which on some machines takes many seconds.
    pub(crate) open: Duration,
}

impl Default for Waits {
    fn default() -> Self {
        Self {
            take: Duration::from_millis(500),
            start: Duration::from_secs(5),
            open: Duration::from_secs(20),
        }
    }
}

/// What's asked for: a window, opened as this process would open it.
#[derive(Debug)]
pub(crate) struct Request<'a> {
    pub(crate) spec: &'a Spec,
    pub(crate) graphics_api: GraphicsApi,
    pub(crate) runtime: &'a RuntimeDir,
    pub(crate) waits: Waits,
}

/// The window wasn't opened by another process, as this says, with the
/// tokens the asker was launched with back.
#[derive(Debug)]
pub(crate) struct NotTaken {
    pub(crate) launch: LaunchToken,
    pub(crate) why: String,
    /// Whether a process started for the window is to take the windows
    /// asked for after it: not if the process asked keeps them.
    pub(crate) host: bool,
}

/// The process asked refused the window, and keeps the socket.
#[derive(Debug)]
struct Kept(String);

impl std::fmt::Display for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refused: {}, and it takes the windows asked for after",
            self.0
        )
    }
}

impl std::error::Error for Kept {}

/// This process's environment and directory, for a window it asks for:
/// whatever the window starts is started with them. Variables that aren't
/// UTF-8 can't be sent, and are left out.
pub(crate) fn here() -> Origin {
    let env: Vec<(OsString, OsString)> = std::env::vars()
        .map(|(name, value)| (name.into(), value.into()))
        .collect();
    Origin {
        env: Some(env.into()),
        dir: std::env::current_dir().ok(),
    }
}

/// Starts a process of windows listening `at`, apart from this one: in a
/// session of its own, so this one's terminal, its signals and its end
/// reach none of the windows, with no input or output of its own (it logs
/// to `window.log`), and outside any directory a window might want gone.
/// It's this very build, even if another has been put in its place.
///
/// # Errors
///
/// If it can't be started.
pub(crate) fn start(at: &Socket, graphics_api: GraphicsApi) -> io::Result<Child> {
    // By its name, so it's named as clackatoo, while that's still this
    // build.
    let program = std::env::current_exe()
        .ok()
        .filter(|program| Build::of(program).is_some_and(|it| Some(it) == Build::running()))
        .unwrap_or_else(|| PathBuf::from("/proc/self/exe"));
    let mut command = Command::new(program);
    command
        .arg(HOST_COMMAND)
        .args(["--renderer", renderer_name(graphics_api)])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir("/");
    if let Socket::Private(path) = at {
        command.arg("--socket").arg(path);
    }
    command.spawn()
}

/// Asks the process of windows listening `at` to open the window
/// `request` describes, with the desktop's `launch` tokens; or with
/// `starting`, the one just started for it, waiting for it to listen.
/// Returns once the window is open, or if it wasn't, why.
pub(crate) fn ask(
    request: &Request<'_>,
    launch: LaunchToken,
    at: &Socket,
    starting: Option<&mut Child>,
) -> Result<(), NotTaken> {
    let not = |launch, why: String| {
        Err(NotTaken {
            launch,
            why,
            host: true,
        })
    };
    if !on() {
        return not(launch, format!("{OFF_VAR} is off"));
    }
    let Some(build) = Build::running() else {
        return not(launch, "this program can't be found".into());
    };
    let started = starting.is_some();
    let stream = match connect(&at.path(request.runtime), starting, request.waits.start) {
        Ok(stream) => stream,
        Err(err) => return not(launch, format!("{err:#}")),
    };
    let waits = request.waits;
    let take_by = after(if started { waits.start } else { waits.take });
    let open_by = take_by.saturating_add(nanos(waits.open));
    let line = request_line(request, &build, &launch, take_by, open_by);
    match asked(&stream, &line, take_by, open_by) {
        Ok(()) => Ok(()),
        Err(err) => Err(NotTaken {
            launch,
            why: format!("{err:#}"),
            host: err.downcast_ref::<Kept>().is_none(),
        }),
    }
}

/// Connects to the socket at `path`; with `starting`, the process just
/// started to listen there, waiting up to `wait` for it to, as long as
/// it runs.
fn connect(
    path: &Path,
    starting: Option<&mut Child>,
    wait: Duration,
) -> anyhow::Result<UnixStream> {
    let Some(child) = starting else {
        return UnixStream::connect(path).context("no process of windows");
    };
    let deadline = Instant::now() + wait;
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) && Instant::now() < deadline =>
            {
                if let Some(status) = child.try_wait()? {
                    anyhow::bail!("the process of windows started ended: {status}");
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(err) => return Err(err).context("the process of windows started isn't listening"),
        }
    }
}

/// Sends `line` on `stream`, and waits for the window to be taken on by
/// `take_by` and to open by `open_by`.
fn asked(stream: &UnixStream, line: &str, take_by: u64, open_by: u64) -> anyhow::Result<()> {
    ours(stream)?;
    stream.set_write_timeout(Some(until(take_by).max(Duration::from_millis(1))))?;
    let mut writer = stream;
    writer.write_all(line.as_bytes())?;
    let mut answers = Answers::default();
    let took = answers
        .next(stream, until(take_by) + SLACK)
        .context("the window wasn't taken on in time")?;
    if let Some(why) = took.get("refused").and_then(Value::as_str) {
        if took.get("keep") == Some(&Value::Bool(true)) {
            return Err(Kept(why.to_owned()).into());
        }
        anyhow::bail!("refused: {why}");
    }
    anyhow::ensure!(took.get("took") == Some(&Value::Bool(true)), "not taken on");
    // Not open by then, it's closed again as it opens: this process opens
    // it instead.
    let opened = answers
        .next(stream, until(open_by) + SLACK)
        .context("the window taken on didn't open in time")?;
    if opened.get("opened") == Some(&Value::Bool(true)) {
        return Ok(());
    }
    let why = opened.get("failed").and_then(Value::as_str);
    anyhow::bail!("couldn't open: {}", why.unwrap_or("it didn't say why"));
}

/// Lines read from a stream, each a JSON object.
#[derive(Debug, Default)]
struct Answers {
    read: Vec<u8>,
}

impl Answers {
    /// The next line, waiting no more than `wait` for it.
    fn next(&mut self, stream: &UnixStream, wait: Duration) -> anyhow::Result<Map<String, Value>> {
        let line = read_line(stream, &mut self.read, Instant::now() + wait)?;
        match serde_json::from_slice(&line)? {
            Value::Object(answer) => Ok(answer),
            _ => anyhow::bail!("an answer that isn't an object"),
        }
    }
}

/// Reads from `stream` into `read` until a whole line is there, by
/// `deadline` however slowly it comes, and takes it out of `read`.
fn read_line(
    stream: &UnixStream,
    read: &mut Vec<u8>,
    deadline: Instant,
) -> anyhow::Result<Vec<u8>> {
    let mut buffer = [0; 4096];
    loop {
        if let Some(end) = read.iter().position(|&byte| byte == b'\n') {
            let rest = read.split_off(end + 1);
            return Ok(std::mem::replace(read, rest));
        }
        anyhow::ensure!(read.len() < MOST, "a line too long");
        let left = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(!left.is_zero(), "no answer in time");
        stream.set_read_timeout(Some(left))?;
        let mut reader = stream;
        match reader.read(&mut buffer) {
            Ok(0) => anyhow::bail!("the other end hung up"),
            Ok(n) => read.extend_from_slice(&buffer[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                anyhow::bail!("no answer in time");
            }
            Err(err) => return Err(err.into()),
        }
    }
}

/// Checks that the other end of `stream` is the user's own program.
fn ours(stream: &UnixStream) -> io::Result<()> {
    let peer = rustix::net::sockopt::socket_peercred(stream)?;
    if peer.uid != rustix::process::getuid() {
        return Err(io::Error::other(
            "another user's program is at the other end",
        ));
    }
    Ok(())
}

/// The request, as one line.
fn request_line(
    request: &Request<'_>,
    build: &Build,
    launch: &LaunchToken,
    take_by: u64,
    open_by: u64,
) -> String {
    let spec = request.spec;
    let (wayland, x11) = launch.raw();
    let origin = &spec.origin;
    let env: Vec<Value> = (origin.env.iter().flat_map(|env| env.iter()))
        .filter_map(|(name, value)| Some(json!([name.to_str()?, value.to_str()?])))
        .collect();
    let line = json!({
        "build": build.0,
        "take_by": take_by,
        "open_by": open_by,
        "match": window_match(),
        "renderer": renderer_name(request.graphics_api),
        "remote": spec.remote.as_ref().map(|remote| &remote.host),
        "remote_command": spec.remote_command,
        "pick_remote": spec.pick_remote,
        "font": spec.font.family,
        "font_size": spec.font.size,
        "font_fixed": [spec.font_fixed.family, spec.font_fixed.size],
        "wayland_token": wayland,
        "x11_token": x11,
        "env": origin.env.as_ref().map(|_| env),
        "dir": origin.dir.as_ref().and_then(|dir| dir.to_str()),
    });
    format!("{line}\n")
}

/// The name a graphics API goes by on the command line.
pub(crate) fn renderer_name(api: GraphicsApi) -> &'static str {
    match api {
        GraphicsApi::Vulkan => "vulkan",
        GraphicsApi::OpenGl => "opengl",
    }
}

/// What a window must match, as this process has it: [`MATCH_ENV`], and
/// clackatoo's config and state directories. A relative `WAYLAND_DISPLAY`
/// names a socket in the runtime directory, so it's made a path, which
/// says which display it is wherever it's read.
fn window_match() -> Map<String, Value> {
    let mut matched = Map::new();
    for name in MATCH_ENV {
        let mut value = std::env::var(name).ok().filter(|value| !value.is_empty());
        if name == "WAYLAND_DISPLAY"
            && let Some(display) = &value
            && !display.starts_with('/')
            && let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR")
        {
            value = Some(format!("{runtime}/{display}"));
        }
        matched.insert(name.to_owned(), value.map_or(Value::Null, Value::String));
    }
    let dir = |dir: Option<PathBuf>| {
        (dir.as_deref().and_then(Path::to_str)).map_or(Value::Null, Into::into)
    };
    matched.insert("config".into(), dir(clackatoo_daemon::config_dir()));
    matched.insert("state".into(), dir(clackatoo_daemon::state_dir()));
    matched
}

/// A window asked for, which the event loop takes on or lets go.
#[derive(Debug)]
pub(crate) struct Asked {
    pub(crate) spec: Spec,
    pub(crate) launch: LaunchToken,
    /// When it was asked for, which the window's start is timed from.
    pub(crate) at: Instant,
    /// When the asker stops waiting to hear it was taken on, and to hear
    /// it opened, on [`now`]'s clock.
    take_by: u64,
    open_by: u64,
    stream: UnixStream,
}

impl Asked {
    /// Takes the window on, saying so, if the asker still waits.
    pub(crate) fn take(&mut self) -> bool {
        now() < self.take_by && self.say(&json!({ "took": true })).is_ok()
    }

    /// Whether the asker has stopped waiting for the window to open, and
    /// opened its own.
    pub(crate) fn late(&self) -> bool {
        now() >= self.open_by
    }

    /// Tells the asker the window opened, or why not.
    pub(crate) fn opened(mut self, result: Result<(), String>) {
        let said = match result {
            Ok(()) => json!({ "opened": true }),
            Err(why) => json!({ "failed": why }),
        };
        // An asker that's gone has nothing to hear.
        let _ = self.say(&said);
    }

    fn say(&mut self, answer: &Value) -> io::Result<()> {
        self.stream.write_all(format!("{answer}\n").as_bytes())
    }
}

/// Listening for windows asked for. Dropped, it takes the socket away, if
/// it's still this one's, and windows asked for after go elsewhere.
#[derive(Debug)]
pub(crate) struct Listening {
    path: PathBuf,
    /// The socket's device and inode, to tell it from one another process
    /// has put in its place since.
    id: (u64, u64),
}

impl Drop for Listening {
    fn drop(&mut self) {
        let still_ours = std::fs::symlink_metadata(&self.path)
            .is_ok_and(|socket| (socket.dev(), socket.ino()) == self.id);
        if still_ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Starts listening `at`, in `runtime`, for windows asked for, on a thread
/// of its own, in place of any process listening there before. Each asked
/// for by this build of the program, matching this process and drawn with
/// `graphics_api`, is handed to `deliver`; any other is refused.
///
/// # Errors
///
/// If the runtime directory or the socket can't be made.
pub(crate) fn listen(
    runtime: &RuntimeDir,
    at: &Socket,
    graphics_api: GraphicsApi,
    deliver: impl Fn(Asked) + Send + Sync + 'static,
) -> anyhow::Result<Listening> {
    runtime.create_private()?;
    let build = Build::running().context("this program can't be found")?;
    let ours = Arc::new(Expected {
        build,
        matched: window_match(),
        renderer: renderer_name(graphics_api),
    });
    let path = at.path(runtime);
    // A shared socket is made under a name of its own, then put in place
    // in one step, so whoever connects meanwhile finds the last process's
    // socket or this one, never none; one left by a process gone is
    // replaced.
    let made = match at {
        Socket::Shared => runtime
            .path()
            .join(format!("window-{}.sock", std::process::id())),
        Socket::Private(path) => path.clone(),
    };
    let (listener, id) = bind(&made, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&made);
    })?;
    let listening = Listening { path, id };
    let deliver = Arc::new(deliver);
    std::thread::Builder::new()
        .name("windows".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let at = Instant::now();
                let ours = Arc::clone(&ours);
                let deliver = Arc::clone(&deliver);
                // Each on a thread of its own: a slow asker holds up no
                // other.
                let read = std::thread::Builder::new()
                    .name("window-asked".into())
                    .spawn(move || match read(stream, &ours, at) {
                        Ok(Some(asked)) => deliver(asked),
                        Ok(None) => {}
                        Err(err) => log::info!("a window asked for wasn't taken on: {err:#}"),
                    });
                if let Err(err) = read {
                    log::warn!("no thread to read a window asked for: {err}");
                }
            }
        })?;
    Ok(listening)
}

/// Binds a socket for the user alone at `made`, and puts it at `path`.
fn bind(made: &Path, path: &Path) -> anyhow::Result<(UnixListener, (u64, u64))> {
    match std::fs::remove_file(made) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let listener = UnixListener::bind(made)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(made, std::fs::Permissions::from_mode(0o600))?;
    }
    let socket = std::fs::symlink_metadata(made)?;
    if made != path {
        std::fs::rename(made, path)?;
    }
    Ok((listener, (socket.dev(), socket.ino())))
}

/// What a request must match to be taken on.
#[derive(Debug)]
struct Expected {
    build: Build,
    matched: Map<String, Value>,
    renderer: &'static str,
}

/// Reads what's asked for on `stream`, at `at`, all of it within
/// [`READ_WAIT`]. Refuses it, returning `None`, if a window opened here
/// wouldn't be the one asked for.
fn read(stream: UnixStream, expected: &Expected, at: Instant) -> anyhow::Result<Option<Asked>> {
    ours(&stream).context("asked by another user's program")?;
    stream.set_write_timeout(Some(READ_WAIT))?;
    let line = read_line(&stream, &mut Vec::new(), at + READ_WAIT)?;
    let Value::Object(request) = serde_json::from_slice(&line)? else {
        anyhow::bail!("a request that isn't an object");
    };
    let mut stream = stream;
    if let Some(refused) = refusal(&request, expected) {
        log::info!("a window asked for is refused: {}", refused.why);
        let answer = json!({ "refused": refused.why, "keep": refused.keep });
        stream.write_all(format!("{answer}\n").as_bytes())?;
        return Ok(None);
    }
    let text = |name: &str| request.get(name).and_then(Value::as_str).map(str::to_owned);
    let number = |name: &str| request.get(name).and_then(Value::as_u64).unwrap_or(0);
    let fixed = request.get("font_fixed").and_then(Value::as_array);
    let fixed = |index: usize| {
        fixed
            .and_then(|fixed| fixed.get(index))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let remote_command = text("remote_command").context("no remote command")?;
    #[allow(clippy::cast_possible_truncation)]
    let size = request
        .get("font_size")
        .and_then(Value::as_f64)
        .context("no font size")? as f32;
    let env = request.get("env").and_then(Value::as_array).map(|env| {
        let pair = |pair: &Value| {
            let pair = pair.as_array()?;
            let name = pair.first()?.as_str()?;
            let value = pair.get(1)?.as_str()?;
            Some((OsString::from(name), OsString::from(value)))
        };
        env.iter().filter_map(pair).collect::<Vec<_>>().into()
    });
    let spec = Spec {
        remote: text("remote").map(|host| Remote {
            host,
            command: remote_command.clone(),
        }),
        pick_remote: request.get("pick_remote") == Some(&Value::Bool(true)),
        remote_command,
        font: FontOptions {
            family: text("font"),
            size,
        },
        font_fixed: FontFixed {
            family: fixed(0),
            size: fixed(1),
        },
        origin: Origin {
            env,
            dir: text("dir").map(PathBuf::from),
        },
    };
    Ok(Some(Asked {
        spec,
        launch: LaunchToken::from_raw(text("wayland_token"), text("x11_token")),
        at,
        take_by: number("take_by"),
        open_by: number("open_by"),
        stream,
    }))
}

/// Why a window is refused, and whether the process refusing it keeps
/// taking the windows asked for after it.
#[derive(Debug, PartialEq, Eq)]
struct Refusal {
    why: String,
    keep: bool,
}

/// Why a window opened here wouldn't be the one `request` asks for, if it
/// wouldn't. A newer build takes windows from here on, but not an older
/// one, nor an asker that sees another display or config: the windows
/// asked for after it are most likely this process's.
fn refusal(request: &Map<String, Value>, expected: &Expected) -> Option<Refusal> {
    let build = request.get("build").and_then(Value::as_str);
    if build != Some(expected.build.0.as_str()) {
        let theirs = build.and_then(|build| Build(build.to_owned()).changed());
        return Some(Refusal {
            why: "another build of clackatoo".into(),
            keep: theirs.is_some_and(|theirs| expected.build.changed() > Some(theirs)),
        });
    }
    let matched = request.get("match").and_then(Value::as_object);
    let differs = (expected.matched.iter())
        .find(|(name, value)| matched.and_then(|matched| matched.get(*name)) != Some(value))
        .map(|(name, _)| format!("another {name}"));
    let renderer = request.get("renderer").and_then(Value::as_str);
    let differs = differs
        .or_else(|| (renderer != Some(expected.renderer)).then(|| "another renderer".into()));
    differs.map(|why| Refusal { why, keep: true })
}

#[cfg(test)]
mod tests;
