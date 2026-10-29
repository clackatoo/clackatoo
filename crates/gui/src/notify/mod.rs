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
    /// A message from the bus.
    Heard(Message),
    /// The bus's connection ended.
    Lost,
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

/// The notifier's thread: the connection, and which notification is
/// which chat's.
struct Thread {
    proxy: Proxy,
    /// For the thread reading the bus to send to.
    back: Sender<Input>,
    bus: Option<Bus>,
    /// When the bus last couldn't be reached.
    failed: Option<Instant>,
    /// Each chat's notification, by the server's number for it.
    ids: HashMap<TerminalId, u32>,
    chats: HashMap<u32, TerminalId>,
    /// Activation tokens the server gave for notifications clicked, until
    /// the click itself comes.
    tokens: HashMap<u32, String>,
    /// What came while a reply was waited for, in order.
    later: VecDeque<Input>,
}

impl Thread {
    fn new(proxy: Proxy, back: Sender<Input>) -> Self {
        Self {
            proxy,
            back,
            bus: None,
            failed: None,
            ids: HashMap::new(),
            chats: HashMap::new(),
            tokens: HashMap::new(),
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
                Input::Heard(message) => self.heard(&message),
                Input::Lost => self.lost(),
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
        let back = self.back.clone();
        std::thread::Builder::new()
            .name("notifications-bus".into())
            .spawn(move || {
                loop {
                    match dbus::read_message(&mut reader) {
                        Ok(message) => {
                            if back.send(Input::Heard(message)).is_err() {
                                return;
                            }
                        }
                        Err(err) => {
                            log::debug!("the session bus's connection ended: {err}");
                            let _ = back.send(Input::Lost);
                            return;
                        }
                    }
                }
            })
            .map_err(|err| err.to_string())?;
        let hello = bus.hello().map_err(|err| err.to_string())?;
        let rule = format!("type='signal',interface='{NOTIFICATIONS}',path='{NOTIFICATIONS_PATH}'");
        bus.add_match(&rule).map_err(|err| err.to_string())?;
        self.bus = Some(bus);
        match self.wait_for(inputs, hello) {
            Some(reply) if reply.is(Kind::Reply) => Ok(()),
            Some(reply) => Err(format!("the bus said {:?} to hello", reply.error)),
            None => Err("the bus didn't answer".to_owned()),
        }
    }

    /// Waits for the reply to call `serial`, keeping what else comes for
    /// after. `None` if none came in time, or the bus went.
    fn wait_for(&mut self, inputs: &Receiver<Input>, serial: u32) -> Option<Message> {
        let until = Instant::now() + REPLY_WAIT;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match inputs.recv_timeout(left) {
                Ok(Input::Heard(message)) if message.reply_serial == Some(serial) => {
                    return Some(message);
                }
                Ok(Input::Lost) => {
                    self.lost();
                    return None;
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
                self.lost();
                return;
            }
        };
        let Some(reply) = self.wait_for(inputs, serial) else {
            log::warn!("the desktop's notification server didn't answer");
            return;
        };
        match reply.args("u").and_then(|mut args| args.u32()) {
            Some(id) if reply.is(Kind::Reply) => {
                if let Some(old) = self.ids.insert(chat, id)
                    && old != id
                {
                    self.chats.remove(&old);
                }
                self.chats.insert(id, chat);
            }
            _ => log::warn!(
                "the desktop's notification server refused a notification: {}",
                reply.error.as_deref().unwrap_or("it said nothing of why")
            ),
        }
    }

    fn withdraw(&mut self, chat: TerminalId) {
        let Some(id) = self.ids.remove(&chat) else {
            return;
        };
        self.chats.remove(&id);
        self.tokens.remove(&id);
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
            self.lost();
        }
    }

    /// A message from the bus: a signal about a notification, perhaps
    /// one of this window's.
    fn heard(&mut self, message: &Message) {
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

    /// The connection's gone: what was shown on it can't be replaced or
    /// closed by number on another, as far as this window knows.
    fn lost(&mut self) {
        self.bus = None;
        self.ids.clear();
        self.chats.clear();
        self.tokens.clear();
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
