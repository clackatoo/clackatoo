use std::sync::mpsc;

use super::*;

/// A runtime directory of its own, for a test.
fn runtime() -> (tempfile::TempDir, RuntimeDir) {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimeDir::at(dir.path().join("clackatoo"));
    (dir, runtime)
}

/// A window on another machine, from somewhere with an environment of its
/// own.
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
        origin: Origin {
            env: Some(vec![("PATH".into(), "/venv/bin:/usr/bin".into())].into()),
            dir: Some("/srv/project".into()),
        },
        ..Spec::default()
    }
}

fn launched() -> LaunchToken {
    LaunchToken::from_raw(Some("wayland-token".into()), Some("x11-token".into()))
}

/// Short waits, so tests of them don't take long.
fn short() -> Waits {
    Waits {
        take: Duration::from_millis(300),
        start: Duration::from_secs(5),
        open: Duration::from_millis(400),
    }
}

/// Asks for `spec` from a thread, as another process would, `at` the
/// socket, waiting as `waits` says.
fn ask_from_thread(
    runtime: &RuntimeDir,
    spec: Spec,
    graphics_api: GraphicsApi,
    at: Socket,
    waits: Waits,
) -> std::thread::JoinHandle<Result<(), String>> {
    let runtime = runtime.clone();
    std::thread::spawn(move || {
        let request = Request {
            spec: &spec,
            graphics_api,
            runtime: &runtime,
            waits,
        };
        ask(&request, launched(), &at, None).map_err(|not| not.why)
    })
}

fn ask_shared(runtime: &RuntimeDir, spec: Spec) -> std::thread::JoinHandle<Result<(), String>> {
    ask_from_thread(runtime, spec, GraphicsApi::Vulkan, Socket::Shared, short())
}

/// Listens on the shared socket, handing what's asked for to a channel.
fn listening(runtime: &RuntimeDir) -> (Listening, mpsc::Receiver<Asked>) {
    let (sender, delivered) = mpsc::channel();
    let sender = std::sync::Mutex::new(sender);
    let listening = listen(
        runtime,
        &Socket::Shared,
        GraphicsApi::Vulkan,
        move |asked| {
            sender.lock().unwrap().send(asked).unwrap();
        },
    )
    .unwrap();
    (listening, delivered)
}

#[test]
fn a_window_asked_for_arrives_whole_and_the_asker_hears_it_opened() {
    let (_dir, runtime) = runtime();
    let (_listening, delivered) = listening(&runtime);
    let waiting = ask_shared(&runtime, remote());
    let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
    let spec = &asked.spec;
    assert_eq!(spec.remote, remote().remote);
    assert_eq!(spec.remote_command, "clackatoo");
    assert!(!spec.pick_remote);
    assert_eq!(spec.font.family.as_deref(), Some("Iosevka"));
    assert!((spec.font.size - 13.5).abs() < f32::EPSILON);
    assert!(spec.font_fixed.family && !spec.font_fixed.size);
    // What the window starts is started as the asker would.
    assert_eq!(spec.origin.var("PATH").unwrap(), "/venv/bin:/usr/bin");
    assert_eq!(spec.origin.var("HOME"), None);
    assert_eq!(spec.origin.dir.as_deref(), Some(Path::new("/srv/project")));
    assert_eq!(asked.launch, launched());
    assert!(asked.take());
    assert!(!asked.late());
    asked.opened(Ok(()));
    assert_eq!(waiting.join().unwrap(), Ok(()));
}

#[test]
fn a_window_that_failed_to_open_is_opened_by_the_asker_instead() {
    let (_dir, runtime) = runtime();
    let (_listening, delivered) = listening(&runtime);
    let waiting = ask_shared(&runtime, remote());
    let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(asked.take());
    asked.opened(Err("no display".into()));
    let why = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("no display"), "{why}");
}

#[test]
fn a_window_taken_on_but_not_opened_in_time_is_the_askers_and_closes_there() {
    let (_dir, runtime) = runtime();
    let (_listening, delivered) = listening(&runtime);
    let waiting = ask_shared(&runtime, remote());
    let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(asked.take());
    // Opening takes longer than the asker waits: it opens its own, and the
    // one opened here is late, so it's closed again.
    let why = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("didn't open in time"), "{why}");
    assert!(asked.late());
}

#[test]
fn a_window_drawn_with_another_renderer_is_refused_before_it_reaches_the_windows() {
    let (_dir, runtime) = runtime();
    let _listening = listen(&runtime, &Socket::Shared, GraphicsApi::Vulkan, |_| {
        panic!("a refused window was delivered");
    })
    .unwrap();
    let waiting = ask_from_thread(
        &runtime,
        remote(),
        GraphicsApi::OpenGl,
        Socket::Shared,
        short(),
    );
    let why = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("another renderer"), "{why}");
    // The asker is the odd one out: the process asked keeps taking
    // windows.
    assert!(why.contains("takes the windows asked for after"), "{why}");
}

#[test]
fn a_window_taken_on_too_late_is_left_to_the_asker() {
    let (_dir, runtime) = runtime();
    let (_listening, delivered) = listening(&runtime);
    let waiting = ask_shared(&runtime, remote());
    let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
    // The event loop is busy for longer than the asker waits.
    let why = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("in time"), "{why}");
    assert!(!asked.take());
}

#[test]
fn a_window_waiting_behind_a_slow_asker_is_still_taken_in_time() {
    let (_dir, runtime) = runtime();
    let (_listening, delivered) = listening(&runtime);
    // Connects, and says half a request, then nothing.
    let mut slow = UnixStream::connect(runtime.path().join(SOCKET)).unwrap();
    slow.write_all(b"{\"build\":").unwrap();
    let waiting = ask_shared(&runtime, remote());
    let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(asked.take());
    asked.opened(Ok(()));
    assert_eq!(waiting.join().unwrap(), Ok(()));
    // The slow one is given up on, whole, within the time a request has.
    let mut answer = Vec::new();
    slow.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = slow.read_to_end(&mut answer);
    assert!(answer.is_empty(), "{answer:?}");
    assert!(delivered.try_recv().is_err());
}

#[test]
fn with_no_process_listening_the_window_isnt_taken() {
    let (_dir, runtime) = runtime();
    let waiting = ask_shared(&runtime, remote());
    let why = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("no process of windows"), "{why}");
    // Nor a local window, now that the daemon is started as its asker
    // would.
    let waiting = ask_shared(&runtime, Spec::default());
    let why = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("no process of windows"), "{why}");
}

#[test]
fn a_private_socket_takes_only_the_window_it_was_made_for() {
    let (_dir, runtime) = runtime();
    let private = Socket::private(&runtime);
    let (sender, delivered) = mpsc::channel();
    let sender = std::sync::Mutex::new(sender);
    let listening = listen(&runtime, &private, GraphicsApi::Vulkan, move |asked| {
        sender.lock().unwrap().send(asked).unwrap();
    })
    .unwrap();
    assert!(!runtime.path().join(SOCKET).exists());
    let waiting = ask_from_thread(
        &runtime,
        remote(),
        GraphicsApi::Vulkan,
        private.clone(),
        short(),
    );
    let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(asked.take());
    asked.opened(Ok(()));
    assert_eq!(waiting.join().unwrap(), Ok(()));
    drop(listening);
    assert!(!private.path(&runtime).exists());
}

#[test]
fn requests_from_another_build_or_display_are_refused_but_not_another_environment() {
    let expected = Expected {
        build: Build("1:2:3:4.000000005".into()),
        matched: window_match(),
        renderer: "vulkan",
    };
    let request = |build: &str, matched: Map<String, Value>| {
        let Value::Object(request) = json!({
            "build": build,
            "match": matched,
            "renderer": "vulkan",
            "env": [["PATH", "/somewhere/else"], ["LANG", "fr_FR.UTF-8"]],
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
    let same = request("1:2:3:4.000000005", window_match());
    assert_eq!(refusal(&same, &expected), None);
    // A newer build takes the windows after it; an older one doesn't.
    let newer = request("1:7:3:9.000000005", window_match());
    assert_eq!(
        refusal(&newer, &expected),
        refused("another build of clackatoo", false)
    );
    let older = request("1:7:3:4.000000001", window_match());
    assert_eq!(
        refusal(&older, &expected),
        refused("another build of clackatoo", true)
    );
    let mut matched = window_match();
    matched.insert("WAYLAND_DISPLAY".into(), "/run/user/1000/wayland-9".into());
    let other_display = request("1:2:3:4.000000005", matched);
    assert_eq!(
        refusal(&other_display, &expected),
        refused("another WAYLAND_DISPLAY", true)
    );
    let mut matched = window_match();
    matched.insert("config".into(), "/elsewhere/clackatoo".into());
    let other_config = request("1:2:3:4.000000005", matched);
    assert_eq!(
        refusal(&other_config, &expected),
        refused("another config", true)
    );
    // A build that sent nothing to match.
    let mut bare = same.clone();
    bare.remove("match");
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
    let older = listen(&runtime, &Socket::Shared, GraphicsApi::Vulkan, |_| {}).unwrap();
    let newer = listen(&runtime, &Socket::Shared, GraphicsApi::Vulkan, |_| {}).unwrap();
    let socket = runtime.path().join(SOCKET);
    drop(older);
    assert!(socket.exists());
    drop(newer);
    assert!(!socket.exists());
    // Nor is the name it was made under left behind.
    let left: Vec<_> = std::fs::read_dir(runtime.path()).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn the_clock_deadlines_are_on_is_one_for_every_process() {
    let before = now();
    std::thread::sleep(Duration::from_millis(5));
    let after = now();
    assert!(after - before >= 5_000_000, "{before} {after}");
    assert!(until(before).is_zero());
}
