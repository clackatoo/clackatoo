use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;

use clackatoo_daemon::Client;
use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{ClientMessage, PROTOCOL_VERSION, ServerMessage};

use super::*;

/// A process of windows, woken through a channel, sharing as `share` says.
fn host(share: bool) -> (Host, mpsc::Receiver<Wake>) {
    let (sender, woken) = mpsc::channel();
    let process = Process {
        runtime: RuntimeDir::at(PathBuf::from("/unused-host-test")),
        config: None,
        graphics_api: GraphicsApi::Vulkan,
        listen: false,
        share,
    };
    (Host::new(process, Waker::Channel(sender)), woken)
}

/// Adds a window that connects nowhere, as a picker of a machine does until
/// one is picked, never to open: nothing here has a display.
fn window(host: &mut Host) -> WindowKey {
    let startup = Startup {
        fonts: Pending::Thread(std::thread::spawn(|| Err(anyhow!("never opened")))),
        gpu: Pending::Thread(std::thread::spawn(|| Err(anyhow!("never opened")))),
        launch: LaunchToken::default(),
    };
    let spec = Spec {
        pick_remote: true,
        ..Spec::default()
    };
    let start = options::Options::drawing_with(&spec.font);
    host.add(spec, start, Instant::now(), startup)
}

fn app() -> App {
    let runtime = RuntimeDir::at(PathBuf::from("/unused-host-test"));
    let session = Session::waiting(&runtime, Proxy::Channel(mpsc::channel().0));
    App::new(
        Instant::now(),
        None,
        session,
        Spec::default(),
        Link::default(),
    )
}

#[test]
fn a_window_that_panics_is_closed_saying_why_and_one_that_asks_is_closed() {
    let mut app = app();
    assert_eq!(guarded(&mut app, |_| {}), Handled::Open);
    let panicked = guarded(&mut app, |_| panic!("a bug in one window"));
    assert_eq!(panicked, Handled::Panicked("a bug in one window".into()));
    let panicked = guarded(&mut app, |_| panic!("{} bugs", 2));
    assert_eq!(panicked, Handled::Panicked("2 bugs".into()));
    assert_eq!(
        guarded(&mut app, |app| app.closing = true),
        Handled::Closing
    );
}

#[test]
fn a_window_that_panics_closes_and_the_others_stay_open() {
    let (mut host, _woken) = host(true);
    let failing = window(&mut host);
    let staying = window(&mut host);
    let closed = host.on_window(failing, |_| panic!("a bug in one window"));
    let why = closed.unwrap().unwrap_err().to_string();
    assert!(why.contains("a bug in one window"), "{why}");
    assert!(!host.windows.contains_key(&failing));
    assert!(host.windows.contains_key(&staying));
    assert!(!host.ending);
    // The window left goes on handling what comes.
    assert!(
        host.on_window(staying, |app| app.woke(Instant::now()))
            .is_none()
    );
    // Closing the last ends the process.
    assert!(host.on_window(staying, |app| app.closing = true).is_some());
    assert!(host.ending);
}

/// A daemon that only greets, and says when its client hangs up.
fn greeter(dir: &Path) -> (PathBuf, mpsc::Receiver<()>) {
    let path = dir.join("daemon.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (hung_up, heard) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0; 4096];
        loop {
            let n = stream.read(&mut buffer).unwrap();
            assert!(n > 0, "hung up before greeting");
            decoder.push(&buffer[..n]);
            if let Some(ClientMessage::Hello { .. }) = decoder.decode(PROTOCOL_VERSION).unwrap() {
                break;
            }
        }
        let hello = ServerMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        };
        stream
            .write_all(&frame::encode(PROTOCOL_VERSION, &hello).unwrap())
            .unwrap();
        // Until the client hangs up.
        while stream.read(&mut buffer).is_ok_and(|n| n > 0) {}
        let _ = hung_up.send(());
    });
    (path, heard)
}

#[test]
fn a_connection_made_for_a_window_already_closed_is_hung_up() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, heard) = greeter(dir.path());
    let client = Client::over(UnixStream::connect(socket).unwrap()).unwrap();
    let wire = client.wire();
    let (sender, _receiver) = client.split().unwrap();
    let (mut host, _woken) = host(true);
    let closed = window(&mut host);
    host.on_window(closed, |app| app.closing = true);
    // The session thread connected just as the window closed.
    let connected = SessionEvent::Connected {
        sender,
        workspaces: clackatoo_protocol::Workspaces::default(),
        contact: None,
        daemon_version: None,
        wire,
        forwarder: None,
        connections: Vec::new(),
    };
    assert!(host.woken(Wake::Window(closed, connected)).is_none());
    heard
        .recv_timeout(Duration::from_secs(5))
        .expect("the daemon heard the client hang up");
}

#[test]
fn with_windows_kept_apart_a_window_opens_others_in_processes_of_their_own() {
    let (mut kept_apart, _woken) = host(false);
    let key = window(&mut kept_apart);
    let link = &kept_apart.windows[&key].link;
    assert!(!link.hosting());
    assert!(!link.replaced(Path::new("/proc/self/exe")));
    let (mut sharing, _woken) = host(true);
    let key = window(&mut sharing);
    let link = &sharing.windows[&key].link;
    assert!(link.hosting());
    assert_eq!(link.renderer(), Some(GraphicsApi::Vulkan));
    assert!(!link.replaced(Path::new("/proc/self/exe")));
    assert!(link.replaced(Path::new("/nonexistent/clackatoo")));
    // Once the GPU is lost, they open elsewhere too.
    sharing.link.0.lost.set(true);
    assert!(!sharing.windows[&key].link.hosting());
}
