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
        let to = match started {
            Some(stream) => To::Started(stream),
            None => To::Socket(shared_socket(&runtime)),
        };
        ask(&request, launched(), to).map_err(|not| (not.why, not.then))
    })
}

fn ask_shared(
    runtime: &RuntimeDir,
    spec: Spec,
) -> std::thread::JoinHandle<Result<(), (String, Then)>> {
    ask_from_thread(runtime, spec, GraphicsApi::Vulkan, None)
}

/// Listens on the shared socket, handing what's asked for to a channel,
/// the process `starting` or not.
fn listening_while(runtime: &RuntimeDir, starting: bool) -> (Listening, mpsc::Receiver<Asked>) {
    let (sender, delivered) = mpsc::channel();
    let sender = std::sync::Mutex::new(sender);
    let starting = Arc::new(std::sync::atomic::AtomicBool::new(starting));
    let listening = listen(
        runtime,
        shared_socket(runtime),
        GraphicsApi::Vulkan,
        starting,
        move |asked| sender.lock().unwrap().send(asked).unwrap(),
    )
    .unwrap();
    (listening, delivered)
}

fn listening(runtime: &RuntimeDir) -> (Listening, mpsc::Receiver<Asked>) {
    listening_while(runtime, false)
}

/// Listens on the shared socket as a process started, drawing with
/// `graphics_api`, with `deliver`.
fn listen_with(
    runtime: &RuntimeDir,
    graphics_api: GraphicsApi,
    deliver: impl Fn(Asked) + Send + Sync + 'static,
) -> Listening {
    let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    listen(
        runtime,
        shared_socket(runtime),
        graphics_api,
        started,
        deliver,
    )
    .unwrap()
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
    let _listening = listen_with(&runtime, GraphicsApi::OpenGl, |_| {
        panic!("the window went to the process on the socket");
    });
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
    assert_eq!(then, Then::Host(Some(shared_socket(&runtime))));
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
    let _listening = listen_with(&runtime, GraphicsApi::Vulkan, |_| {
        panic!("a refused window was delivered");
    });
    let waiting = ask_from_thread(&runtime, remote(), GraphicsApi::OpenGl, None);
    let (why, then) = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("another renderer"), "{why}");
    // The asker is the odd one out: the process asked keeps taking
    // windows, and the asker goes to its group's.
    assert_eq!(then, Then::Group);
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
    assert_eq!(then, Then::Host(Some(shared_socket(&runtime))));
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
    assert_eq!(then, Then::Host(Some(shared_socket(&runtime))));
    // A socket left by a process that ended without taking it away.
    std::fs::create_dir_all(runtime.path()).unwrap();
    drop(UnixListener::bind(runtime.path().join(SOCKET)).unwrap());
    let waiting = ask_shared(&runtime, remote());
    let (why, then) = waiting.join().unwrap().unwrap_err();
    assert!(why.contains("no process of windows"), "{why}");
    assert_eq!(then, Then::Host(Some(shared_socket(&runtime))));
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
    let older = listen_with(&runtime, GraphicsApi::Vulkan, |_| {});
    let newer = listen_with(&runtime, GraphicsApi::Vulkan, |_| {});
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

#[test]
fn a_window_asked_of_a_process_still_starting_is_taken_at_once_and_opens_after() {
    let (_dir, runtime) = runtime();
    let (_listening, delivered) = listening_while(&runtime, true);
    let waiting = ask_shared(&runtime, remote());
    let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
    // The event loop is busy setting up the GPU for longer than the asker
    // waits to hear it's taken: it heard already.
    std::thread::sleep(short().take + SLACK);
    assert!(asked.take());
    asked.opened(Ok(()));
    assert_eq!(waiting.join().unwrap(), Ok(()));
}

#[test]
fn windows_alike_share_a_group_socket_named_after_what_they_share() {
    let (_dir, runtime) = runtime();
    let group = group_socket(&runtime, GraphicsApi::Vulkan);
    assert_eq!(group, group_socket(&runtime, GraphicsApi::Vulkan));
    assert_ne!(group, group_socket(&runtime, GraphicsApi::OpenGl));
    assert_ne!(group, shared_socket(&runtime));
    let name = group.file_stem().unwrap().to_str().unwrap();
    assert!(name.starts_with("window-"), "{name}");
    assert_eq!(group.extension().unwrap(), "sock");
    // Refused by its group's process for what it sees, a window gets a
    // process of its own, taking no others.
    let _listening = listen(
        &runtime,
        group.clone(),
        GraphicsApi::Vulkan,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        |_| panic!("a refused window was delivered"),
    )
    .unwrap();
    let runtime_for = runtime.clone();
    let waiting = std::thread::spawn(move || {
        let spec = remote();
        let request = Request {
            spec: &spec,
            graphics_api: GraphicsApi::OpenGl,
            runtime: &runtime_for,
            waits: short(),
        };
        ask(&request, launched(), To::Socket(group)).map_err(|not| not.then)
    });
    assert_eq!(waiting.join().unwrap(), Err(Then::Host(None)));
}

#[test]
fn only_the_locale_variables_that_change_a_window_are_matched() {
    for name in window_match().keys() {
        assert!(
            !name.starts_with("LC_") || MATCH_ENV.contains(&name.as_str()),
            "{name} is matched"
        );
    }
    assert!(MATCH_ENV.contains(&"LC_CTYPE") && !MATCH_ENV.contains(&"LC_TERMINAL"));
}

/// `clackatoo open` hands its window to a process of windows running, and
/// gets the tokens back, to start a window some other way, when none is:
/// never forking one, nor opening the window itself.
#[test]
fn a_window_handed_over_goes_to_the_process_running_or_comes_back() {
    let (_dir, runtime) = runtime();
    let options = crate::Options {
        runtime: runtime.clone(),
        remote: None,
        pick_remote: false,
        remote_command: "clackatoo".into(),
        font: FontOptions::default(),
        font_fixed: FontFixed::default(),
        graphics_api: GraphicsApi::Vulkan,
        config: None,
    };
    assert_eq!(crate::hand_window(&options, launched()), Err(launched()));
    let (_listening, delivered) = listening(&runtime);
    let handing = std::thread::spawn(move || crate::hand_window(&options, launched()));
    let mut asked = delivered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(asked.launch, launched());
    assert!(asked.take());
    asked.opened(Ok(()));
    assert_eq!(handing.join().unwrap(), Ok(()));
}
