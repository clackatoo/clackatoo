//! Asking the process that already has windows open for one more, so it
//! opens without setting up a GPU of its own, which is most of the time a
//! window takes to open (see [`crate::app::host`]).
//!
//! A process with windows listens on `window.sock` in the runtime
//! directory, which only the user can get into, and both ends check the
//! other is the user's own. `clackatoo` connects and sends, on one line of
//! JSON, what it would open: the window's arguments, the desktop's tokens
//! it was launched with, the [`WINDOW_ENV`] it has and how long it waits.
//! The process answers `{"took":true}` once its event loop has taken the
//! window on, only while the asker still waits, then `{"opened":true}` or
//! `{"failed":WHY}`. One that wouldn't open exactly the window the asker
//! would, being another build or seeing another display or config,
//! answers `{"refused":WHY}`. Whenever the window isn't taken, the asker
//! opens it itself, and takes the socket over for the next, unless the
//! answer says `"keep":true`: the asker sees another display or config,
//! and is the odd one out, or is an older build than the process asked.
//!
//! A build is the program's file, so a build put in place by `clackatoo
//! update` or a deploy is never asked to open a window by an older one, or
//! the other way round: the first window of the new build takes the socket
//! over. Between builds, only the request's `build` and the `refused`
//! answer are a contract: a process refuses any other build before reading
//! anything else.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clackatoo_daemon::RuntimeDir;
use clackatoo_daemon::remote::Remote;
use serde_json::{Map, Value, json};

use crate::app::host::Spec;
use crate::launch::LaunchToken;
use crate::{FontFixed, FontOptions, GraphicsApi};

/// The socket, in the runtime directory.
const SOCKET: &str = "window.sock";

/// How long an asker waits for its window to be taken on, before opening
/// it itself: longer than a busy window takes to handle what's before it,
/// shorter than opening a window without the process's GPU.
const TAKE_WAIT: Duration = Duration::from_millis(500);

/// How long past [`TAKE_WAIT`] the asker reads for an answer the process
/// sent just in time.
const SLACK: Duration = Duration::from_millis(200);

/// How long an asker waits for a window taken on to open.
const OPEN_WAIT: Duration = Duration::from_secs(10);

/// The most a request or an answer may be.
const MOST: u64 = 64 * 1024;

/// The environment that changes what a window does, which the process
/// opening a window must have as the asker does: the display, where
/// config and state are, how ssh is reached for another machine, the GPU
/// asked for, and the desktop's bus for notifications.
const WINDOW_ENV: [&str; 13] = [
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_DATA_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "SSH_AUTH_SOCK",
    "CLACKATOO_SSH",
    "WGPU_POWER_PREF",
    "WGPU_BACKEND",
    "WGPU_ADAPTER_NAME",
    "CLACKATOO_TEST_FONT",
];

/// Set to `off`, a window neither asks another process for itself nor
/// opens others: every window has a process of its own, as before there
/// was a choice.
pub(crate) const OFF_VAR: &str = "CLACKATOO_WINDOW_HOST";

/// Whether windows may share a process, as they do unless [`OFF_VAR`]
/// says not.
pub(crate) fn on() -> bool {
    std::env::var_os(OFF_VAR).is_none_or(|value| value != "off")
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

/// What's asked for: a window, opened as this process would open it.
#[derive(Debug)]
pub(crate) struct Request<'a> {
    pub(crate) spec: &'a Spec,
    pub(crate) graphics_api: GraphicsApi,
    pub(crate) runtime: &'a RuntimeDir,
}

/// The window wasn't opened by another process, as this says, with the
/// tokens the asker was launched with back.
#[derive(Debug)]
pub(crate) struct NotTaken {
    pub(crate) launch: LaunchToken,
    pub(crate) why: String,
    /// Whether the process opening the window instead is to take the
    /// windows asked for after it: not if the process asked keeps them.
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

/// Asks the process with windows open, if there is one, to open the
/// window `request` describes, with the desktop's `launch` tokens. Returns
/// once it's open, or if it wasn't taken on, why, so this process opens it
/// instead.
pub(crate) fn ask(request: &Request<'_>, launch: LaunchToken) -> Result<(), NotTaken> {
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
    // A local window's daemon is started by whoever opens it, with that
    // process's environment: it must be this one's, as without a host.
    if request.spec.remote.is_none()
        && !request.spec.pick_remote
        && UnixStream::connect(request.runtime.socket_path()).is_err()
    {
        return not(launch, "the daemon isn't running".into());
    }
    let Some(build) = Build::running() else {
        return not(launch, "this program can't be found".into());
    };
    let line = request_line(request, &build, &launch);
    match asked(request.runtime, &line) {
        Ok(()) => Ok(()),
        Err(err) => Err(NotTaken {
            launch,
            why: format!("{err:#}"),
            host: err.downcast_ref::<Kept>().is_none(),
        }),
    }
}

/// Sends `line` to the process listening in `runtime`, and waits for its
/// window to open.
fn asked(runtime: &RuntimeDir, line: &str) -> anyhow::Result<()> {
    let stream =
        UnixStream::connect(runtime.path().join(SOCKET)).context("no process has windows open")?;
    ours(&stream)?;
    stream.set_write_timeout(Some(TAKE_WAIT))?;
    (&stream).write_all(line.as_bytes())?;
    stream.set_read_timeout(Some(TAKE_WAIT + SLACK))?;
    let mut answers = BufReader::new((&stream).take(MOST));
    let took = answer(&mut answers).context("the window wasn't taken on in time")?;
    if let Some(why) = took.get("refused").and_then(Value::as_str) {
        if took.get("keep") == Some(&Value::Bool(true)) {
            return Err(Kept(why.to_owned()).into());
        }
        anyhow::bail!("refused: {why}");
    }
    anyhow::ensure!(took.get("took") == Some(&Value::Bool(true)), "not taken on");
    stream.set_read_timeout(Some(OPEN_WAIT))?;
    let opened = answer(&mut answers);
    match opened {
        Ok(opened) if opened.get("opened") == Some(&Value::Bool(true)) => Ok(()),
        Ok(failed) => {
            let why = failed.get("failed").and_then(Value::as_str);
            anyhow::bail!("couldn't open: {}", why.unwrap_or("it didn't say why"));
        }
        // Taken on, but not said to be open: it may yet open, and another
        // would make two.
        Err(err) => {
            log::warn!("a window taken on by another process isn't open yet: {err:#}");
            Ok(())
        }
    }
}

/// The next answer.
fn answer(answers: &mut impl BufRead) -> anyhow::Result<Map<String, Value>> {
    let mut line = String::new();
    answers.read_line(&mut line)?;
    anyhow::ensure!(line.ends_with('\n'), "no answer");
    match serde_json::from_str(&line)? {
        Value::Object(answer) => Ok(answer),
        _ => anyhow::bail!("an answer that isn't an object"),
    }
}

/// Checks that the other end of `stream` is the user's own program.
fn ours(stream: &UnixStream) -> io::Result<()> {
    let peer = rustix::net::sockopt::socket_peercred(stream)?;
    if peer.uid != rustix::process::getuid() {
        return Err(io::Error::other("another user's program is listening"));
    }
    Ok(())
}

/// The request, as one line.
fn request_line(request: &Request<'_>, build: &Build, launch: &LaunchToken) -> String {
    let spec = request.spec;
    let (wayland, x11) = launch.raw();
    let line = json!({
        "build": build.0,
        "wait_ms": u64::try_from(TAKE_WAIT.as_millis()).unwrap_or(u64::MAX),
        "env": window_env(),
        "renderer": renderer_name(request.graphics_api),
        "remote": spec.remote.as_ref().map(|remote| &remote.host),
        "remote_command": spec.remote_command,
        "pick_remote": spec.pick_remote,
        "font": spec.font.family,
        "font_size": spec.font.size,
        "font_fixed": [spec.font_fixed.family, spec.font_fixed.size],
        "wayland_token": wayland,
        "x11_token": x11,
    });
    format!("{line}\n")
}

fn renderer_name(api: GraphicsApi) -> &'static str {
    match api {
        GraphicsApi::Vulkan => "vulkan",
        GraphicsApi::OpenGl => "opengl",
    }
}

/// [`WINDOW_ENV`] as this process has it. A relative `WAYLAND_DISPLAY`
/// names a socket in the runtime directory, so it's made a path, which
/// says which display it is wherever it's read.
fn window_env() -> Map<String, Value> {
    let mut env = Map::new();
    for name in WINDOW_ENV {
        let mut value = std::env::var(name).ok().filter(|value| !value.is_empty());
        if name == "WAYLAND_DISPLAY"
            && let Some(display) = &value
            && !display.starts_with('/')
            && let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR")
        {
            value = Some(format!("{runtime}/{display}"));
        }
        env.insert(name.to_owned(), value.map_or(Value::Null, Value::String));
    }
    env
}

/// A window asked for, which the event loop takes on or lets go.
#[derive(Debug)]
pub(crate) struct Asked {
    pub(crate) spec: Spec,
    pub(crate) launch: LaunchToken,
    /// When it was asked for, which the window's start is timed from.
    pub(crate) at: Instant,
    /// When the asker stops waiting to hear it was taken on.
    deadline: Instant,
    stream: UnixStream,
}

impl Asked {
    /// Takes the window on, saying so, if the asker still waits.
    pub(crate) fn take(&mut self) -> bool {
        Instant::now() < self.deadline && self.say(&json!({ "took": true })).is_ok()
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

/// Listening for windows asked for, as the process with windows open.
/// Dropped, it takes the socket away, if it's still this one's.
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

/// Starts listening in `runtime` for windows asked for, on a thread of its
/// own, in place of any process listening before. Each asked for by this
/// build of the program, with this process's environment and drawing with
/// `graphics_api`, is handed to `deliver`; any other is refused.
///
/// # Errors
///
/// If the runtime directory or the socket can't be made.
pub(crate) fn listen(
    runtime: &RuntimeDir,
    graphics_api: GraphicsApi,
    deliver: impl Fn(Asked) + Send + 'static,
) -> anyhow::Result<Listening> {
    runtime.create_private()?;
    let build = Build::running().context("this program can't be found")?;
    let ours = Expected {
        build,
        env: window_env(),
        renderer: renderer_name(graphics_api),
    };
    // Made under a name of its own, then put in place in one step, so
    // whoever connects meanwhile finds the last process's socket or this
    // one, never none; a socket left by a process gone is replaced.
    let path = runtime.path().join(SOCKET);
    let fresh = runtime
        .path()
        .join(format!("window-{}.sock", std::process::id()));
    match std::fs::remove_file(&fresh) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let listener = UnixListener::bind(&fresh)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o600))?;
    }
    let socket = std::fs::symlink_metadata(&fresh)?;
    std::fs::rename(&fresh, &path)?;
    let listening = Listening {
        path,
        id: (socket.dev(), socket.ino()),
    };
    std::thread::Builder::new()
        .name("windows".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let asked = stream.map_err(anyhow::Error::from).and_then(|stream| {
                    let at = Instant::now();
                    read(stream, &ours, at)
                });
                match asked {
                    Ok(Some(asked)) => deliver(asked),
                    Ok(None) => {}
                    Err(err) => log::info!("a window asked for wasn't taken on: {err:#}"),
                }
            }
        })?;
    Ok(listening)
}

/// What a request must match to be taken on.
#[derive(Debug)]
struct Expected {
    build: Build,
    env: Map<String, Value>,
    renderer: &'static str,
}

/// Reads what's asked for on `stream`, at `at`. Refuses it, returning
/// `None`, if a window opened here wouldn't be the one asked for.
fn read(stream: UnixStream, expected: &Expected, at: Instant) -> anyhow::Result<Option<Asked>> {
    ours(&stream).context("asked by another user's program")?;
    stream.set_read_timeout(Some(TAKE_WAIT))?;
    stream.set_write_timeout(Some(TAKE_WAIT))?;
    let mut line = String::new();
    BufReader::new((&stream).take(MOST)).read_line(&mut line)?;
    let Value::Object(request) = serde_json::from_str(&line)? else {
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
    let wait = request.get("wait_ms").and_then(Value::as_u64).unwrap_or(0);
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
    };
    Ok(Some(Asked {
        spec,
        launch: LaunchToken::from_raw(text("wayland_token"), text("x11_token")),
        at,
        deadline: at + Duration::from_millis(wait).min(TAKE_WAIT),
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
    let env = request.get("env").and_then(Value::as_object);
    let differs = (expected.env.iter())
        .find(|(name, value)| env.and_then(|env| env.get(*name)) != Some(value))
        .map(|(name, _)| format!("another {name}"));
    let renderer = request.get("renderer").and_then(Value::as_str);
    let differs = differs
        .or_else(|| (renderer != Some(expected.renderer)).then(|| "another renderer".into()));
    differs.map(|why| Refusal { why, keep: true })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    /// A runtime directory of its own, for a test.
    fn runtime() -> (tempfile::TempDir, RuntimeDir) {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimeDir::at(dir.path().join("clackatoo"));
        (dir, runtime)
    }

    /// A window on another machine, which is asked for whether or not a
    /// daemon runs here.
    fn remote() -> Spec {
        Spec {
            remote: Some(Remote {
                host: "desk".into(),
                command: "clackatoo".into(),
            }),
            remote_command: "clackatoo".into(),
            font: FontOptions {
                family: Some("Iosevka".into()),
                size: 13.5,
            },
            font_fixed: FontFixed {
                family: true,
                size: false,
            },
            ..Spec::default()
        }
    }

    fn launched() -> LaunchToken {
        LaunchToken::from_raw(Some("wayland-token".into()), Some("x11-token".into()))
    }

    /// Asks for `spec` from a thread, as another process would.
    fn ask_from_thread(
        runtime: &RuntimeDir,
        spec: Spec,
        graphics_api: GraphicsApi,
    ) -> std::thread::JoinHandle<Result<(), String>> {
        let runtime = runtime.clone();
        std::thread::spawn(move || {
            let request = Request {
                spec: &spec,
                graphics_api,
                runtime: &runtime,
            };
            ask(&request, launched()).map_err(|not| not.why)
        })
    }

    #[test]
    fn a_window_asked_for_arrives_whole_and_the_asker_hears_it_opened() {
        let (_dir, runtime) = runtime();
        let (sender, delivered) = mpsc::channel();
        let _listening = listen(&runtime, GraphicsApi::Vulkan, move |asked| {
            sender.send(asked).unwrap();
        })
        .unwrap();
        let waiting = ask_from_thread(&runtime, remote(), GraphicsApi::Vulkan);
        let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
        let spec = &asked.spec;
        assert_eq!(spec.remote, remote().remote);
        assert_eq!(spec.remote_command, "clackatoo");
        assert!(!spec.pick_remote);
        assert_eq!(spec.font.family.as_deref(), Some("Iosevka"));
        assert!((spec.font.size - 13.5).abs() < f32::EPSILON);
        assert!(spec.font_fixed.family && !spec.font_fixed.size);
        assert_eq!(asked.launch, launched());
        assert!(asked.take());
        asked.opened(Ok(()));
        assert_eq!(waiting.join().unwrap(), Ok(()));
    }

    #[test]
    fn a_window_that_failed_to_open_is_opened_by_the_asker_instead() {
        let (_dir, runtime) = runtime();
        let _listening = listen(&runtime, GraphicsApi::Vulkan, |mut asked| {
            assert!(asked.take());
            asked.opened(Err("no display".into()));
        })
        .unwrap();
        let waiting = ask_from_thread(&runtime, remote(), GraphicsApi::Vulkan);
        let why = waiting.join().unwrap().unwrap_err();
        assert!(why.contains("no display"), "{why}");
    }

    #[test]
    fn a_window_drawn_with_another_renderer_is_refused_before_it_reaches_the_windows() {
        let (_dir, runtime) = runtime();
        let _listening = listen(&runtime, GraphicsApi::Vulkan, |_| {
            panic!("a refused window was delivered");
        })
        .unwrap();
        let waiting = ask_from_thread(&runtime, remote(), GraphicsApi::OpenGl);
        let why = waiting.join().unwrap().unwrap_err();
        assert!(why.contains("another renderer"), "{why}");
        // The asker is the odd one out: the process asked keeps taking
        // windows.
        assert!(why.contains("takes the windows asked for after"), "{why}");
    }

    #[test]
    fn a_window_taken_on_too_late_is_left_to_the_asker() {
        let (_dir, runtime) = runtime();
        let (sender, delivered) = mpsc::channel();
        let _listening = listen(&runtime, GraphicsApi::Vulkan, move |asked| {
            sender.send(asked).unwrap();
        })
        .unwrap();
        let waiting = ask_from_thread(&runtime, remote(), GraphicsApi::Vulkan);
        let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
        // The event loop is busy for longer than the asker waits.
        let why = waiting.join().unwrap().unwrap_err();
        assert!(why.contains("in time"), "{why}");
        assert!(!asked.take());
    }

    #[test]
    fn with_no_process_listening_the_asker_opens_the_window() {
        let (_dir, runtime) = runtime();
        let waiting = ask_from_thread(&runtime, remote(), GraphicsApi::Vulkan);
        let why = waiting.join().unwrap().unwrap_err();
        assert!(why.contains("no process"), "{why}");
        // A local window isn't asked for while no daemon runs: one would be
        // started with another process's environment.
        let waiting = ask_from_thread(&runtime, Spec::default(), GraphicsApi::Vulkan);
        let why = waiting.join().unwrap().unwrap_err();
        assert!(why.contains("daemon"), "{why}");
    }

    #[test]
    fn requests_from_another_build_or_environment_are_refused() {
        let expected = Expected {
            build: Build("1:2:3:4.000000005".into()),
            env: window_env(),
            renderer: "vulkan",
        };
        let request = |build: &str, env: Map<String, Value>| {
            let Value::Object(request) = json!({
                "build": build,
                "env": env,
                "renderer": "vulkan",
            }) else {
                unreachable!("an object")
            };
            request
        };
        let refused = |why: &str, keep| {
            Some(Refusal {
                why: why.into(),
                keep,
            })
        };
        let same = request("1:2:3:4.000000005", window_env());
        assert_eq!(refusal(&same, &expected), None);
        // A newer build takes the windows after it; an older one doesn't.
        let newer = request("1:7:3:9.000000005", window_env());
        assert_eq!(
            refusal(&newer, &expected),
            refused("another build of clackatoo", false)
        );
        let older = request("1:7:3:4.000000001", window_env());
        assert_eq!(
            refusal(&older, &expected),
            refused("another build of clackatoo", true)
        );
        let mut env = window_env();
        env.insert("WAYLAND_DISPLAY".into(), "/run/user/1000/wayland-9".into());
        let other_display = request("1:2:3:4.000000005", env);
        assert_eq!(
            refusal(&other_display, &expected),
            refused("another WAYLAND_DISPLAY", true)
        );
        // A build that sent no environment at all.
        let mut bare = same.clone();
        bare.remove("env");
        assert!(refusal(&bare, &expected).is_some());
    }

    #[test]
    fn a_build_is_its_file_and_one_put_in_its_place_is_another() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("clackatoo");
        std::fs::write(&program, "one").unwrap();
        let before = Build::of(&program).unwrap();
        assert_eq!(Build::of(&program), Some(before.clone()));
        // As `clackatoo update` and a deploy put a build in place.
        let next = dir.path().join("clackatoo.new");
        std::fs::write(&next, "two").unwrap();
        std::fs::rename(&next, &program).unwrap();
        assert_ne!(Build::of(&program), Some(before));
        assert!(Build::running().is_some());
    }

    #[test]
    fn a_process_that_stops_listening_leaves_a_newer_ones_socket_alone() {
        let (_dir, runtime) = runtime();
        let older = listen(&runtime, GraphicsApi::Vulkan, |_| {}).unwrap();
        let newer = listen(&runtime, GraphicsApi::Vulkan, |_| {}).unwrap();
        let socket = runtime.path().join(SOCKET);
        drop(older);
        assert!(socket.exists());
        drop(newer);
        assert!(!socket.exists());
    }
}
