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

/// Asks for `spec` from a thread, as another process would: of the
/// process on `window.sock`, or with `started`, of the one at the other
/// end of that socket pair.
fn ask_from_thread(
    runtime: &RuntimeDir,
    spec: Spec,
    graphics_api: GraphicsApi,
    started: Option<UnixStream>,
) -> std::thread::JoinHandle<Result<(), (String, Then)>> {
    let runtime = runtime.clone();
    std::thread::spawn(move || {
        let request = Request {
            spec: &spec,
            graphics_api,
            runtime: &runtime,
            waits: short(),
        };
        ask(&request, launched(), started).map_err(|not| (not.why, not.then))
    })
}

fn ask_shared(
    runtime: &RuntimeDir,
    spec: Spec,
) -> std::thread::JoinHandle<Result<(), (String, Then)>> {
    ask_from_thread(runtime, spec, GraphicsApi::Vulkan, None)
}

/// Listens on the shared socket, handing what's asked for to a channel.
fn listening(runtime: &RuntimeDir) -> (Listening, mpsc::Receiver<Asked>) {
    let (sender, delivered) = mpsc::channel();
    let sender = std::sync::Mutex::new(sender);
    let listening = listen(runtime, GraphicsApi::Vulkan, move |asked| {
        sender.lock().unwrap().send(asked).unwrap();
    })
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
fn a_window_asked_of_the_process_just_forked_goes_to_it_and_not_to_the_socket() {
    let (_dir, runtime) = runtime();
    // An older process on the socket, which would refuse it.
    let _listening = listen(&runtime, GraphicsApi::OpenGl, |_| {
        panic!("the window went to the process on the socket");
    })
    .unwrap();
    let (ours, theirs) = UnixStream::pair().unwrap();
    let waiting = ask_from_thread(&runtime, remote(), GraphicsApi::Vulkan, Some(ours));
    // As the process forked reads it, first thing.
    let mut asked = first(theirs, GraphicsApi::Vulkan).unwrap();
    assert_eq!(asked.spec.remote, remote().remote);
    assert_eq!(asked.launch, launched());
    assert!(asked.take());
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
    let (why, then) = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("no display"), "{why}");
    assert_eq!(then, Then::Host);
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
    let (why, _) = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("didn't open in time"), "{why}");
    assert!(asked.late());
}

#[test]
fn a_window_drawn_with_another_renderer_is_refused_before_it_reaches_the_windows() {
    let (_dir, runtime) = runtime();
    let _listening = listen(&runtime, GraphicsApi::Vulkan, |_| {
        panic!("a refused window was delivered");
    })
    .unwrap();
    let waiting = ask_from_thread(&runtime, remote(), GraphicsApi::OpenGl, None);
    let (why, then) = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("another renderer"), "{why}");
    // The asker is the odd one out: the process asked keeps taking
    // windows.
    assert_eq!(then, Then::Alone);
}

#[test]
fn a_window_taken_on_too_late_is_left_to_the_asker() {
    let (_dir, runtime) = runtime();
    let (_listening, delivered) = listening(&runtime);
    let waiting = ask_shared(&runtime, remote());
    let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
    // The event loop is busy for longer than the asker waits.
    let (why, then) = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("in time"), "{why}");
    assert_eq!(then, Then::Host);
    assert!(!asked.take());
}

#[test]
fn a_window_waiting_behind_a_slow_asker_is_still_taken_in_time() {
    let (_dir, runtime) = runtime();
    let (_listening, delivered) = listening(&runtime);
    // Connects, and says half a request, then nothing.
    let mut slow = UnixStream::connect(runtime.path().join(SOCKET)).unwrap();
    let slow_since = Instant::now();
    slow.write_all(b"{\"build\":").unwrap();
    let waiting = ask_shared(&runtime, remote());
    let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
    // Taken while the slow one still has time to finish.
    assert!(
        slow_since.elapsed() < READ_WAIT,
        "{:?}",
        slow_since.elapsed()
    );
    assert!(asked.take());
    asked.opened(Ok(()));
    assert_eq!(waiting.join().unwrap(), Ok(()));
    // The slow one is hung up on once its time is up, unanswered, and
    // never delivered.
    let mut answer = Vec::new();
    slow.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    slow.read_to_end(&mut answer).unwrap();
    let hung_up = slow_since.elapsed();
    assert!(answer.is_empty(), "{answer:?}");
    assert!(
        hung_up >= READ_WAIT && hung_up < READ_WAIT + Duration::from_secs(2),
        "{hung_up:?}"
    );
    assert!(delivered.try_recv().is_err());
}

#[test]
fn with_no_process_listening_or_one_gone_a_process_is_forked() {
    let (_dir, runtime) = runtime();
    let waiting = ask_shared(&runtime, remote());
    let (why, then) = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("no process of windows"), "{why}");
    assert_eq!(then, Then::Host);
    // A socket left by a process that ended without taking it away.
    std::fs::create_dir_all(runtime.path()).unwrap();
    drop(UnixListener::bind(runtime.path().join(SOCKET)).unwrap());
    let waiting = ask_shared(&runtime, remote());
    let (why, then) = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("no process of windows"), "{why}");
    assert_eq!(then, Then::Host);
}

#[test]
fn a_request_too_big_to_be_read_opens_the_window_here() {
    let (_dir, runtime) = runtime();
    let (_listening, delivered) = listening(&runtime);
    let mut huge = remote();
    huge.origin.env = Some(vec![("HUGE".into(), "x".repeat(MOST).into())].into());
    let waiting = ask_shared(&runtime, huge);
    let (why, then) = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("too big"), "{why}");
    // Nor is a process forked for it, which couldn't read it either.
    assert_eq!(then, Then::Here);
    assert!(delivered.try_recv().is_err());
}

#[test]
fn requests_from_another_build_display_or_locale_are_refused_but_not_another_path() {
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
            "env": [["PATH", "/somewhere/else"], ["VIRTUAL_ENV", "/srv/venv"]],
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
    // A family of variables the process has none of, as the asker sets
    // one for itself.
    let mut matched = window_match();
    matched.insert("LC_TESTING_ONLY".into(), "fr_FR.UTF-8".into());
    let other_locale = request("1:2:3:4.000000005", matched);
    assert_eq!(
        refusal(&other_locale, &expected),
        refused("another LC_TESTING_ONLY", true)
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
    let older = listen(&runtime, GraphicsApi::Vulkan, |_| {}).unwrap();
    let newer = listen(&runtime, GraphicsApi::Vulkan, |_| {}).unwrap();
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
fn deadlines_are_on_the_clock_every_process_shares() {
    // The same clock as `Instant`'s, which is CLOCK_MONOTONIC too: what
    // another process measures with it, this one can compare.
    let started = Instant::now();
    let deadline = after(Duration::from_millis(50));
    let left = until(deadline);
    assert!(
        left <= Duration::from_millis(50) && left > Duration::from_millis(30),
        "{left:?}"
    );
    std::thread::sleep(Duration::from_millis(60));
    assert!(until(deadline).is_zero());
    let waited = Duration::from_nanos(now() - deadline) + Duration::from_millis(50);
    let elapsed = started.elapsed();
    assert!(
        waited.abs_diff(elapsed) < Duration::from_millis(5),
        "{waited:?} {elapsed:?}"
    );
}
