//! Desktop notifications for chats the user can't see: one that finished,
//! asks something, or failed. Which to show, and when to take one back,
//! is the window's call (see `desk/notices.rs`), made with what the tabs'
//! marks go by; this is how they're shown.
//!
//! [`Desktop`] shows them as the freedesktop notification spec says, over
//! D-Bus on the session bus (see [`dbus`]), which GNOME, KDE and the other
//! Linux desktops' notification servers listen on. Talking to the bus
//! blocks, so it's done on a thread of its own, started the first time a
//! notification is shown, never as the window opens; the window only ever
//! puts what to do on a channel. Each chat has at most one notification,
//! replaced (`replaces_id`) as what it says changes, and closed when the
//! window takes it back.
//!
//! Clicking one sends [`SessionEvent::Notified`] to the window, with the
//! activation token the server gave for it, if it gave one, so the window
//! can raise itself on Wayland, where a window can't take the focus
//! without one.
//!
//! What a notification says is a chat's title and a line about it, never
//! what the agent wrote, as for the phone's (see the daemon's
//! `web/push.rs`): a lock screen can show it.

mod dbus;

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use clackatoo_protocol::TerminalId;

use crate::session::{Proxy, SessionEvent};

use dbus::{Body, Bus, Call, Kind, Message};

/// The notification server's name, path and interface.
const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

/// How long a call to the server may take to be answered.
const REPLY_WAIT: Duration = Duration::from_secs(5);

/// How long after the bus couldn't be reached it's tried again.
const RETRY_AFTER: Duration = Duration::from_mins(1);

/// A notification about a chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    /// Its first line: the chat's title.
    pub(crate) title: String,
    /// What happened, as "Claude Code finished".
    pub(crate) body: String,
}

/// What shows the window's notifications: [`Desktop`], or in tests, one
/// that notes what it's asked.
pub(crate) trait Notifier: std::fmt::Debug {
    /// Shows `notice` for `chat`, in place of the one it has, if any.
    fn show(&mut self, chat: TerminalId, notice: &Notice);
    /// Takes back `chat`'s notification, if it has one.
    fn withdraw(&mut self, chat: TerminalId);
}

/// What the notifier's thread is told: by the window, or by the thread
/// reading the bus.
#[derive(Debug)]
enum Input {
    Show(TerminalId, Notice),
    Withdraw(TerminalId),
    /// A message from the bus, on connection number `.0`.
    Heard(u64, Message),
    /// Connection number `.0` to the bus ended.
    Lost(u64),
    /// The window's gone.
    Quit,
}

/// The desktop's notifications, over D-Bus.
#[derive(Debug)]
pub(crate) struct Desktop {
    /// Where clicks are sent.
    proxy: Proxy,
    /// The thread's channel, once it's started.
    thread: Option<Sender<Input>>,
}

impl Desktop {
    /// The session bus's notifications, with clicks sent to `proxy`.
    pub(crate) fn new(proxy: Proxy) -> Self {
        Self {
            proxy,
            thread: None,
        }
    }

    /// Hands `input` to the thread, starting it if it isn't yet.
    fn send(&mut self, input: Input) {
        if self.thread.is_none() {
            let (sender, receiver) = mpsc::channel();
            let thread = Thread::new(self.proxy.clone(), sender.clone());
            let started = std::thread::Builder::new()
                .name("notifications".into())
                .spawn(move || thread.run(&receiver));
            match started {
                Ok(_) => self.thread = Some(sender),
                Err(err) => {
                    log::warn!("desktop notifications are off: {err}");
                    return;
                }
            }
        }
        if let Some(thread) = &self.thread {
            let _ = thread.send(input);
        }
    }
}

impl Drop for Desktop {
    /// Ends the thread, which the thread reading the bus would otherwise
    /// keep waiting, as it can tell it things too.
    fn drop(&mut self) {
        if let Some(thread) = &self.thread {
            let _ = thread.send(Input::Quit);
        }
    }
}

impl Notifier for Desktop {
    fn show(&mut self, chat: TerminalId, notice: &Notice) {
        self.send(Input::Show(chat, notice.clone()));
    }

    fn withdraw(&mut self, chat: TerminalId) {
        // Nothing was ever shown before the thread starts.
        if self.thread.is_some() {
            self.send(Input::Withdraw(chat));
        }
    }
}

/// A `Notify` call not yet answered: which chat's, and whether the window
/// took it back meanwhile.
#[derive(Clone, Copy, Debug)]
struct Pending {
    chat: TerminalId,
    withdrawn: bool,
}

/// The notifier's thread: the connection, and which notification is
/// which chat's.
struct Thread {
    proxy: Proxy,
    /// For the thread reading the bus to send to.
    back: Sender<Input>,
    bus: Option<Bus>,
    /// The connection's number, so what's heard of one gone is ignored.
    connection: u64,
    /// When the bus last couldn't be reached.
    failed: Option<Instant>,
    /// The notification server's unique name on the bus, as `:1.42`: only
    /// its signals count, as anyone on the bus could send one like them.
    /// `None` while no server runs, until the bus starts one.
    server: Option<String>,
    /// Each chat's notification, by the server's number for it.
    ids: HashMap<TerminalId, u32>,
    chats: HashMap<u32, TerminalId>,
    /// Activation tokens the server gave for notifications clicked, until
    /// the click itself comes.
    tokens: HashMap<u32, String>,
    /// `Notify` calls not yet answered, by their numbers: an answer that
    /// comes late is still filed.
    pending: HashMap<u32, Pending>,
    /// The newest `Notify` call for each chat, by its number.
    latest: HashMap<TerminalId, u32>,
    /// What came while a reply was waited for, in order.
    later: VecDeque<Input>,
}

impl Thread {
    fn new(proxy: Proxy, back: Sender<Input>) -> Self {
        Self {
            proxy,
            back,
            bus: None,
            connection: 0,
            failed: None,
            server: None,
            ids: HashMap::new(),
            chats: HashMap::new(),
            tokens: HashMap::new(),
            pending: HashMap::new(),
            latest: HashMap::new(),
            later: VecDeque::new(),
        }
    }

    /// Does what it's told until the window's gone, then takes its
    /// notifications back, as nothing would come of clicking them.
    fn run(mut self, inputs: &Receiver<Input>) {
        loop {
            let input = match self.later.pop_front() {
                Some(input) => input,
                None => match inputs.recv() {
                    Ok(input) => input,
                    Err(_) => break,
                },
            };
            match input {
                Input::Quit => break,
                Input::Show(chat, notice) => self.show(inputs, chat, &notice),
                Input::Withdraw(chat) => self.withdraw(chat),
                Input::Heard(connection, message) => {
                    if connection == self.connection {
                        self.heard(&message);
                    }
                }
                Input::Lost(connection) => self.lost(connection),
            }
        }
        let chats: Vec<TerminalId> = self.ids.keys().copied().collect();
        for chat in chats {
            self.withdraw(chat);
        }
    }

    /// Connects to the session bus, unless it is, or couldn't be lately.
    fn connect(&mut self, inputs: &Receiver<Input>) -> bool {
        if self.bus.is_some() {
            return true;
        }
        if self.failed.is_some_and(|at| at.elapsed() < RETRY_AFTER) {
            return false;
        }
        match self.open(inputs) {
            Ok(()) => true,
            Err(err) => {
                // Logged once a minute at most, as the next try is.
                log::warn!("desktop notifications can't be shown: {err}");
                self.bus = None;
                self.failed = Some(Instant::now());
                false
            }
        }
    }

    fn open(&mut self, inputs: &Receiver<Input>) -> Result<(), String> {
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .ok()
            .filter(|address| !address.is_empty())
            .or_else(|| {
                let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
                Some(format!("unix:path={runtime}/bus"))
            })
            .ok_or("there's no session bus")?;
        let mut bus = Bus::connect(&address).map_err(|err| format!("the session bus: {err}"))?;
        let mut reader = bus.reader().map_err(|err| err.to_string())?;
        self.connection += 1;
        self.forget();
        let connection = self.connection;
        let back = self.back.clone();
        std::thread::Builder::new()
            .name("notifications-bus".into())
            .spawn(move || {
                loop {
                    match dbus::read_message(&mut reader) {
                        Ok(Some(message)) => {
                            if back.send(Input::Heard(connection, message)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => log::debug!("read past a message too big for the window"),
                        Err(err) => {
                            log::debug!("the session bus's connection ended: {err}");
                            let _ = back.send(Input::Lost(connection));
                            return;
                        }
                    }
                }
            })
            .map_err(|err| err.to_string())?;
        let hello = bus.hello().map_err(|err| err.to_string())?;
        self.bus = Some(bus);
        match self.wait_for(inputs, hello) {
            Some(reply) if reply.is(Kind::Reply) => {}
            Some(reply) => return Err(format!("the bus said {:?} to hello", reply.error)),
            None => return Err("the bus didn't answer".to_owned()),
        }
        // Only once the bus has said hello back, as nothing may come
        // before. The server's signals, and its coming and going, which
        // the bus itself says; then who the server is now, if one runs.
        let rules = [
            format!(
                "type='signal',sender='{NOTIFICATIONS}',interface='{NOTIFICATIONS}',path='{NOTIFICATIONS_PATH}'"
            ),
            format!(
                "type='signal',sender='{BUS}',interface='{BUS}',member='NameOwnerChanged',arg0='{NOTIFICATIONS}'",
                BUS = dbus::BUS
            ),
        ];
        let bus = self.bus.as_mut().ok_or("the bus's connection ended")?;
        for rule in &rules {
            bus.add_match(rule).map_err(|err| err.to_string())?;
        }
        let owner = bus
            .get_name_owner(NOTIFICATIONS)
            .map_err(|err| err.to_string())?;
        if let Some(reply) = self.wait_for(inputs, owner)
            && reply.is(Kind::Reply)
            && let Some(name) = reply.args("s").and_then(|mut args| args.string())
        {
            self.server = Some(name.to_owned());
        }
        Ok(())
    }

    /// Waits for the reply to call `serial`, keeping what the window asks
    /// meanwhile for after, and hearing the rest. `None` if none came in
    /// time, or the bus went.
    fn wait_for(&mut self, inputs: &Receiver<Input>, serial: u32) -> Option<Message> {
        let until = Instant::now() + REPLY_WAIT;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match inputs.recv_timeout(left) {
                Ok(Input::Heard(connection, message)) if connection == self.connection => {
                    if message.reply_serial == Some(serial) {
                        return Some(message);
                    }
                    self.heard(&message);
                }
                Ok(Input::Heard(..)) => {}
                Ok(Input::Lost(connection)) => {
                    if connection == self.connection {
                        self.lost(connection);
                        return None;
                    }
                }
                Ok(other) => self.later.push_back(other),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn show(&mut self, inputs: &Receiver<Input>, chat: TerminalId, notice: &Notice) {
        if !self.connect(inputs) {
            return;
        }
        let replaces = self.ids.get(&chat).copied().unwrap_or(0);
        let body = notify_body(replaces, notice);
        let call = Call {
            destination: NOTIFICATIONS,
            path: NOTIFICATIONS_PATH,
            interface: NOTIFICATIONS,
            member: "Notify",
            signature: "susssasa{sv}i",
        };
        let Some(bus) = self.bus.as_mut() else {
            return;
        };
        let serial = match bus.call(&call, &body, false) {
            Ok(serial) => serial,
            Err(err) => {
                log::warn!("a desktop notification couldn't be sent: {err}");
                let connection = self.connection;
                self.lost(connection);
                return;
            }
        };
        self.latest.insert(chat, serial);
        self.pending.insert(
            serial,
            Pending {
                chat,
                withdrawn: false,
            },
        );
        // Waited for, so the next notification of the chat replaces this
        // one; one later than that is filed as it comes (see `answered`).
        match self.wait_for(inputs, serial) {
            Some(reply) => self.answered(&reply),
            None => log::warn!("the desktop's notification server is slow to answer"),
        }
    }

    /// Files the answer to a `Notify` call: the server's number for the
    /// chat's notification. One the window has taken back, or replaced
    /// with another meanwhile, is closed.
    fn answered(&mut self, reply: &Message) {
        let Some(pending) = reply
            .reply_serial
            .and_then(|serial| self.pending.remove(&serial))
        else {
            return;
        };
        let id = match reply.args("u").and_then(|mut args| args.u32()) {
            Some(id) if reply.is(Kind::Reply) => id,
            _ => {
                log::warn!(
                    "the desktop's notification server refused a notification: {}",
                    reply.error.as_deref().unwrap_or("it said nothing of why")
                );
                return;
            }
        };
        if pending.withdrawn {
            self.close(id);
            return;
        }
        let chat = pending.chat;
        let newest =
            reply.reply_serial.is_some() && self.latest.get(&chat) == reply.reply_serial.as_ref();
        if let Some(old) = self.ids.get(&chat).copied()
            && old != id
        {
            // Two notifications of one chat, as when an answer came too
            // late to be replaced: the newer call's stays.
            if !newest {
                self.close(id);
                return;
            }
            self.chats.remove(&old);
            self.tokens.remove(&old);
            self.close(old);
        }
        self.ids.insert(chat, id);
        self.chats.insert(id, chat);
    }

    fn withdraw(&mut self, chat: TerminalId) {
        for pending in self.pending.values_mut() {
            if pending.chat == chat {
                pending.withdrawn = true;
            }
        }
        let Some(id) = self.ids.remove(&chat) else {
            return;
        };
        self.chats.remove(&id);
        self.tokens.remove(&id);
        self.close(id);
    }

    /// Closes the server's notification `id`.
    fn close(&mut self, id: u32) {
        let call = Call {
            destination: NOTIFICATIONS,
            path: NOTIFICATIONS_PATH,
            interface: NOTIFICATIONS,
            member: "CloseNotification",
            signature: "u",
        };
        let mut body = Body::default();
        body.u32(id);
        if let Some(bus) = self.bus.as_mut()
            && let Err(err) = bus.call(&call, &body, true)
        {
            log::debug!("a desktop notification couldn't be closed: {err}");
            let connection = self.connection;
            self.lost(connection);
        }
    }

    /// A message from the bus: a late answer, the server coming or going,
    /// or a signal about a notification, perhaps one of this window's.
    /// Signals count only from the server they're of.
    fn heard(&mut self, message: &Message) {
        if message.reply_serial.is_some() {
            self.answered(message);
            return;
        }
        if message.is_signal(dbus::BUS, "NameOwnerChanged") {
            if message.sender.as_deref() == Some(dbus::BUS)
                && let Some(mut args) = message.args("sss")
                && let (Some(name), Some(_old), Some(new)) =
                    (args.string(), args.string(), args.string())
                && name == NOTIFICATIONS
            {
                // Another server's numbers are its own: what this window
                // knew of the last one's means nothing to it.
                self.forget();
                self.server = (!new.is_empty()).then(|| new.to_owned());
            }
            return;
        }
        if self.server.is_none() || message.sender != self.server {
            return;
        }
        if message.is_signal(NOTIFICATIONS, "ActivationToken") {
            // Sent before the click it's for.
            if let Some(mut args) = message.args("us")
                && let (Some(id), Some(token)) = (args.u32(), args.string())
                && self.chats.contains_key(&id)
            {
                self.tokens.insert(id, token.to_owned());
            }
        } else if message.is_signal(NOTIFICATIONS, "ActionInvoked") {
            if let Some(mut args) = message.args("us")
                && let (Some(id), Some(action)) = (args.u32(), args.string())
                && action == "default"
                && let Some(&chat) = self.chats.get(&id)
            {
                let token = self.tokens.remove(&id);
                let _ = self
                    .proxy
                    .send_event(SessionEvent::Notified { chat, token });
            }
        } else if message.is_signal(NOTIFICATIONS, "NotificationClosed")
            && let Some(id) = message.args("uu").and_then(|mut args| args.u32())
            && let Some(chat) = self.chats.remove(&id)
        {
            self.ids.remove(&chat);
            self.tokens.remove(&id);
        }
    }

    /// Connection number `connection` is gone: if it's the one in use,
    /// what was shown on it can't be replaced or closed by number on
    /// another, as far as this window knows. A gone one's is nothing.
    fn lost(&mut self, connection: u64) {
        if connection != self.connection {
            return;
        }
        self.bus = None;
        self.server = None;
        self.forget();
    }

    /// Forgets which notification is which chat's.
    fn forget(&mut self) {
        self.ids.clear();
        self.chats.clear();
        self.tokens.clear();
        self.pending.clear();
        self.latest.clear();
    }
}

/// `Notify`'s arguments, for `notice`, in place of notification `replaces`
/// (0 for none): from clackatoo, with its icon, clicked to open the chat.
fn notify_body(replaces: u32, notice: &Notice) -> Body {
    let mut body = Body::default();
    body.string("clackatoo");
    body.u32(replaces);
    body.string("clackatoo");
    body.string(&notice.title);
    body.string(&notice.body);
    let actions = body.begin_array(4);
    // The default action is a click on the notification itself.
    body.string("default");
    body.string("Open");
    body.end_array(actions);
    let hints = body.begin_array(8);
    body.begin_struct();
    // Which desktop file is the sender's, for its name and icon, and so
    // the server can give an activation token for the click.
    body.string("desktop-entry");
    body.variant_string("clackatoo");
    body.begin_struct();
    // Normal, never critical, even for a question: critical ones break
    // through Do Not Disturb.
    body.string("urgency");
    body.variant_byte(1);
    body.end_array(hints);
    // The server's choice of how long it shows.
    body.i32(-1);
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER: &str = ":1.5";

    /// A thread on connection 1, whose server is [`SERVER`], with chat 4's
    /// notification its number 7, and where the clicks it sends go.
    fn thread() -> (Thread, Receiver<SessionEvent>) {
        let (sender, events) = mpsc::channel();
        let (back, _) = mpsc::channel();
        let mut thread = Thread::new(Proxy::Channel(sender), back);
        thread.connection = 1;
        thread.server = Some(SERVER.to_owned());
        thread.ids.insert(TerminalId(4), 7);
        thread.chats.insert(7, TerminalId(4));
        (thread, events)
    }

    fn signal(
        sender: &str,
        member: &str,
        signature: &str,
        write: impl FnOnce(&mut Body),
    ) -> Message {
        let mut body = Body::default();
        write(&mut body);
        let interface = if member == "NameOwnerChanged" {
            dbus::BUS
        } else {
            NOTIFICATIONS
        };
        Message {
            kind: Kind::Signal as u8,
            sender: Some(sender.to_owned()),
            interface: Some(interface.to_owned()),
            member: Some(member.to_owned()),
            signature: signature.to_owned(),
            body: body.as_bytes().to_vec(),
            ..Message::default()
        }
    }

    fn click(sender: &str, id: u32) -> [Message; 2] {
        [
            signal(sender, "ActivationToken", "us", |body| {
                body.u32(id);
                body.string("token");
            }),
            signal(sender, "ActionInvoked", "us", |body| {
                body.u32(id);
                body.string("default");
            }),
        ]
    }

    fn owner_changed(sender: &str, new: &str) -> Message {
        signal(sender, "NameOwnerChanged", "sss", |body| {
            body.string(NOTIFICATIONS);
            body.string(SERVER);
            body.string(new);
        })
    }

    fn answer(serial: u32, id: u32) -> Message {
        let mut body = Body::default();
        body.u32(id);
        Message {
            kind: Kind::Reply as u8,
            reply_serial: Some(serial),
            sender: Some(SERVER.to_owned()),
            signature: "u".to_owned(),
            body: body.as_bytes().to_vec(),
            ..Message::default()
        }
    }

    #[test]
    fn a_click_counts_only_from_the_notification_server() {
        let (mut thread, events) = thread();
        for message in click(":1.6", 7) {
            thread.heard(&message);
        }
        assert!(
            events.try_recv().is_err(),
            "another peer on the bus can't click"
        );
        for message in click(SERVER, 7) {
            thread.heard(&message);
        }
        match events.try_recv() {
            Ok(SessionEvent::Notified { chat, token }) => {
                assert_eq!((chat, token.as_deref()), (TerminalId(4), Some("token")));
            }
            other => panic!("{other:?}"),
        }
        // Another app's notification, clicked: not this window's.
        for message in click(SERVER, 8) {
            thread.heard(&message);
        }
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_server_restarting_forgets_the_last_ones_numbers() {
        let (mut thread, events) = thread();
        // Said by anyone but the bus: nothing.
        thread.heard(&owner_changed(":1.6", ":1.8"));
        assert_eq!(thread.server.as_deref(), Some(SERVER));
        assert_eq!(thread.ids.len(), 1);
        thread.heard(&owner_changed(dbus::BUS, ":1.8"));
        assert_eq!(thread.server.as_deref(), Some(":1.8"));
        assert!(thread.ids.is_empty() && thread.chats.is_empty());
        // The new server's notification 7 is another app's.
        for message in click(":1.8", 7) {
            thread.heard(&message);
        }
        assert!(events.try_recv().is_err());
        // Gone: no server, so no signal counts.
        thread.heard(&owner_changed(dbus::BUS, ""));
        assert_eq!(thread.server, None);
    }

    #[test]
    fn a_notification_closed_is_forgotten() {
        let (mut thread, _events) = thread();
        thread.heard(&signal(SERVER, "NotificationClosed", "uu", |body| {
            body.u32(7);
            body.u32(2);
        }));
        assert!(thread.ids.is_empty() && thread.chats.is_empty());
    }

    #[test]
    fn only_the_connection_in_use_being_lost_counts() {
        let (mut thread, _events) = thread();
        thread.lost(0);
        assert_eq!(thread.ids.len(), 1);
        assert!(thread.server.is_some());
        thread.lost(1);
        assert!(thread.ids.is_empty());
        assert_eq!(thread.server, None);
    }

    #[test]
    fn a_late_answer_is_filed_closed_or_dropped_as_it_should_be() {
        let (mut thread, _events) = thread();
        let pending = |chat| Pending {
            chat: TerminalId(chat),
            withdrawn: false,
        };
        // Chat 5's, late: filed.
        thread.pending.insert(3, pending(5));
        thread.latest.insert(TerminalId(5), 3);
        thread.heard(&answer(3, 9));
        assert_eq!(thread.ids.get(&TerminalId(5)), Some(&9));
        assert_eq!(thread.chats.get(&9), Some(&TerminalId(5)));
        // Taken back before it came: not filed.
        thread.pending.insert(4, pending(6));
        thread.latest.insert(TerminalId(6), 4);
        thread.withdraw(TerminalId(6));
        thread.heard(&answer(4, 10));
        assert!(!thread.ids.contains_key(&TerminalId(6)));
        // An older call's, after a newer one's was filed: dropped.
        thread.pending.insert(5, pending(4));
        thread.latest.insert(TerminalId(4), 6);
        thread.heard(&answer(5, 12));
        assert_eq!(thread.ids.get(&TerminalId(4)), Some(&7));
        assert!(!thread.chats.contains_key(&12));
        // The newest, with another number: it replaces the one before.
        thread.pending.insert(6, pending(4));
        thread.heard(&answer(6, 13));
        assert_eq!(thread.ids.get(&TerminalId(4)), Some(&13));
        assert!(!thread.chats.contains_key(&7));
    }

    #[test]
    fn notify_says_who_what_and_the_click() {
        let notice = Notice {
            title: "notes".to_owned(),
            body: "Claude Code finished".to_owned(),
        };
        let body = notify_body(5, &notice);
        let bytes = body.as_bytes();
        let text = String::from_utf8_lossy(bytes);
        for word in [
            "clackatoo",
            "notes",
            "Claude Code finished",
            "default",
            "desktop-entry",
            "urgency",
        ] {
            assert!(text.contains(word), "{word} in {text:?}");
        }
        // app_name "clackatoo" (4 + 10 bytes), padding to 16, then 5.
        assert_eq!(&bytes[16..20], &5u32.to_le_bytes());
        // Ends with the timeout, -1.
        assert_eq!(&bytes[bytes.len() - 4..], &(-1i32).to_le_bytes());
    }
}
