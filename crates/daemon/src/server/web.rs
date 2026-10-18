//! The web app on the event loop: its listener and HTTP connections, the
//! WebSocket connections paired devices are clients over, the devices asking to
//! pair, and the chats as a list, with what needs the user, and the
//! notifications sent when no one is looking (see [`crate::web`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    ChatState, ChatSummary, Device, Error, Event, MachineId, Outcome, Response, ServerMessage,
    TerminalId, WorkspaceId,
};
use clackatoo_protocol::{frame, wire};
use mio::{Interest, Token};

use super::{Broadcast, Client, Daemon, Link, Source, WEB};
use crate::web::{self, Filled, Handled, HttpConn, Read, State, Web};

/// The most HTTP connections open at once, WebSocket connections apart:
/// past it, a new one is closed at once.
const MOST_CONNECTIONS: usize = 64;

/// The most HTTP connections open at once from one address, TLS
/// handshakes under way among them.
const MOST_FROM_ONE: usize = 8;

/// The most requests a connection has answered in one turn of the event
/// loop: the rest wait for the next.
const MOST_REQUESTS_AT_ONCE: usize = 16;

/// Why a connection's requests stopped being answered for now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stopped {
    /// None more is whole, or one waits for an answer.
    Done,
    /// Enough for one turn.
    Enough,
    /// Its answers back up, unread.
    BackedUp,
}

/// How long `here` from a client lasts unless it's said again.
pub(super) const PRESENT_FOR: Duration = Duration::from_mins(5);

/// How long a device goes without typing into a terminal before its next
/// key is noted in the log again, as a new spell of typing.
const TYPING_SPELL: Duration = Duration::from_mins(5);

/// How long after a chat finishes or asks something its notification
/// waits, in case someone looks, or the question is answered, meanwhile.
const NOTIFY_AFTER: Duration = Duration::from_secs(3);

/// The chats as the list of them shows them, and what's to be told.
#[derive(Debug, Default)]
pub(super) struct ChatWatch {
    /// What each chat was when last looked at.
    glances: BTreeMap<TerminalId, Glance>,
    /// The chats whose turn ended while no one present was looking, and
    /// no one has looked since.
    unseen: BTreeSet<TerminalId>,
    /// The list changed since the clients watching it were told.
    pub(super) dirty: bool,
    /// Notifications waiting to go.
    due: Vec<Notice>,
    /// The list as every client watching it was last told, if they all
    /// were told the same.
    sent: Option<Vec<ChatSummary>>,
}

/// What a chat was, as last looked at.
#[derive(Debug)]
struct Glance {
    /// The question it asked, by its number.
    question: Option<u64>,
    answering: bool,
    summary: ChatSummary,
}

/// A notification waiting to go.
#[derive(Debug)]
struct Notice {
    at: Instant,
    chat: TerminalId,
    /// It asked something, rather than finished.
    asked: bool,
}

impl Daemon {
    /// Starts the web app, if the config says to, and listens.
    pub(super) fn start_web(&mut self) {
        let Some(config) = self.config.web.clone() else {
            return;
        };
        match Web::start(config, &self.waker) {
            Ok(mut web) => {
                if let Err(err) =
                    self.poll
                        .registry()
                        .register(web.listener.source(), WEB, Interest::READABLE)
                {
                    log::error!("the web app couldn't be watched: {err}");
                    return;
                }
                log::info!("the web app listens on {}", web.describe());
                if let Some(due) = web.reload_due() {
                    web.due_by(due);
                }
                self.web = Some(web);
            }
            Err(err) => log::error!("the web app is off: {err}"),
        }
    }

    /// Where the web app listens on the network, if it does: for tests,
    /// which ask it to listen on any port.
    #[must_use]
    pub fn web_address(&self) -> Option<std::net::SocketAddr> {
        self.web.as_ref()?.address()
    }

    /// Takes the web connections waiting.
    pub(super) fn accept_web(&mut self) {
        let Some(web) = self.web.as_mut() else {
            return;
        };
        loop {
            let (stream, peer) = match web.listener.accept() {
                Ok(accepted) => accepted,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    log::warn!("accepting a web connection failed: {err}");
                    return;
                }
            };
            let (mut open, mut from_there) = (0, 0);
            for token in &web.conns {
                if let Some(Source::Web(conn)) = self.sources.get(token.0) {
                    open += 1;
                    from_there += usize::from(peer.ip().is_some() && conn.peer.ip() == peer.ip());
                }
            }
            if open >= MOST_CONNECTIONS || from_there >= MOST_FROM_ONE {
                log::warn!("a web connection from {peer} closed: too many are open");
                continue;
            }
            let transport = match web::Transport::new(stream, web.tls.as_ref()) {
                Ok(transport) => transport,
                Err(err) => {
                    log::warn!("a web connection from {peer} failed: {err}");
                    continue;
                }
            };
            let now = Instant::now();
            // Reached over HTTPS either way: TLS of its own, or Tailscale's.
            let mut conn = HttpConn::new(transport, peer, now, web.config.head_wait, true);
            let entry = self.sources.vacant_entry();
            let token = Token(entry.key());
            if let Err(err) =
                self.poll
                    .registry()
                    .register(conn.transport.stream(), token, Interest::READABLE)
            {
                log::warn!("watching a web connection failed: {err}");
                continue;
            }
            web.due_by(conn.due());
            web.conns.push(token);
            entry.insert(Source::Web(Box::new(conn)));
        }
    }

    /// Reads what an HTTP connection sent, at most a turn's worth, answers
    /// what's whole of it, and writes what it can of the answers.
    pub(super) fn serve_web(&mut self, token: Token) {
        let now = Instant::now();
        let Some(Source::Web(conn)) = self.sources.get_mut(token.0) else {
            return;
        };
        match conn.fill(&mut self.buffer, now) {
            Filled::Closed => {
                self.drop_web(token);
                return;
            }
            Filled::Open { more: true } => self.read_again(token),
            Filled::Open { more: false } => {}
        }
        if let (Some(Source::Web(conn)), Some(web)) = (self.sources.get(token.0), self.web.as_mut())
        {
            web.due_by(conn.due());
        }
        self.answer_web(token, now);
    }

    /// Answers the requests whole of what an HTTP connection sent, a few
    /// at a time, and not while its answers back up, then writes what it
    /// can of them.
    fn answer_web(&mut self, token: Token, now: Instant) {
        let mut answered = 0;
        let mut stopped = Stopped::Done;
        loop {
            let (Some(Source::Web(conn)), Some(web)) =
                (self.sources.get_mut(token.0), self.web.as_mut())
            else {
                return;
            };
            if conn.backed_up() {
                stopped = Stopped::BackedUp;
                break;
            }
            if answered == MOST_REQUESTS_AT_ONCE {
                stopped = Stopped::Enough;
                break;
            }
            let request = match conn.next_request(now) {
                Read::Request(request) => request,
                Read::Nothing => break,
                Read::Closed => {
                    self.drop_web(token);
                    return;
                }
            };
            answered += 1;
            let head_only = request.method == web::Method::Head;
            match web.handle(conn, token, &request, now) {
                Handled::Respond(response) => conn.respond(&response, head_only, now),
                // Nothing it sent after is answered.
                Handled::Close(response) => {
                    conn.respond_and_close(&response, now);
                    break;
                }
                Handled::Wait => break,
                Handled::Upgrade { device, response } => {
                    conn.respond(&response, false, now);
                    self.become_web_client(token, device);
                    return;
                }
            }
        }
        self.flush_web(token);
        // What's waiting is looked at next turn: the socket won't say
        // anything new, if it's all been read.
        let backed_up = matches!(self.sources.get(token.0),
            Some(Source::Web(conn)) if conn.backed_up());
        if stopped == Stopped::Enough || (stopped == Stopped::BackedUp && !backed_up) {
            self.read_again(token);
        }
    }

    /// Serves the connection at `token` again on the loop's next turn: it
    /// has more to read, or to answer, and won't say so again.
    pub(super) fn read_again(&mut self, token: Token) {
        if !self.web_unread.contains(&token) {
            self.web_unread.push(token);
        }
    }

    /// Serves the web connections that had more to read or answer.
    pub(super) fn serve_web_unread(&mut self) {
        for token in std::mem::take(&mut self.web_unread) {
            match self.sources.get(token.0) {
                Some(Source::Web(_)) => self.serve_web(token),
                Some(Source::Client(Client {
                    link: Link::Web(_), ..
                })) => self.serve_client(token),
                _ => {}
            }
        }
    }

    /// Writes what it can of an HTTP connection's answers, and drops it
    /// once it's done with.
    fn flush_web(&mut self, token: Token) {
        let Some(Source::Web(conn)) = self.sources.get_mut(token.0) else {
            return;
        };
        if conn.flush(self.poll.registry(), token) {
            self.drop_web(token);
        }
    }

    /// Forgets an HTTP connection, closing it.
    fn drop_web(&mut self, token: Token) {
        if let Some(Source::Web(mut conn)) = self.sources.try_remove(token.0) {
            let _ = self.poll.registry().deregister(conn.transport.stream());
        }
        if let Some(web) = self.web.as_mut() {
            web.conns.retain(|&conn| conn != token);
        }
    }

    /// Makes the HTTP connection at `token`, whose answer opening a
    /// WebSocket is queued, a client: `device`'s.
    fn become_web_client(&mut self, token: Token, device: u64) {
        let Some(Source::Web(conn)) = self.sources.try_remove(token.0) else {
            return;
        };
        let now = Instant::now();
        let link = conn.into_link(device, now);
        let entry = self.sources.vacant_entry();
        let client = Token(entry.key());
        entry.insert(Source::Client(Client::new(Link::Web(Box::new(link)))));
        if let Some(web) = self.web.as_mut() {
            web.conns.retain(|&conn| conn != token);
            web.conns.push(client);
        }
        let Some(Source::Client(Client {
            link: Link::Web(link),
            ..
        })) = self.sources.get_mut(client.0)
        else {
            return;
        };
        if let Err(err) = self
            .poll
            .registry()
            .reregister(link.stream(), client, Interest::READABLE)
        {
            log::warn!("watching a device's connection failed: {err}");
            self.drop_client(client);
            return;
        }
        // Writes the answer, and reads whatever came with it.
        self.serve_client(client);
    }

    /// When the web app next needs something done, if it does.
    pub(super) fn next_web_due(&self) -> Option<Instant> {
        let web_due = self.web.as_ref().and_then(|web| web.next_due);
        let notice = self.chat_watch.due.iter().map(|notice| notice.at).min();
        web_due.into_iter().chain(notice).min()
    }

    /// Does what the web app's threads and timers ask: answers passwords
    /// checked, gives up on connections waiting too long, pings quiet
    /// devices, cuts off those no longer let in, sends notifications due,
    /// and tells windows of devices asking.
    pub(super) fn web_chores(&mut self) {
        // Any client may watch the chats, the web app on or not.
        self.send_chat_summaries();
        let Some(web) = self.web.as_mut() else {
            return;
        };
        web.drop_gone_subscriptions();
        let sources = &self.sources;
        let waiting = |token: Token, number| {
            matches!(sources.get(token.0),
                Some(Source::Web(conn)) if matches!(conn.state, State::Verifying(n) if n == number))
        };
        let now = Instant::now();
        for (token, response) in web.checked(waiting) {
            if let Some(Source::Web(conn)) = self.sources.get_mut(token.0) {
                conn.respond(&response, false, now);
                self.answer_web(token, now);
            }
        }
        if self.next_web_due().is_some_and(|due| due <= now) {
            self.web_timers(now);
        }
        self.send_pairing();
    }

    /// Acts on the web app's timers due by `now`.
    fn web_timers(&mut self, now: Instant) {
        let Some(web) = self.web.as_mut() else {
            self.chat_watch.due.clear();
            return;
        };
        web.next_due = None;
        web.drop_old_asks(now);
        web.reload_certificate(now);
        // Devices whose password is to be given again, or unused too long,
        // are cut off, telling them why; the latter are unpaired.
        let connected = self.connected_devices();
        if let Some(web) = self.web.as_mut() {
            let now_ms = web::epoch_millis();
            let lapsed: Vec<(u64, web::Denied)> = (connected.iter())
                .filter_map(|&device| Some((device, web.lapsed(device, now_ms)?)))
                .collect();
            web.expire();
            for (device, denied) in lapsed {
                let (status, why) = denied.close();
                log::info!("device {device} cut off: {why}");
                self.cut_off_device(device, status, why);
            }
        }
        self.settle_pairings(now);
        let tokens = self
            .web
            .as_ref()
            .map(|web| web.conns.clone())
            .unwrap_or_default();
        let mut next: Option<Instant> = None;
        let mut soonest = |at: Instant| next = Some(next.map_or(at, |next| next.min(at)));
        for token in tokens {
            match self.sources.get_mut(token.0) {
                Some(Source::Web(conn)) if now >= conn.due() => {
                    log::debug!("a web connection from {} timed out", conn.peer);
                    self.drop_web(token);
                }
                Some(Source::Web(conn)) => soonest(conn.due()),
                Some(Source::Client(Client {
                    link: Link::Web(link),
                    ..
                })) => {
                    if link.tick(now) {
                        log::info!(
                            "device {} at {} went quiet; dropping it",
                            link.device,
                            link.peer
                        );
                        self.drop_client(token);
                    } else {
                        soonest(link.due());
                        self.flush_client(token);
                    }
                }
                _ => {}
            }
        }
        let connected = self.connected_devices();
        if let Some(web) = self.web.as_ref() {
            for ask in web.asks_until() {
                soonest(ask);
            }
            if let Some(lapse) = web.next_lapse(&connected, now) {
                soonest(lapse);
            }
            if let Some(reload) = web.reload_due() {
                soonest(reload);
            }
        }
        self.notify_due(now);
        if let Some(web) = self.web.as_mut()
            && let Some(next) = next
        {
            web.due_by(next);
        }
    }

    /// The devices connected now.
    fn connected_devices(&self) -> Vec<u64> {
        let Some(web) = self.web.as_ref() else {
            return Vec::new();
        };
        (web.conns.iter())
            .filter_map(|token| match self.sources.get(token.0) {
                Some(Source::Client(Client {
                    link: Link::Web(link),
                    ..
                })) => Some(link.device),
                _ => None,
            })
            .collect()
    }

    /// Notes in the log what a device asks of a chat that answers or
    /// sends something: a phone acts with the user's authority, and the
    /// log says which phone did.
    pub(super) fn note_device_action(
        &self,
        token: Token,
        chat: TerminalId,
        action: &clackatoo_protocol::ChatAction,
    ) {
        use clackatoo_protocol::ChatAction;
        let Some(Source::Client(Client {
            link: Link::Web(link),
            ..
        })) = self.sources.get(token.0)
        else {
            return;
        };
        let what = match action {
            ChatAction::Answer { option, .. } => {
                format!("answered chat {chat}'s question with {option:?}")
            }
            ChatAction::Send(text) => format!("sent chat {chat} a message of {} bytes", text.len()),
            ChatAction::Stop => format!("stopped chat {chat}"),
            ChatAction::Set { setting, value } => format!("set chat {chat}'s {setting} to {value}"),
            ChatAction::Switch { agent, .. } => format!("moved chat {chat} to {agent}"),
            _ => return,
        };
        log::info!("device {} {what}", link.device);
    }

    /// Notes in the log that a device typed into terminal `terminal`:
    /// once when it starts, and again after a pause of [`TYPING_SPELL`],
    /// so the log says which phone typed without a line for every key.
    /// What it typed is never written, as it may be a password.
    pub(super) fn note_device_typing(&mut self, token: Token, terminal: TerminalId) {
        let Some(Source::Client(Client {
            link: Link::Web(link),
            ..
        })) = self.sources.get_mut(token.0)
        else {
            return;
        };
        let now = Instant::now();
        let last = link.typed.insert(terminal, now);
        if last.is_none_or(|last| now.duration_since(last) >= TYPING_SPELL) {
            log::info!("device {} typed into terminal {terminal}", link.device);
        }
    }

    /// Answers the connections waiting for windows to answer their asks,
    /// where they have, or they've waited too long.
    fn settle_pairings(&mut self, now: Instant) {
        let Some(web) = self.web.as_mut() else {
            return;
        };
        let mut settled = Vec::new();
        for token in web.conns.clone() {
            if let Some(Source::Web(conn)) = self.sources.get_mut(token.0)
                && let Some(response) = web.settle(conn, now)
            {
                conn.respond(&response, false, now);
                settled.push(token);
            }
        }
        for token in settled {
            self.answer_web(token, now);
        }
    }

    /// Tells every window of the devices asking to pair, if that changed.
    fn send_pairing(&mut self) {
        let Some(web) = self.web.as_mut() else {
            return;
        };
        if !std::mem::take(&mut web.asks_changed) {
            return;
        }
        let event = ServerMessage::Event(Event::Pairing(web.pairing()));
        let mut event = Broadcast::new(&event, "the devices asking to pair");
        let mut told = Vec::new();
        for (key, source) in &mut self.sources {
            if let Source::Client(client) = source
                && client.watching_workspaces
                && !client.finished
                && !matches!(client.link, Link::Web(_))
                && let Some(bytes) = event.to(client.wire)
            {
                client.outgoing.extend_from_slice(bytes);
                told.push(Token(key));
            }
        }
        for token in told {
            self.flush_client(token);
        }
    }

    /// Queues the devices asking for the client at `token`, if any are and
    /// it's a window: it has just started watching the workspaces.
    pub(super) fn tell_of_asks(&mut self, token: Token) {
        let Some(pairing) = (self.web.as_ref()).map(Web::pairing) else {
            return;
        };
        if pairing.asking.is_empty() && !pairing.locked {
            return;
        }
        if let Some(Source::Client(client)) = self.sources.get_mut(token.0)
            && !matches!(client.link, Link::Web(_))
            && let Ok(bytes) =
                frame::encode(client.wire, &ServerMessage::Event(Event::Pairing(pairing)))
        {
            client.later.extend_from_slice(&bytes);
        }
    }

    /// Answers [`Request::AnswerPairing`] from the client at `token`.
    ///
    /// [`Request::AnswerPairing`]: clackatoo_protocol::Request::AnswerPairing
    pub(super) fn answer_pairing(
        &mut self,
        token: Token,
        code: Option<&str>,
    ) -> Result<Response, Error> {
        if let Some(Source::Client(client)) = self.sources.get(token.0)
            && matches!(client.link, Link::Web(_))
        {
            return Err(Error::Pairing(
                "devices are allowed from a window, not from another device".into(),
            ));
        }
        let web = (self.web.as_mut()).ok_or_else(|| Error::Pairing("the web app is off".into()))?;
        let said = web.answer_asks(code).map_err(Error::Pairing)?;
        self.settle_pairings(Instant::now());
        Ok(Response::Text(said))
    }

    /// Answers [`Request::UnlockPairing`] from the client at `token`.
    ///
    /// [`Request::UnlockPairing`]: clackatoo_protocol::Request::UnlockPairing
    pub(super) fn unlock_pairing(&mut self, token: Token) -> Result<Response, Error> {
        if let Some(Source::Client(client)) = self.sources.get(token.0)
            && matches!(client.link, Link::Web(_))
        {
            return Err(Error::Pairing(
                "pairing is unlocked from a window, not from a device".into(),
            ));
        }
        let web = (self.web.as_mut()).ok_or_else(|| Error::Pairing("the web app is off".into()))?;
        web.unlock().map(Response::Text).map_err(Error::Pairing)
    }

    /// Answers [`Request::Devices`](clackatoo_protocol::Request::Devices).
    pub(super) fn devices(&self) -> Response {
        let Some(web) = self.web.as_ref() else {
            return Response::Devices(Vec::new());
        };
        let connected: BTreeSet<u64> = (self.sources.iter())
            .filter_map(|(_, source)| match source {
                Source::Client(Client {
                    link: Link::Web(link),
                    ..
                }) => Some(link.device),
                _ => None,
            })
            .collect();
        Response::Devices(
            (web.devices.list.iter())
                .map(|device| Device {
                    id: device.id,
                    name: device.name.clone(),
                    paired: device.paired,
                    used: device.used,
                    login: device.login.clone(),
                    notifications: device.push.is_some(),
                    connected: connected.contains(&device.id),
                })
                .collect(),
        )
    }

    /// Unpairs a device, cutting it off if it's connected.
    pub(super) fn revoke_device(&mut self, device: u64) -> Result<Response, Error> {
        let web = (self.web.as_mut()).ok_or(Error::NoSuchDevice(device))?;
        if !web.devices.remove(device) {
            return Err(Error::NoSuchDevice(device));
        }
        log::info!("device {device} unpaired");
        self.cut_off_device(device, 4001, "this device was unpaired");
        Ok(Response::Done)
    }

    /// Closes every connection `device` has, telling it why.
    fn cut_off_device(&mut self, device: u64, status: u16, why: &str) {
        let tokens: Vec<Token> = (self.sources.iter_mut())
            .filter_map(|(key, source)| match source {
                Source::Client(Client {
                    link: Link::Web(link),
                    finished,
                    ..
                }) if link.device == device => {
                    link.close(status, why);
                    *finished = true;
                    Some(Token(key))
                }
                _ => None,
            })
            .collect();
        for token in tokens {
            self.flush_client(token);
        }
    }

    /// Closes every device's connection, telling it why: the daemon is
    /// about to stop, or become another build. Each is written what it can
    /// take at once, and no more is waited for: a phone reconnects anyway.
    pub(super) fn close_web(&mut self, why: &str) {
        let tokens: Vec<Token> = (self.sources.iter_mut())
            .filter_map(|(key, source)| match source {
                Source::Client(Client {
                    link: Link::Web(link),
                    ..
                }) => {
                    link.close(1012, why);
                    Some(Token(key))
                }
                _ => None,
            })
            .collect();
        for token in tokens {
            if let Some(Source::Client(Client {
                link: Link::Web(link),
                outgoing,
                ..
            })) = self.sources.get_mut(token.0)
            {
                let _ = link.send(self.poll.registry(), token, outgoing);
            }
        }
    }

    /// Writes the devices' last changes before an upgrade, and tells the
    /// devices connected why they're cut off. The web app stops as the
    /// daemon is dropped, before the next build starts: it listens again,
    /// and takes the devices' file and lock.
    ///
    /// # Errors
    ///
    /// If the devices can't be written in time.
    pub(super) fn prepare_web_for_upgrade(&mut self) -> Result<(), String> {
        let Some(web) = &self.web else {
            return Ok(());
        };
        web.devices.flush(web::FLUSH_WAIT)?;
        self.close_web("the daemon is upgrading to a new build");
        Ok(())
    }

    // --- Chats, as a list -------------------------------------------------

    /// Answers [`Request::WatchChats`](clackatoo_protocol::Request::WatchChats).
    pub(super) fn watch_chats(&mut self, token: Token) -> Response {
        if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
            client.watching_chats = true;
        }
        // The next change goes to every watcher, whatever was sent before.
        self.chat_watch.sent = None;
        // Those lent to another machine's daemon are its alone.
        let mut chats = self.chats_for(token, &self.chat_summaries());
        // A client from before federation is shown only this daemon's.
        if let Some(Source::Client(client)) = self.sources.get(token.0)
            && client.wire < wire::FEDERATION
        {
            chats.retain(|chat| self.chat_ref(chat.chat).is_some());
        }
        Response::Chats(chats)
    }

    /// Every chat, summed up, in the order they were made: this daemon's,
    /// then those of other machines whose workspaces it shows.
    fn chat_summaries(&self) -> Vec<ChatSummary> {
        (self.terminals.keys())
            .filter_map(|&id| self.chat_summary(id))
            .chain(self.relayed_chats().cloned())
            .collect()
    }

    /// The chat `id`, summed up.
    pub(super) fn chat_summary(&self, id: TerminalId) -> Option<ChatSummary> {
        let chat = self.chat_ref(id)?;
        let node = chat.node();
        let active = node.turns.last().map_or(0, |turn| {
            turn.end
                .as_ref()
                .map_or(turn.sent, |end| end.at.max(turn.sent))
        });
        Some(ChatSummary {
            chat: id,
            title: chat.info().title,
            agent: node.agent.clone(),
            state: node.state.clone(),
            asking: node.question.is_some(),
            unseen: self.chat_watch.unseen.contains(&id),
            parent: chat.relations().parent,
            workspace: self.workspace_of_chat(id),
            active,
        })
    }

    /// The workspace chat `id` is in: its pane's, or that of the nearest
    /// chat above it in one.
    pub(super) fn workspace_of_chat(&self, mut id: TerminalId) -> Option<WorkspaceId> {
        let workspaces = self.workspaces.get();
        // Chats are started at most a few deep: this only guards a loop.
        for _ in 0..16 {
            if let Some(pane) = workspaces.pane_showing(id) {
                return workspaces
                    .find_pane(pane)
                    .map(|(workspace, ..)| workspace.id);
            }
            id = self.chat_ref(id)?.relations().parent?;
        }
        None
    }

    /// Looks at chat `id` after it changed: notes for the list what it is
    /// now, and if it has just finished a turn, or asked something, while
    /// no one present was looking, notes it as unseen, and a notification
    /// to send.
    pub(super) fn glance(&mut self, id: TerminalId) {
        let Some(summary) = self.chat_summary(id) else {
            return;
        };
        let Some(chat) = self.chat_ref(id) else {
            return;
        };
        let node = chat.node();
        let question = node.question.as_ref().map(|question| question.id);
        let answering = node.state == ChatState::Answering;
        let ended = (node.turns.last())
            .and_then(|turn| turn.end.as_ref())
            .map(|end| &end.outcome);
        let previous = self.chat_watch.glances.get(&id);
        let asked = question.is_some() && question != previous.and_then(|glance| glance.question);
        // A chat another chat started tells that chat, not the user.
        let finished = summary.parent.is_none()
            && previous.is_some_and(|glance| glance.answering)
            && !answering
            && matches!(ended, Some(Outcome::Done | Outcome::Failed(_)));
        let changed = previous.is_none_or(|glance| glance.summary != summary);
        self.chat_watch.glances.insert(
            id,
            Glance {
                question,
                answering,
                summary,
            },
        );
        self.chat_watch.dirty |= changed;
        if !(asked || finished) || self.looked_at(id) {
            return;
        }
        if finished && self.chat_watch.unseen.insert(id) {
            self.chat_watch.dirty = true;
            if let Some(glance) = self.chat_watch.glances.get_mut(&id) {
                glance.summary.unseen = true;
            }
        }
        if self
            .web
            .as_ref()
            .is_some_and(|web| (web.devices.list.iter()).any(|device| device.push.is_some()))
        {
            let at = Instant::now() + NOTIFY_AFTER;
            self.chat_watch.due.retain(|notice| notice.chat != id);
            self.chat_watch.due.push(Notice {
                at,
                chat: id,
                asked,
            });
        }
    }

    /// Forgets chat `id`, which closed.
    pub(super) fn forget_glance(&mut self, id: TerminalId) {
        self.chat_watch.glances.remove(&id);
        self.chat_watch.unseen.remove(&id);
        self.chat_watch.due.retain(|notice| notice.chat != id);
        self.chat_watch.dirty = true;
    }

    /// Notes that the chats may have moved between workspaces.
    pub(super) fn chats_moved(&mut self) {
        self.chat_watch.dirty = true;
    }

    /// Whether someone present is looking at chat `id`: a client whose
    /// user is there follows it.
    fn looked_at(&self, id: TerminalId) -> bool {
        let now = Instant::now();
        self.terminals.get(&id).is_some_and(|slot| {
            slot.viewers.iter().any(|token| {
                matches!(self.sources.get(token.0),
                    Some(Source::Client(client)) if client.present_until.is_some_and(|until| until > now))
            })
        })
    }

    /// Notes whether the user of the client at `token` is there, as it
    /// says; a client whose user is there has seen the chats it follows.
    pub(super) fn present(&mut self, token: Token, here: bool) {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return;
        };
        client.present_until = here.then(|| Instant::now() + PRESENT_FOR);
        if here {
            let followed: Vec<TerminalId> = client.views.keys().copied().collect();
            for id in followed {
                self.seen(id);
            }
        }
    }

    /// Notes that the client at `token` started following `id`, which it
    /// has seen if its user is there.
    pub(super) fn attached_to_chat(&mut self, token: Token, id: TerminalId) {
        let present = matches!(self.sources.get(token.0),
            Some(Source::Client(client)) if client.present_until.is_some_and(|until| until > Instant::now()));
        if present {
            self.seen(id);
        }
    }

    /// Notes that chat `id` has been seen.
    fn seen(&mut self, id: TerminalId) {
        if self.chat_watch.unseen.remove(&id) {
            self.chat_watch.dirty = true;
            if let Some(glance) = self.chat_watch.glances.get_mut(&id) {
                glance.summary.unseen = false;
            }
        }
        self.chat_watch.due.retain(|notice| notice.chat != id);
    }

    /// Sends the notifications due by `now`, of chats no one has looked at
    /// meanwhile, that still have something to say.
    fn notify_due(&mut self, now: Instant) {
        let (due, waiting): (Vec<Notice>, Vec<Notice>) =
            (std::mem::take(&mut self.chat_watch.due).into_iter()).partition(|n| n.at <= now);
        self.chat_watch.due = waiting;
        for notice in due {
            // One lent to another machine's daemon is said there, if
            // anywhere.
            if self.looked_at(notice.chat) || self.terminal_lent_to(notice.chat).is_some() {
                continue;
            }
            let Some(chat) = self.chat_ref(notice.chat) else {
                continue;
            };
            let node = chat.node();
            let body = if notice.asked {
                if node.question.is_none() {
                    continue;
                }
                format!("{} asks you something", node.agent)
            } else {
                match node.turns.last().and_then(|turn| turn.end.as_ref()) {
                    _ if node.state == ChatState::Answering => continue,
                    Some(end) if matches!(end.outcome, Outcome::Failed(_)) => {
                        format!("{} stopped: something went wrong", node.agent)
                    }
                    _ => format!("{} finished", node.agent),
                }
            };
            let payload = web::push_payload(notice.chat.0, &chat.info().title, &body);
            if let Some(web) = &self.web {
                web.notify(&payload, &format!("chat-{}", notice.chat), notice.asked);
            }
        }
    }

    /// Tells the clients watching the chats what they are now, if that
    /// changed.
    fn send_chat_summaries(&mut self) {
        if !std::mem::take(&mut self.chat_watch.dirty) {
            return;
        }
        let watching = (self.sources.iter())
            .any(|(_, source)| matches!(source, Source::Client(client) if client.watching_chats));
        if !watching {
            self.chat_watch.sent = None;
            return;
        }
        // As when a split is dragged: the workspaces changed, the chats
        // didn't.
        let chats = self.chat_summaries();
        if self.chat_watch.sent.as_ref() == Some(&chats) {
            return;
        }
        self.chat_watch.sent = Some(chats.clone());
        // Those lent to another machine's daemon go to it alone.
        let lent: HashMap<TerminalId, MachineId> = (chats.iter())
            .filter_map(|chat| Some((chat.chat, self.terminal_lent_to(chat.chat)?)))
            .collect();
        // A client from before federation is shown only this daemon's own,
        // as it's shown only its workspaces.
        let own = (chats.iter())
            .filter(|chat| self.chat_ref(chat.chat).is_some() && !lent.contains_key(&chat.chat))
            .cloned()
            .collect();
        let shown: Vec<ChatSummary> = (chats.iter())
            .filter(|chat| !lent.contains_key(&chat.chat))
            .cloned()
            .collect();
        let event = ServerMessage::Event(Event::Chats(shown));
        let mut event = Broadcast::new(&event, "the chats");
        let older = ServerMessage::Event(Event::Chats(own));
        let mut older = Broadcast::new(&older, "the chats");
        let mut told = Vec::new();
        for (key, source) in &mut self.sources {
            let Source::Client(client) = source else {
                continue;
            };
            if !client.watching_chats || client.finished {
                continue;
            }
            let borrower = client.machine.as_ref().map(|machine| machine.id);
            let theirs = borrower.is_some_and(|machine| lent.values().any(|&to| to == machine));
            if theirs && client.wire >= wire::FEDERATION {
                let with: Vec<ChatSummary> = (chats.iter())
                    .filter(|chat| lent.get(&chat.chat).is_none_or(|&to| Some(to) == borrower))
                    .cloned()
                    .collect();
                match frame::encode(client.wire, &ServerMessage::Event(Event::Chats(with))) {
                    Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
                    Err(err) => log::error!("encoding the chats failed: {err}"),
                }
                told.push(Token(key));
            } else if let Some(bytes) = if client.wire < wire::FEDERATION {
                older.to(client.wire)
            } else {
                event.to(client.wire)
            } {
                client.outgoing.extend_from_slice(bytes);
                told.push(Token(key));
            }
        }
        for token in told {
            self.flush_client(token);
        }
    }
}
