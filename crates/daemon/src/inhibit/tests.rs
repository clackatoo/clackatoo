//! The thread against a fake logind: a bus on a socket of its own, which
//! logs the daemon in, answers its calls as logind would, passing a pipe's
//! end as the lock, and sends logind's signals.

use std::io::{IoSlice, Read as _, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Instant;

use super::*;
use crate::dbus;

/// logind's unique name on the fake bus, and another connection's.
const LOGIND: &str = ":1.1";
const OTHER: &str = ":1.9";

/// The fake bus's end of the daemon's connection.
struct FakeBus {
    stream: UnixStream,
    serial: u32,
}

/// A line of the login, `\r\n` and all.
fn line(stream: &mut UnixStream) -> String {
    let mut said = Vec::new();
    let mut byte = [0];
    while !said.ends_with(b"\r\n") {
        stream.read_exact(&mut byte).unwrap();
        said.push(byte[0]);
    }
    String::from_utf8(said).unwrap()
}

/// A header field: its code, a variant's signature and its value, as
/// little-endian bytes, aligned from the header's start.
fn field(fields: &mut Vec<u8>, code: u8, signature: u8, value: &[u8]) {
    while !fields.len().is_multiple_of(8) {
        fields.push(0);
    }
    fields.extend_from_slice(&[code, 1, signature, 0]);
    fields.extend_from_slice(value);
}

/// A string as D-Bus writes one, at a place aligned to 4.
fn string(text: &str) -> Vec<u8> {
    let mut bytes = u32::try_from(text.len()).unwrap().to_le_bytes().to_vec();
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(0);
    bytes
}

impl FakeBus {
    /// Takes the daemon's connection on `listener` and logs it in, passing
    /// file descriptors, as the system bus does.
    fn accept(listener: &UnixListener) -> Self {
        let (mut stream, _) = listener.accept().unwrap();
        assert!(line(&mut stream).starts_with("\0AUTH EXTERNAL "));
        stream.write_all(b"OK 0123456789abcdef\r\n").unwrap();
        assert_eq!(line(&mut stream), "NEGOTIATE_UNIX_FD\r\n");
        stream.write_all(b"AGREE_UNIX_FD\r\n").unwrap();
        assert_eq!(line(&mut stream), "BEGIN\r\n");
        Self { stream, serial: 0 }
    }

    /// The daemon's next call.
    fn call(&mut self) -> Message {
        let message = dbus::read_message(&mut self.stream).unwrap().unwrap();
        assert!(message.is(Kind::Call));
        message
    }

    /// Sends what `what` says from `sender`, with `body` of `signature`,
    /// and `fd` passed if given.
    fn send(
        &mut self,
        sender: &str,
        what: Sent<'_>,
        (signature, body): (&str, &[u8]),
        fd: Option<&OwnedFd>,
    ) {
        self.serial += 1;
        let mut fields = Vec::new();
        let kind = match what {
            Sent::Reply(serial) => {
                field(&mut fields, 5, b'u', &serial.to_le_bytes());
                Kind::Reply as u8
            }
            Sent::Error(serial, name) => {
                field(&mut fields, 4, b's', &string(name));
                field(&mut fields, 5, b'u', &serial.to_le_bytes());
                3
            }
            Sent::Signal(member) => {
                field(&mut fields, 1, b'o', &string(LOGIN_PATH));
                field(&mut fields, 2, b's', &string(MANAGER));
                field(&mut fields, 3, b's', &string(member));
                Kind::Signal as u8
            }
            Sent::Bus(member) => {
                field(&mut fields, 1, b'o', &string("/org/freedesktop/DBus"));
                field(&mut fields, 2, b's', &string(BUS));
                field(&mut fields, 3, b's', &string(member));
                Kind::Signal as u8
            }
        };
        field(&mut fields, 7, b's', &string(sender));
        let mut value = vec![u8::try_from(signature.len()).unwrap()];
        value.extend_from_slice(signature.as_bytes());
        value.push(0);
        field(&mut fields, 8, b'g', &value);
        if fd.is_some() {
            field(&mut fields, 9, b'u', &1u32.to_le_bytes());
        }
        let mut message = vec![b'l', kind, 0, 1];
        message.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        message.extend_from_slice(&self.serial.to_le_bytes());
        message.extend_from_slice(&u32::try_from(fields.len()).unwrap().to_le_bytes());
        message.extend_from_slice(&fields);
        while !message.len().is_multiple_of(8) {
            message.push(0);
        }
        message.extend_from_slice(body);
        let fds: Vec<_> = fd.map(OwnedFd::as_fd).into_iter().collect();
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = rustix::net::SendAncillaryBuffer::new(&mut space);
        if !fds.is_empty() {
            assert!(control.push(rustix::net::SendAncillaryMessage::ScmRights(&fds)));
        }
        rustix::net::sendmsg(
            &self.stream,
            &[IoSlice::new(&message)],
            &mut control,
            rustix::net::SendFlags::empty(),
        )
        .unwrap();
    }

    /// Answers `call` with the string `text`.
    fn answer(&mut self, call: &Message, text: &str) {
        let reply = Sent::Reply(call.serial);
        self.send("org.freedesktop.DBus", reply, ("s", &string(text)), None);
    }

    /// Takes the daemon from logging in to asking for the lock, answering
    /// as the bus and logind would: returns the call asking for it.
    fn up_to_the_lock(&mut self) -> Message {
        let owner = self.up_to_the_name();
        self.answer(&owner, LOGIND);
        let inhibit = self.call();
        assert_eq!(inhibit.member.as_deref(), Some("Inhibit"));
        assert_eq!(inhibit.body, inhibit_body().as_bytes());
        inhibit
    }

    /// Takes the daemon from logging in to asking for logind's name, which
    /// it asks only once it hears of a new one: returns that call.
    fn up_to_the_name(&mut self) -> Message {
        let hello = self.call();
        assert_eq!(hello.member.as_deref(), Some("Hello"));
        self.answer(&hello, ":1.5");
        let first = self.call();
        assert_eq!(first.member.as_deref(), Some("AddMatch"));
        let rule = first.args("s").unwrap().string().unwrap().to_owned();
        assert!(rule.contains("NameOwnerChanged"), "{rule}");
        for _ in 0..2 {
            assert_eq!(self.call().member.as_deref(), Some("AddMatch"));
        }
        let owner = self.call();
        assert_eq!(owner.member.as_deref(), Some("GetNameOwner"));
        owner
    }

    /// Passes the lock, a pipe's write end, answering `inhibit`, and
    /// returns its read end, which ends once every copy of the lock is
    /// closed.
    fn lock(&mut self, inhibit: &Message) -> OwnedFd {
        self.lock_from(LOGIND, inhibit)
    }

    /// Passes a lock as [`FakeBus::lock`] does, from `sender`.
    fn lock_from(&mut self, sender: &str, inhibit: &Message) -> OwnedFd {
        let (read, write) = rustix::pipe::pipe().unwrap();
        let reply = Sent::Reply(inhibit.serial);
        self.send(sender, reply, ("h", &0u32.to_le_bytes()), Some(&write));
        read
    }

    /// Sends logind's signal `member`, saying `coming`, from `sender`.
    fn signal(&mut self, sender: &str, member: &'static str, coming: bool) {
        let body = u32::from(coming).to_le_bytes();
        self.send(sender, Sent::Signal(member), ("b", &body), None);
    }
}

/// What a message the fake bus sends is.
#[derive(Clone, Copy)]
enum Sent<'a> {
    /// The reply to a call, by its number.
    Reply(u32),
    /// An error, the answer to a call, and its name.
    Error(u32, &'a str),
    /// A signal of logind's.
    Signal(&'a str),
    /// A signal of the bus's own.
    Bus(&'a str),
}

/// Whether the pipe whose read end is `read` has ended: every copy of
/// its write end closed.
fn ended(read: &OwnedFd) -> bool {
    let mut poll = [rustix::event::PollFd::new(
        read,
        rustix::event::PollFlags::IN,
    )];
    let ready = rustix::event::poll(&mut poll, Some(&rustix::time::Timespec::default()));
    ready.is_ok_and(|ready| ready > 0)
}

/// The next news from `inhibitor`, failing after a few seconds.
fn next(inhibitor: &Inhibitor) -> News {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(news) = inhibitor.news.try_iter().next() {
            return news;
        }
        assert!(Instant::now() < until, "no news");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The body of the bus's `NameOwnerChanged`: `name` has gone from `old`
/// to `new`.
fn owner_changed(name: &str, old: &str, new: &str) -> Vec<u8> {
    let mut body = Vec::new();
    for text in [name, old, new] {
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        body.extend_from_slice(&string(text));
    }
    body
}

impl FakeBus {
    /// Says, as the bus, that logind's name has gone from `old` to `new`.
    fn logind_moved(&mut self, old: &str, new: &str) {
        let body = owner_changed(LOGIN, old, new);
        self.send(BUS, Sent::Bus("NameOwnerChanged"), ("sss", &body), None);
    }
}

/// A fake bus, listening at a socket in a directory of its own, and the
/// inhibitor connected to it. The directory goes when `dir` is dropped,
/// so a test binds it.
struct Connected {
    dir: tempfile::TempDir,
    listener: UnixListener,
    bus: FakeBus,
    inhibitor: Inhibitor,
}

fn connected() -> Connected {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("bus");
    let listener = UnixListener::bind(&socket).unwrap();
    let inhibitor = Inhibitor::start(format!("unix:path={}", socket.display()), || {});
    let bus = FakeBus::accept(&listener);
    Connected {
        dir,
        listener,
        bus,
        inhibitor,
    }
}

#[test]
fn the_lock_is_handed_over_as_logind_says_shutdown_comes_and_taken_again_if_it_doesnt() {
    let Connected {
        dir: _dir,
        mut bus,
        inhibitor,
        ..
    } = connected();
    let inhibit = bus.up_to_the_lock();
    let held = bus.lock(&inhibit);
    // Anyone else's signal is passed over.
    bus.signal(OTHER, "PrepareForShutdown", true);
    bus.signal(LOGIND, "PrepareForShutdown", true);
    let News::ShuttingDown(Some(lock)) = next(&inhibitor) else {
        panic!("not the shutdown, with the lock");
    };
    assert!(inhibitor.news().is_empty());
    assert!(
        !ended(&held),
        "the lock is held until the daemon lets it go"
    );
    drop(lock);
    assert!(ended(&held));

    // Called off: the lock is taken again.
    bus.signal(LOGIND, "PrepareForShutdown", false);
    assert!(matches!(next(&inhibitor), News::NotShuttingDown));
    let again = bus.call();
    assert_eq!(again.member.as_deref(), Some("Inhibit"));
    let held = bus.lock(&again);

    // As it is after sleeping, which says nothing more.
    bus.signal(LOGIND, "PrepareForSleep", true);
    let News::Sleeping(Some(lock)) = next(&inhibitor) else {
        panic!("not sleep, with the lock");
    };
    drop(lock);
    assert!(ended(&held));
    bus.signal(LOGIND, "PrepareForSleep", false);
    assert_eq!(bus.call().member.as_deref(), Some("Inhibit"));
    assert!(inhibitor.news().is_empty());
}

#[test]
fn letting_the_thread_go_lets_the_lock_go() {
    let Connected {
        dir: _dir,
        mut bus,
        inhibitor,
        ..
    } = connected();
    let inhibit = bus.up_to_the_lock();
    let held = bus.lock(&inhibit);
    assert!(!ended(&held));
    drop(inhibitor);
    let until = Instant::now() + Duration::from_secs(10);
    while !ended(&held) {
        assert!(Instant::now() < until, "the lock is still held");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// logind refusing the lock, or the bus going away, as when either
/// starts again, has the thread say the lock is lost, then ask again a
/// moment later, on a new connection.
#[test]
fn a_refusal_or_the_bus_going_has_the_thread_ask_again_later() {
    let Connected {
        dir: _dir,
        listener,
        mut bus,
        inhibitor,
        ..
    } = connected();
    let inhibit = bus.up_to_the_lock();
    let refused = Sent::Error(inhibit.serial, "org.freedesktop.DBus.Error.AccessDenied");
    bus.send(LOGIND, refused, ("s", &string("Permission denied")), None);
    assert!(matches!(next(&inhibitor), News::Lost));
    let asked = Instant::now();
    let mut bus = FakeBus::accept(&listener);
    assert!(
        asked.elapsed() >= Duration::from_millis(500),
        "asked again at once"
    );
    let inhibit = bus.up_to_the_lock();
    let held = bus.lock(&inhibit);

    // The bus goes, the lock with it.
    drop(bus);
    assert!(matches!(next(&inhibitor), News::Lost));
    let mut bus = FakeBus::accept(&listener);
    let inhibit = bus.up_to_the_lock();
    drop(held);
    let held = bus.lock(&inhibit);
    bus.signal(LOGIND, "PrepareForShutdown", true);
    let News::ShuttingDown(Some(lock)) = next(&inhibitor) else {
        panic!("not the shutdown, with the new lock");
    };
    drop(lock);
    assert!(ended(&held));
}

/// logind started again has a new name on the bus: its signals count from
/// then on, and the old name's don't, and it's asked for a lock of its
/// own. One that goes while it's asked is asked again once it's back, and
/// the bus's answer for it passed over.
#[test]
fn logind_starting_again_is_heard_under_its_new_name() {
    let Connected {
        dir: _dir,
        mut bus,
        inhibitor,
        ..
    } = connected();
    let inhibit = bus.up_to_the_lock();
    let held = bus.lock(&inhibit);
    let restarted = ":1.7";
    // Only the bus says who has a name.
    let forged = owner_changed(LOGIN, LOGIND, OTHER);
    bus.send(OTHER, Sent::Bus("NameOwnerChanged"), ("sss", &forged), None);
    bus.logind_moved(LOGIND, restarted);
    let asked = bus.call();
    assert_eq!(asked.member.as_deref(), Some("Inhibit"));
    bus.signal(LOGIND, "PrepareForShutdown", true);
    bus.signal(OTHER, "PrepareForShutdown", true);
    bus.signal(restarted, "PrepareForShutdown", true);
    let News::ShuttingDown(Some(lock)) = next(&inhibitor) else {
        panic!("not the shutdown, with the lock");
    };
    drop(lock);
    assert!(ended(&held));
    assert!(inhibitor.news().is_empty());

    // Gone while asked: what the bus answers for it is passed over, and
    // logind back is asked again.
    bus.logind_moved(restarted, "");
    let no_reply = Sent::Error(asked.serial, "org.freedesktop.DBus.Error.NoReply");
    bus.send(BUS, no_reply, ("s", &string("gone")), None);
    bus.logind_moved("", LOGIND);
    let again = bus.call();
    assert_eq!(again.member.as_deref(), Some("Inhibit"));
    let held = bus.lock(&again);
    bus.signal(LOGIND, "PrepareForShutdown", true);
    let News::ShuttingDown(Some(lock)) = next(&inhibitor) else {
        panic!("not the shutdown, with the lock");
    };
    drop(lock);
    assert!(ended(&held));
    assert!(inhibitor.news().is_empty());
}

/// logind not running as the daemon asks for its name: it's asked for its
/// lock once the bus says it is.
#[test]
fn logind_not_running_yet_is_asked_once_it_is() {
    let Connected {
        dir: _dir,
        mut bus,
        inhibitor,
        ..
    } = connected();
    let owner = bus.up_to_the_name();
    let none = Sent::Error(owner.serial, "org.freedesktop.DBus.Error.NameHasNoOwner");
    bus.send(BUS, none, ("s", &string("no owner")), None);
    bus.logind_moved("", LOGIND);
    let inhibit = bus.call();
    assert_eq!(inhibit.member.as_deref(), Some("Inhibit"));
    let held = bus.lock(&inhibit);
    bus.signal(LOGIND, "PrepareForShutdown", true);
    let News::ShuttingDown(Some(lock)) = next(&inhibitor) else {
        panic!("not the shutdown, with the lock");
    };
    drop(lock);
    assert!(ended(&held));
}

/// A lock only counts from logind itself: one that seems to answer the
/// daemon's call, from another connection, is passed over.
#[test]
fn a_lock_from_anyone_but_logind_is_passed_over() {
    let Connected {
        dir: _dir,
        mut bus,
        inhibitor,
        ..
    } = connected();
    let inhibit = bus.up_to_the_lock();
    let forged = bus.lock_from(OTHER, &inhibit);
    // Closed unseen as it was passed over.
    let until = Instant::now() + Duration::from_secs(10);
    while !ended(&forged) {
        assert!(Instant::now() < until, "the forged lock is kept");
        std::thread::sleep(Duration::from_millis(5));
    }
    let held = bus.lock(&inhibit);
    bus.signal(LOGIND, "PrepareForShutdown", true);
    let News::ShuttingDown(Some(lock)) = next(&inhibitor) else {
        panic!("not the shutdown, with logind's lock");
    };
    assert!(!ended(&held));
    drop(lock);
    assert!(ended(&held));
}
