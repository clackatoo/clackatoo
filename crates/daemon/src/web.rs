//! The web app: what a phone, or any browser, uses to reach the daemon,
//! served by the daemon itself, as `web.toml` says (see [`config`]).
//!
//! It's an HTTP server on the event loop, serving the app's files, which
//! are built into the `clackatoo` program, and a WebSocket carrying the
//! protocol, so a phone is a client like any window, through the same
//! requests, with the same checks. Nothing slow runs on the loop:
//! passwords are checked, devices written, and notifications sent, on
//! threads of their own.
//!
//! # Pairing
//!
//! A device pairs once: `POST /pair` with the password, or asking to be
//! allowed from a window. The daemon then gives it a token, in a cookie
//! its scripts can't read, which it carries from then on, to `/session`,
//! `/push` and the WebSocket. A device asking shows a four-digit code,
//! and waits (`POST /pair/wait`) while every window shows the question;
//! typing the code into a window, with `:Devices allow CODE`, allows it.
//! The code is never shown on the daemon's side, so a device is allowed
//! only by someone who can see it.
//!
//! See [`auth`] for what's checked, and why.

pub mod config;

mod auth;
mod conn;
mod devices;
mod http;
mod push;
mod socket;

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mio::Waker;
use mio::net::{TcpListener, UnixListener};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::json;

pub use auth::hash_password;
pub use config::{WebConfig, WebMode};

pub(crate) use auth::Refused;
pub(crate) use conn::{Filled, HttpConn, Peer, Read, State, WebLink};
pub(crate) use http::{Method, Request, Response};
pub(crate) use push::payload as push_payload;
pub(crate) use socket::{Stream, Transport};

use push::{Gone, Push};

use auth::{Bucket, Buckets};
use devices::{Devices, Subscription, millis};
use push::{Pusher, Vapid};

/// How long a device asking to pair waits for an answer.
pub(crate) const ASK_WAIT: Duration = Duration::from_mins(2);

/// The most devices that may be asking at once.
const MOST_ASKING: usize = 4;

/// How many devices may ask in a burst from one address, and how often
/// one more may once they're used up: a scanner can't keep every window
/// asking.
const ASKS: u32 = 4;
const ASK_EVERY: Duration = Duration::from_mins(2);

/// The most passwords waiting to be checked, one after another: past it,
/// a try is turned away to come back in a moment.
const MOST_CHECKS_WAITING: usize = 8;

/// The most a device's name may be, in characters.
const MOST_NAME: usize = 40;

/// The longest the daemon waits, as it upgrades or stops, for the
/// devices' last changes to be written.
pub(crate) const FLUSH_WAIT: Duration = Duration::from_secs(5);

/// One of the app's files, built into the program.
#[derive(Clone, Copy, Debug)]
pub struct Asset {
    /// Where it's served: `/index.html`. `/` serves `/index.html`.
    pub path: &'static str,
    /// Its media type, as `Content-Type` says it.
    pub content_type: &'static str,
    /// What it holds.
    pub body: &'static [u8],
    /// The same, gzipped, if that's smaller.
    pub gzip: Option<&'static [u8]>,
    /// What tells this version of it from any other, for caching.
    pub etag: &'static str,
}

/// What's served for the app when the program has none built in.
const NO_APP: &str = "<!doctype html><meta charset=utf-8>\
    <meta name=viewport content=\"width=device-width\"><title>clackatoo</title>\
    <p>This build of clackatoo was made without its web app. Build it with \
    wasm-bindgen-cli installed, as the docs say, and upgrade the daemon onto it.";

/// Where the web app listens.
#[derive(Debug)]
pub(crate) enum Listener {
    /// On the network, for public.
    Tcp(TcpListener),
    /// On a unix socket kept to the user, for `tailscale
    /// serve`, at this path, which is taken away when it closes.
    Unix(UnixListener, PathBuf),
}

impl Listener {
    /// Takes a connection waiting, if one is.
    ///
    /// # Errors
    ///
    /// If none is waiting (`WouldBlock`), or taking it failed.
    pub(crate) fn accept(&self) -> io::Result<(Stream, Peer)> {
        match self {
            Self::Tcp(listener) => {
                let (stream, address) = listener.accept()?;
                let _ = stream.set_nodelay(true);
                Ok((Stream::Tcp(stream), Peer::Tcp(address)))
            }
            Self::Unix(listener, _) => {
                let (stream, _) = listener.accept()?;
                Ok((Stream::Unix(stream), Peer::Local))
            }
        }
    }

    /// The socket, to register with the event loop.
    pub(crate) fn source(&mut self) -> &mut dyn mio::event::Source {
        match self {
            Self::Tcp(listener) => listener,
            Self::Unix(listener, _) => listener,
        }
    }

    /// Where it listens, to say.
    fn describe(&self) -> String {
        match self {
            Self::Tcp(listener) => listener
                .local_addr()
                .map_or_else(|_| "the network".to_owned(), |address| address.to_string()),
            Self::Unix(_, path) => path.display().to_string(),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Self::Unix(_, path) = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A device asking to pair.
#[derive(Debug)]
pub(crate) struct Ask {
    /// What the device waits on: secret, so only it can collect the
    /// answer.
    wait: [u8; 16],
    /// The code it shows, which allows it.
    code: String,
    name: String,
    /// Where it asks from, to show.
    from: String,
    /// Who Tailscale said it was.
    login: Option<String>,
    until: Instant,
    until_ms: u64,
    /// Allowed or turned down, once a window has said.
    answer: Option<bool>,
}

/// What a password being checked is for.
#[derive(Debug)]
struct Checking {
    /// The connection waiting for it.
    token: mio::Token,
    /// Its number, which the connection says it waits for: if that
    /// connection has gone, and another has its token, the answer isn't
    /// that one's.
    number: u64,
    /// The name of the device it pairs, or the device whose password it
    /// gives again.
    pairing: Pairing,
}

/// What giving the password does.
#[derive(Debug)]
enum Pairing {
    /// Pairs a new device, with this name, as this Tailscale login.
    New { name: String, login: Option<String> },
    /// Gives this device's password again, as it's asked for.
    Renew(u64),
}

/// The thread checking passwords, one after another.
#[derive(Debug)]
struct Checker {
    /// Each password to check, numbered, with the hash it's checked
    /// against.
    jobs: Sender<(u64, String, String)>,
    /// Whether each matched, by its number.
    results: Receiver<(u64, bool)>,
}

impl Checker {
    /// Starts the thread, which calls `wake` with each answer.
    fn start(wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        let (jobs, inbox) = mpsc::channel::<(u64, String, String)>();
        let (tell, results) = mpsc::channel();
        std::thread::Builder::new()
            .name("password".to_owned())
            .spawn(move || {
                for (number, password, hash) in inbox {
                    let _ = tell.send((number, auth::password_matches(&password, &hash)));
                    wake();
                }
            })
            .map_err(|err| format!("the thread checking passwords couldn't start: {err}"))?;
        Ok(Self { jobs, results })
    }
}

/// What came of a request, for the event loop to do.
#[derive(Debug)]
pub(crate) enum Handled {
    /// Answer it, and read the next.
    Respond(Response),
    /// Answer it, and close the connection.
    Close(Response),
    /// Answer it, and make the connection a client, the device's.
    Upgrade { device: u64, response: Response },
    /// Wait: the connection's state says for what.
    Wait,
}

/// The web app's state, on the event loop.
#[derive(Debug)]
pub(crate) struct Web {
    pub(crate) config: WebConfig,
    pub(crate) listener: Listener,
    pub(crate) tls: Option<Arc<rustls::ServerConfig>>,
    /// When the certificate's files were last changed, as last looked at.
    certificate_changed: Option<(SystemTime, SystemTime)>,
    /// When they're next looked at.
    reload_at: Instant,
    random: SystemRandom,
    pub(crate) devices: Devices,
    vapid: Arc<Vapid>,
    pusher: Pusher,
    pub(crate) asks: Vec<Ask>,
    /// The devices asking changed since the windows were told.
    pub(crate) asks_changed: bool,
    tries: Buckets,
    /// Tries from everywhere at once.
    all_tries: Bucket,
    /// Wrong passwords in a row, which lock pairing with the password once
    /// there are enough.
    wrong: u32,
    ask_tries: Buckets,
    checker: Checker,
    /// The passwords being checked, oldest first.
    checking: Vec<Checking>,
    /// How many passwords have been checked, which numbers them.
    checks: u64,
    /// The connections, HTTP and WebSocket, by their tokens.
    pub(crate) conns: Vec<mio::Token>,
    /// When something next needs doing, at the latest.
    pub(crate) next_due: Option<Instant>,
}

impl Web {
    /// Starts the web app as `config` says, listening, with `waker` to
    /// wake the event loop when a thread has done something.
    ///
    /// # Errors
    ///
    /// If it can't listen, or its directory or certificate can't be read.
    pub(crate) fn start(config: WebConfig, waker: &Arc<Waker>) -> Result<Self, String> {
        let (tls, certificate_changed) = match &config.mode {
            WebMode::Tailscale => (None, None),
            WebMode::Public { certificate, key } => (
                Some(tls_config(certificate, key)?),
                changed_at(certificate, key),
            ),
        };
        // Taken first: it says the directory, and so the socket, is ours.
        let devices = Devices::open(&config.data)?;
        let random = SystemRandom::new();
        let vapid = Arc::new(Vapid::open(&config.data, &random)?);
        let listener = listen(&config)?;
        let wake = |waker: &Arc<Waker>| {
            let waker = Arc::clone(waker);
            move || {
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for the web app failed: {err}");
                }
            }
        };
        let pusher = Pusher::start(&vapid, wake(waker));
        let checker = Checker::start(wake(waker))?;
        let reload_at = Instant::now() + config.reload_every;
        Ok(Self {
            config,
            listener,
            tls,
            certificate_changed,
            reload_at,
            random,
            devices,
            vapid,
            pusher,
            asks: Vec::new(),
            asks_changed: false,
            tries: Buckets::new(auth::TRIES, auth::TRY_EVERY),
            all_tries: Bucket::new(auth::ALL_TRIES, auth::ALL_TRY_EVERY, Instant::now()),
            wrong: 0,
            ask_tries: Buckets::new(ASKS, ASK_EVERY),
            checker,
            checking: Vec::new(),
            checks: 0,
            conns: Vec::new(),
            next_due: None,
        })
    }

    /// Where it listens, if on the network.
    pub(crate) fn address(&self) -> Option<SocketAddr> {
        match &self.listener {
            Listener::Tcp(listener) => listener.local_addr().ok(),
            Listener::Unix(..) => None,
        }
    }

    /// Where it listens, to say.
    pub(crate) fn describe(&self) -> String {
        self.listener.describe()
    }

    /// Makes sure something is done by `at`.
    pub(crate) fn due_by(&mut self, at: Instant) {
        self.next_due = Some(self.next_due.map_or(at, |due| due.min(at)));
    }

    /// When each ask runs out.
    pub(crate) fn asks_until(&self) -> impl Iterator<Item = Instant> + '_ {
        self.asks.iter().map(|ask| ask.until)
    }

    /// How pairing stands, as windows show it: the asks waiting for an
    /// answer, and whether the password is locked.
    pub(crate) fn pairing(&self) -> clackatoo_protocol::PairingState {
        clackatoo_protocol::PairingState {
            asking: (self.asks.iter())
                .filter(|ask| ask.answer.is_none())
                .map(|ask| clackatoo_protocol::PairingAsk {
                    name: ask.name.clone(),
                    from: ask.from.clone(),
                    until: ask.until_ms,
                })
                .collect(),
            locked: self.devices.locked(),
        }
    }

    /// Lets devices pair with the password again, after too many wrong
    /// ones locked it. Returns what was done.
    ///
    /// # Errors
    ///
    /// If it isn't locked.
    pub(crate) fn unlock(&mut self) -> Result<String, String> {
        if !self.devices.locked() {
            return Err("pairing with the password isn't locked".into());
        }
        self.devices.set_locked(false);
        self.wrong = 0;
        self.asks_changed = true;
        log::info!("pairing with the password unlocked from a window");
        Ok("Devices can pair with the password again".into())
    }

    /// Answers the devices asking: allows the one showing `code`, or with
    /// none, turns them all down. Returns what was done.
    ///
    /// # Errors
    ///
    /// If no device asking shows `code`.
    pub(crate) fn answer_asks(&mut self, code: Option<&str>) -> Result<String, String> {
        let open = |ask: &&mut Ask| ask.answer.is_none();
        let Some(code) = code.map(str::trim) else {
            let mut turned_down = 0;
            for ask in self.asks.iter_mut().filter(|ask| ask.answer.is_none()) {
                ask.answer = Some(false);
                turned_down += 1;
            }
            self.asks_changed = true;
            return match turned_down {
                0 => Err("no device is asking to pair".into()),
                1 => Ok("Turned down the device asking to pair".into()),
                n => Ok(format!("Turned down the {n} devices asking to pair")),
            };
        };
        let ask = (self.asks.iter_mut().filter(open))
            .find(|ask| ask.code == code)
            .ok_or_else(|| format!("no device asking to pair shows {code}"))?;
        ask.answer = Some(true);
        self.asks_changed = true;
        log::info!("{} ({}) allowed to pair from a window", ask.name, ask.from);
        Ok(format!("Allowed {} to pair", ask.name))
    }

    /// Answers `request`, from `conn`, at `token`, at `now`.
    pub(crate) fn handle(
        &mut self,
        conn: &mut HttpConn,
        token: mio::Token,
        request: &Request,
        now: Instant,
    ) -> Handled {
        let login = if self.config.tailscale() {
            match auth::tailscale_login(conn.peer, request.header("tailscale-user-login")) {
                Ok(login) if self.login_allowed(&login) => Some(login),
                Ok(_) => return Handled::Close(refused(Refused::OtherLogin)),
                Err(refusal) => return Handled::Close(refused(refusal)),
            }
        } else {
            None
        };
        if !auth::host_allowed(
            self.config.origin.as_deref(),
            request.header("host"),
            self.forwarded_host(request),
        ) {
            log::warn!("refused a request for another name, from {}", conn.peer);
            return Handled::Close(Response::error(421, "this isn't the app's name"));
        }
        match (request.method, request.path.as_str()) {
            (Method::Get, "/ws") => self.open_socket(conn, request, login.as_deref(), now),
            (Method::Get, "/session") => Handled::Respond(self.session(request, login.as_deref())),
            (Method::Post, path) if !self.is_from_app(request) => {
                log::warn!(
                    "refused {path} from a page that isn't the app's ({})",
                    conn.peer
                );
                Handled::Respond(refused(Refused::OtherOrigin))
            }
            (Method::Post, "/pair") => self.pair(conn, token, request, login, now),
            (Method::Post, "/pair/wait") => self.wait_for_answer(conn, request, now),
            (Method::Post, "/push") => Handled::Respond(self.subscribe(request, login.as_deref())),
            (Method::Get | Method::Head, path) => Handled::Respond(self.asset(request, path)),
            _ => Handled::Respond(Response::error(405, "the app doesn't do that")),
        }
    }

    /// The host `tailscale serve` says a request was sent to, trusted only
    /// from it.
    fn forwarded_host<'a>(&self, request: &'a Request) -> Option<&'a str> {
        if self.config.tailscale() {
            request.header("x-forwarded-host")
        } else {
            None
        }
    }

    /// Who a request comes from, for limiting tries: its address, or
    /// through Tailscale, the address `tailscale serve` says it came from,
    /// or else its login.
    ///
    /// `tailscale serve` (1.102, in `ipn/ipnlocal/serve.go`) proxies with
    /// Go's `ReverseProxy` and `Rewrite`, which takes away the client's
    /// `X-Forwarded-For` before it sets its own, the tailnet address the
    /// request came from: one address, which is the last, the hop it
    /// saw.
    fn who(conn: &HttpConn, request: &Request, login: Option<&str>) -> String {
        match conn.peer {
            Peer::Tcp(address) => auth::address_key(address.ip()),
            Peer::Local => {
                let forwarded = (request.header("x-forwarded-for"))
                    .and_then(|addresses| addresses.rsplit(',').next())
                    .map(str::trim)
                    .filter(|address| !address.is_empty());
                match forwarded {
                    Some(address) => {
                        (address.parse().ok()).map_or_else(|| address.to_owned(), auth::address_key)
                    }
                    None => login.unwrap_or("local").to_owned(),
                }
            }
        }
    }

    /// Whether `login`, whom Tailscale says a request is from, is let in:
    /// the one the settings name, or else the first device's, or anyone
    /// until a device has paired.
    fn login_allowed(&self, login: &str) -> bool {
        match &self.config.login {
            Some(allowed) => allowed == login,
            None => self.first_login().is_none_or(|first| first == login),
        }
    }

    /// The login of the first device paired through Tailscale, if any has.
    fn first_login(&self) -> Option<&str> {
        (self.devices.list.iter()).find_map(|device| device.login.as_deref())
    }

    /// Whether a request came from a page of the app's own origin.
    fn is_from_app(&self, request: &Request) -> bool {
        auth::origin_allowed(
            request.header("origin"),
            self.config.origin.as_deref(),
            request.header("host"),
            self.forwarded_host(request),
        )
    }

    /// Why the device with `id` isn't let in at `now`, in milliseconds
    /// since the Unix epoch, if it isn't: it's gone unused too long, or
    /// its password is to be given again.
    pub(crate) fn lapsed(&self, id: u64, now: u64) -> Option<Denied> {
        let Some(device) = self.devices.list.iter().find(|device| device.id == id) else {
            return Some(Denied::Unpaired);
        };
        if device.used.saturating_add(millis(self.config.expire_after)) < now {
            return Some(Denied::Unpaired);
        }
        if let (Some(every), Some(_)) = (self.config.password_every, &self.config.password) {
            let given = device.password.unwrap_or(device.paired);
            if given.saturating_add(millis(every)) < now {
                return Some(Denied::Renew);
            }
        }
        None
    }

    /// When a device next lapses, at the soonest: one of `connected`
    /// whose password is to be given again, or any unused too long.
    pub(crate) fn next_lapse(&self, connected: &[u64], now: Instant) -> Option<Instant> {
        let now_ms = epoch_millis();
        let at = |ms: u64| now + Duration::from_millis(ms.saturating_sub(now_ms));
        let expiring = (self.devices.list.iter())
            .map(|device| at(device.used.saturating_add(millis(self.config.expire_after)) + 1));
        let renewing = (self.config.password.as_ref())
            .and(self.config.password_every)
            .into_iter()
            .flat_map(|every| {
                (self.devices.list.iter())
                    .filter(|device| connected.contains(&device.id))
                    .map(move |device| {
                        let given = device.password.unwrap_or(device.paired);
                        at(given.saturating_add(millis(every)) + 1)
                    })
            });
        expiring.chain(renewing).min()
    }

    /// The device `request` carries the token of, if it's paired and let
    /// in now, as `login` through Tailscale.
    fn device(&mut self, request: &Request, login: Option<&str>) -> Result<u64, Denied> {
        let token = request.cookie(auth::COOKIE).ok_or(Denied::Unpaired)?;
        let device = (self.devices.by_token(&auth::token_hash(token))).ok_or(Denied::Unpaired)?;
        let id = device.id;
        if let (Some(paired), Some(login)) = (&device.login, login)
            && paired != login
        {
            return Err(Denied::Refused(Refused::OtherLogin));
        }
        match self.lapsed(id, epoch_millis()) {
            Some(Denied::Unpaired) => {
                log::info!("device {id} expired, unused");
                self.devices.remove(id);
                Err(Denied::Unpaired)
            }
            Some(denied) => Err(denied),
            None => Ok(id),
        }
    }

    /// Answers `GET /session`: whether the device is paired and let in,
    /// and if not, how it can be.
    fn session(&mut self, request: &Request, login: Option<&str>) -> Response {
        let state = match self.device(request, login) {
            Ok(id) => {
                let device = self.devices.list.iter().find(|device| device.id == id);
                return Response::json(
                    200,
                    &json!({
                        "state": "paired",
                        "name": device.map(|device| device.name.as_str()),
                        "key": self.vapid.public_key(),
                        "notifications": device.is_some_and(|device| device.push.is_some()),
                    }),
                );
            }
            Err(Denied::Unpaired) => "unpaired",
            Err(Denied::Renew) => "renew",
            Err(Denied::Refused(refusal)) => {
                return Response::json(
                    200,
                    &json!({ "state": "refused", "reason": refusal.reason() }),
                );
            }
        };
        Response::json(
            200,
            &json!({
                "state": state,
                "password": self.config.password.is_some() && !self.devices.locked(),
                "ask": self.config.ask,
                "tailscale": self.config.tailscale(),
            }),
        )
    }

    /// Answers `POST /pair`: the password, or an ask to be allowed from a
    /// window, or the password again, for a device asked for it.
    fn pair(
        &mut self,
        conn: &mut HttpConn,
        token: mio::Token,
        request: &Request,
        login: Option<String>,
        now: Instant,
    ) -> Handled {
        let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
            return Handled::Respond(Response::error(400, "pairing asks in JSON"));
        };
        let who = Self::who(conn, request, login.as_deref());
        let name = clean_name(body["name"].as_str().unwrap_or_default());
        let renew = body["renew"].as_bool().unwrap_or(false);
        let pairing = if renew {
            if let Err(Denied::Refused(refusal)) = self.device(request, login.as_deref()) {
                return Handled::Close(refused(refusal));
            }
            match self.renewing(request) {
                Some(id) => Pairing::Renew(id),
                None => return Handled::Respond(Response::error(403, "this device isn't paired")),
            }
        } else {
            if self.devices.list.len() >= devices::MOST {
                return Handled::Respond(Response::error(
                    409,
                    "too many devices are paired: unpair some with :Devices",
                ));
            }
            Pairing::New { name, login }
        };
        if let Some(password) = body["password"].as_str() {
            // Through Tailscale with no login named, the first device's
            // login becomes the only one let in: that's for a window to
            // allow, not for whoever guesses the password first.
            let first = self.config.tailscale()
                && self.config.login.is_none()
                && self.first_login().is_none();
            if first && matches!(pairing, Pairing::New { .. }) {
                return Handled::Respond(Response::error(
                    403,
                    "the first device is allowed from a window: ask a window, or name the \
                     login let in as login in web.toml",
                ));
            }
            return self.check_password(conn, token, password, pairing, &who, now);
        }
        match (body["ask"].as_bool(), pairing) {
            (Some(true), Pairing::New { name, login }) => self.ask(conn, name, login, &who, now),
            _ => Handled::Respond(Response::error(400, "pairing gives a password, or asks")),
        }
    }

    /// The device whose token `request` carries, if it has one, paired or
    /// not let in for now: to give its password again.
    fn renewing(&self, request: &Request) -> Option<u64> {
        let token = request.cookie(auth::COOKIE)?;
        Some(self.devices.by_token(&auth::token_hash(token))?.id)
    }

    /// Queues `password` to be checked, for `pairing`, on the checking
    /// thread, with `conn` waiting for the answer, if `who` may try now.
    fn check_password(
        &mut self,
        conn: &mut HttpConn,
        token: mio::Token,
        password: &str,
        pairing: Pairing,
        who: &str,
        now: Instant,
    ) -> Handled {
        let Some(hash) = self.config.password.clone() else {
            return Handled::Respond(Response::error(
                403,
                "no password is set: allow this device from a window instead",
            ));
        };
        if self.devices.locked() {
            return Handled::Respond(Response::error(
                423,
                "pairing with the password is locked after too many wrong ones: \
                 :Devices unlock in a window lets it again",
            ));
        }
        if self.checking.len() >= MOST_CHECKS_WAITING {
            return Handled::Respond(too_many(Duration::from_secs(1)));
        }
        if let Err(wait) = self.tries.take(who, now) {
            log::warn!("a password try from {who} refused: too many tries");
            return Handled::Respond(too_many(wait));
        }
        // However many addresses they come from.
        if let Err(wait) = self.all_tries.take(now) {
            log::warn!("a password try from {who} refused: too many tries from everywhere");
            return Handled::Respond(too_many(wait));
        }
        self.checks += 1;
        let number = self.checks;
        if (self.checker.jobs.send((number, password.to_owned(), hash))).is_err() {
            return Handled::Respond(Response::error(503, "the password can't be checked now"));
        }
        self.checking.push(Checking {
            token,
            number,
            pairing,
        });
        conn.state = State::Verifying(number);
        // As long as the checks before it could take, and more.
        conn.until = now + Duration::from_mins(1);
        self.due_by(conn.until);
        Handled::Wait
    }

    /// The connections waiting for passwords' checks done since last
    /// asked, and the answers for them, where `waiting` says the
    /// connection with the token waits for the check with the number
    /// still: one that has gone gets nothing, and pairs nothing.
    pub(crate) fn checked(
        &mut self,
        waiting: impl Fn(mio::Token, u64) -> bool,
    ) -> Vec<(mio::Token, Response)> {
        let mut answers = Vec::new();
        while let Ok((number, matches)) = self.checker.results.try_recv() {
            let Some(at) = self
                .checking
                .iter()
                .position(|check| check.number == number)
            else {
                continue;
            };
            let checking = self.checking.remove(at);
            if !waiting(checking.token, checking.number) {
                log::info!("a password was checked for a connection that has gone");
                continue;
            }
            if matches {
                self.wrong = 0;
            } else {
                self.wrong += 1;
                if self.wrong >= self.config.lock_after && !self.devices.locked() {
                    log::warn!(
                        "{} wrong passwords in a row: pairing with the password is locked",
                        self.wrong
                    );
                    self.devices.set_locked(true);
                    self.asks_changed = true;
                }
            }
            let response = if matches {
                match checking.pairing {
                    Pairing::New { name, login } => {
                        let response = self.paired(name.clone(), login, true);
                        log::info!("{name} paired with the password");
                        response
                    }
                    Pairing::Renew(id) => {
                        if let Some(device) = self.devices.get_mut(id) {
                            device.password = Some(epoch_millis());
                            log::info!("device {id} ({}) gave the password again", device.name);
                        }
                        self.devices.save();
                        Response::json(200, &json!({ "paired": true }))
                    }
                }
            } else {
                log::warn!("a wrong password was given for pairing");
                Response::error(403, "that isn't the password")
            };
            answers.push((checking.token, response));
        }
        answers
    }

    /// Pairs a device called `name`, as `login`, its password given if
    /// `password`: the answer giving it its token.
    fn paired(&mut self, name: String, login: Option<String>, password: bool) -> Response {
        let (token, hash) = auth::new_token(&self.random);
        let id = self
            .devices
            .add(name, hash, login, password, epoch_millis());
        Response::json(200, &json!({ "paired": true, "device": id }))
            .with("set-cookie", auth::set_cookie(&token))
    }

    /// Asks every window to allow a device called `name`, as `login`, for
    /// `who`.
    fn ask(
        &mut self,
        conn: &HttpConn,
        name: String,
        login: Option<String>,
        who: &str,
        now: Instant,
    ) -> Handled {
        if !self.config.ask {
            return Handled::Respond(Response::error(
                403,
                "devices can't ask to pair here: give the password",
            ));
        }
        self.drop_old_asks(now);
        if self.asks.len() >= MOST_ASKING {
            return Handled::Respond(too_many(ASK_WAIT));
        }
        if let Err(wait) = self.ask_tries.take(who, now) {
            return Handled::Respond(too_many(wait));
        }
        let mut wait = [0; 16];
        let mut code = [0; 2];
        if self.random.fill(&mut wait).is_err() {
            return Handled::Respond(Response::error(503, "no randomness to be had"));
        }
        let code = loop {
            if self.random.fill(&mut code).is_err() {
                return Handled::Respond(Response::error(503, "no randomness to be had"));
            }
            let code = format!("{:04}", u16::from_le_bytes(code) % 10_000);
            if !self.asks.iter().any(|ask| ask.code == code) {
                break code;
            }
        };
        let from = match (&login, conn.peer) {
            (Some(login), _) => login.clone(),
            (None, Peer::Tcp(address)) => address.ip().to_string(),
            (None, Peer::Local) => who.to_owned(),
        };
        let until = now + ASK_WAIT;
        let until_ms = epoch_millis() + millis(ASK_WAIT);
        log::info!("{name} ({from}) asks to pair");
        self.asks.push(Ask {
            wait,
            code: code.clone(),
            name,
            from,
            login,
            until,
            until_ms,
            answer: None,
        });
        self.asks_changed = true;
        self.due_by(until);
        Handled::Respond(Response::json(
            200,
            &json!({ "wait": hex(&wait), "code": code, "until": until_ms }),
        ))
    }

    /// Answers `POST /pair/wait`: waits for a window to answer the ask
    /// the request names.
    fn wait_for_answer(&mut self, conn: &mut HttpConn, request: &Request, now: Instant) -> Handled {
        let wait = serde_json::from_slice::<serde_json::Value>(&request.body)
            .ok()
            .and_then(|body| unhex16(body["wait"].as_str()?));
        let Some(wait) = wait else {
            return Handled::Respond(Response::error(400, "waiting names what it waits on"));
        };
        let found = self.asks.iter().find(|ask| same16(&ask.wait, &wait));
        let Some(until) = found.map(|ask| ask.until) else {
            return Handled::Respond(Response::error(403, "nobody answered: ask again"));
        };
        conn.state = State::Pairing(wait);
        conn.until = until;
        self.due_by(until);
        match self.settle(conn, now) {
            Some(response) => Handled::Respond(response),
            None => Handled::Wait,
        }
    }

    /// The answer for `conn`, waiting for a window to answer its ask, if
    /// one has, or it's waited too long.
    pub(crate) fn settle(&mut self, conn: &mut HttpConn, now: Instant) -> Option<Response> {
        let State::Pairing(wait) = conn.state else {
            return None;
        };
        let at = self.asks.iter().position(|ask| same16(&ask.wait, &wait));
        let answer = at.and_then(|at| self.asks[at].answer);
        let response = match (at, answer) {
            (Some(at), Some(true)) => {
                let ask = self.asks.remove(at);
                if self.devices.list.len() >= devices::MOST {
                    Response::error(
                        409,
                        "too many devices are paired: unpair some with :Devices",
                    )
                } else {
                    log::info!("{} paired, allowed from a window", ask.name);
                    self.paired(ask.name, ask.login, false)
                }
            }
            (Some(at), Some(false)) => {
                self.asks.remove(at);
                Response::error(403, "turned down")
            }
            (Some(at), None) if now < self.asks[at].until => return None,
            _ => Response::error(403, "nobody answered: ask again"),
        };
        conn.state = State::Reading;
        Some(response)
    }

    /// Lets go of asks nobody answered in time, or whose answer nobody
    /// collected.
    pub(crate) fn drop_old_asks(&mut self, now: Instant) {
        let before = self.asks.len();
        self.asks.retain(|ask| now < ask.until);
        if self.asks.len() != before {
            self.asks_changed = true;
        }
    }

    /// Answers `POST /push`: where the device's notifications go, or
    /// that they're to stop.
    fn subscribe(&mut self, request: &Request, login: Option<&str>) -> Response {
        let id = match self.device(request, login) {
            Ok(id) => id,
            Err(denied) => return denied.response(),
        };
        let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
            return Response::error(400, "a subscription is JSON");
        };
        let subscription = if body["endpoint"].is_null() {
            None
        } else {
            let b64 = |key: &str| {
                use base64::Engine as _;
                let text = body["keys"][key].as_str()?;
                (base64::engine::general_purpose::URL_SAFE_NO_PAD)
                    .decode(text.trim_end_matches('='))
                    .ok()
            };
            let endpoint = body["endpoint"].as_str().unwrap_or_default();
            match (push::audience(endpoint), b64("p256dh"), b64("auth")) {
                (Some(_), Some(p256dh), Some(auth)) if p256dh.len() == 65 && auth.len() == 16 => {
                    Some(Subscription {
                        endpoint: endpoint.to_owned(),
                        p256dh,
                        auth,
                    })
                }
                (None, ..) => {
                    return Response::error(400, "that isn't a browser's push service");
                }
                _ => return Response::error(400, "that subscription's keys aren't right"),
            }
        };
        if let Some(device) = self.devices.get_mut(id) {
            log::info!(
                "device {id} ({}) {} notifications",
                device.name,
                if subscription.is_some() {
                    "asked for"
                } else {
                    "stopped"
                }
            );
            device.push = subscription;
        }
        self.devices.save();
        Response::empty(204).with("cache-control", "no-store")
    }

    /// Answers `GET /ws`: opens a WebSocket for a paired device.
    fn open_socket(
        &mut self,
        conn: &HttpConn,
        request: &Request,
        login: Option<&str>,
        now: Instant,
    ) -> Handled {
        let upgrade =
            request.lists("upgrade", "websocket") && request.lists("connection", "upgrade");
        let key = request.header("sec-websocket-key");
        let (true, Some(key), Some("13")) = (upgrade, key, request.header("sec-websocket-version"))
        else {
            return Handled::Close(
                Response::error(426, "this is the WebSocket").with("sec-websocket-version", "13"),
            );
        };
        if !self.is_from_app(request) {
            log::warn!(
                "refused a WebSocket from a page that isn't the app's ({})",
                conn.peer
            );
            return Handled::Close(refused(Refused::OtherOrigin));
        }
        let device = match self.device(request, login) {
            Ok(device) => device,
            Err(denied) => return Handled::Close(denied.response()),
        };
        self.devices.used(device, epoch_millis());
        // It's cut off when it next lapses, if it's still connected.
        if let Some(lapse) = self.next_lapse(&[device], now) {
            self.due_by(lapse);
        }
        if let Some(found) = self.devices.list.iter().find(|found| found.id == device) {
            log::info!(
                "device {device} ({}) connected from {}",
                found.name,
                login.map_or_else(|| conn.peer.to_string(), str::to_owned)
            );
        }
        let mut response = Response::empty(101)
            .with("upgrade", "websocket")
            .with("connection", "Upgrade")
            .with("sec-websocket-accept", socket::accept_key(key));
        if request.lists("sec-websocket-protocol", "clackatoo") {
            response = response.with("sec-websocket-protocol", "clackatoo");
        }
        self.due_by(now + conn::PING_EVERY);
        Handled::Upgrade { device, response }
    }

    /// Answers a request for one of the app's files.
    fn asset(&self, request: &Request, path: &str) -> Response {
        let path = if path == "/" { "/index.html" } else { path };
        if self.config.assets.is_empty() && path == "/index.html" {
            return Response::new(200, "text/html; charset=utf-8", NO_APP.as_bytes())
                .with("cache-control", "no-store");
        }
        let Some(asset) = self.config.assets.iter().find(|asset| asset.path == path) else {
            return Response::error(404, "the app has no such file");
        };
        let quoted = format!("\"{}\"", asset.etag);
        if request.header("if-none-match") == Some(quoted.as_str()) {
            return Response::empty(304)
                .with("etag", quoted)
                .with("cache-control", "no-cache");
        }
        let response = Response::empty(200)
            .with("content-type", asset.content_type)
            .with("etag", quoted)
            // Asked again each time, answered 304 while it's the same: an
            // upgraded daemon's app is never stale.
            .with("cache-control", "no-cache")
            .with("vary", "accept-encoding");
        match asset.gzip {
            Some(gzip) if request.lists("accept-encoding", "gzip") => {
                response.with("content-encoding", "gzip").body(gzip)
            }
            _ => response.body(asset.body),
        }
    }

    /// Sends `payload`, about chat `topic`, to every device that asked for
    /// notifications, urgently if `urgent`.
    pub(crate) fn notify(&self, payload: &[u8], topic: &str, urgent: bool) {
        for device in &self.devices.list {
            if let Some(subscription) = &device.push {
                self.pusher.send(Push {
                    device: device.id,
                    subscription: subscription.clone(),
                    payload: payload.to_vec(),
                    urgent,
                    topic: topic.to_owned(),
                });
            }
        }
    }

    /// Drops the subscriptions push services have said are gone.
    pub(crate) fn drop_gone_subscriptions(&mut self) {
        let gone = self.pusher.gone();
        if gone.is_empty() {
            return;
        }
        for Gone { device, endpoint } in gone {
            if let Some(device) = self.devices.get_mut(device)
                && device
                    .push
                    .as_ref()
                    .is_some_and(|push| push.endpoint == endpoint)
            {
                log::info!(
                    "device {} ({}) no longer takes notifications",
                    device.id,
                    device.name
                );
                device.push = None;
            }
        }
        self.devices.save();
    }

    /// Unpairs the devices unused too long. Returns those unpaired.
    pub(crate) fn expire(&mut self) -> Vec<u64> {
        let before = epoch_millis().saturating_sub(millis(self.config.expire_after));
        self.devices.expire(before)
    }

    /// Reads the certificate again if its files have changed since they
    /// were last read, as when they're renewed, once `reload_every` has
    /// passed since it was last looked at. New connections get it.
    pub(crate) fn reload_certificate(&mut self, now: Instant) {
        if now < self.reload_at {
            return;
        }
        self.reload_at = now + self.config.reload_every;
        let WebMode::Public { certificate, key } = &self.config.mode else {
            return;
        };
        let changed = changed_at(certificate, key);
        if changed == self.certificate_changed {
            return;
        }
        match tls_config(certificate, key) {
            Ok(tls) => {
                log::info!("read the web app's certificate again, as it changed");
                self.tls = Some(tls);
                self.certificate_changed = changed;
            }
            // Perhaps half written: looked at again next time.
            Err(err) => log::warn!("the web app's certificate changed, but: {err}"),
        }
    }

    /// When the certificate is next looked at, in public.
    pub(crate) fn reload_due(&self) -> Option<Instant> {
        (!self.config.tailscale()).then_some(self.reload_at)
    }
}

/// Why a device isn't let in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Denied {
    /// It has no token, or one no device has, or it's gone unused too
    /// long.
    Unpaired,
    /// Its password must be given again.
    Renew,
    /// It isn't let in at all.
    Refused(Refused),
}

impl Denied {
    fn response(self) -> Response {
        match self {
            Self::Unpaired => Response::error(401, "this device isn't paired"),
            Self::Renew => Response::error(401, "enter the password again"),
            Self::Refused(refusal) => refused(refusal),
        }
    }

    /// What closing a device's connection for it says: the WebSocket
    /// status, and why.
    pub(crate) fn close(self) -> (u16, &'static str) {
        match self {
            Self::Unpaired => (4001, "this device is no longer paired"),
            Self::Renew => (4002, "enter the password again"),
            Self::Refused(refusal) => (4003, refusal.reason()),
        }
    }
}

/// The answer refusing a request, saying why.
fn refused(refusal: Refused) -> Response {
    Response::error(403, refusal.reason())
}

/// The answer to a try too soon, saying how long to wait.
fn too_many(wait: Duration) -> Response {
    Response::error(429, "too many tries: wait a little")
        .with("retry-after", wait.as_secs().max(1).to_string())
}

/// Whether `c` is a format character (Unicode's Cf), or a line or
/// paragraph separator: invisible, and some turn text around, so a name
/// could seem to say what it doesn't.
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{06DD}' | '\u{070F}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}'
        | '\u{110BD}' | '\u{1BCA0}'..='\u{1BCA3}' | '\u{1D173}'..='\u{1D17A}'
        | '\u{E0001}' | '\u{E0020}'..='\u{E007F}')
}

/// A device's name, as it asked to be called: trimmed, without control
/// or format characters, and short.
fn clean_name(name: &str) -> String {
    let name: String = (name.chars())
        .filter(|&c| !c.is_control() && !is_format(c))
        .take(MOST_NAME)
        .collect();
    match name.trim() {
        "" => "a device".to_owned(),
        name => name.to_owned(),
    }
}

/// Now, in milliseconds since the Unix epoch.
pub(crate) fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, millis)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn unhex16(text: &str) -> Option<[u8; 16]> {
    let mut bytes = [0; 16];
    if text.len() != 32 || !text.is_ascii() {
        return None;
    }
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

/// Whether two 16-byte secrets are the same, taking as long however much
/// matches.
fn same16(a: &[u8; 16], b: &[u8; 16]) -> bool {
    a.iter().zip(b).fold(0, |differ, (a, b)| differ | (a ^ b)) == 0
}

/// Listens as `config` says: on its unix socket for Tailscale, kept to the
/// user, in a directory only the user may write in, or on the network for
/// public.
fn listen(config: &WebConfig) -> Result<Listener, String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    if !config.tailscale() {
        let listener = TcpListener::bind(config.address)
            .map_err(|err| format!("can't listen on {}: {err}", config.address))?;
        return Ok(Listener::Tcp(listener));
    }
    let path = &config.socket;
    let parent = (path.parent())
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // A directory others can write in could have the socket swapped for
    // one of theirs, or theirs put in its place.
    let meta = std::fs::metadata(parent)
        .map_err(|err| format!("{} can't be looked at: {err}", parent.display()))?;
    if meta.uid() != rustix::process::getuid().as_raw() || meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{} isn't a directory only you can write in: the socket goes elsewhere",
            parent.display()
        ));
    }
    // Made in a directory only the user can enter, kept to the user, then
    // moved into place: never, even for a moment, open to anyone else.
    let staging = parent.join(format!(".web-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .map_err(|err| format!("{} couldn't be made: {err}", staging.display()))?;
    let staged = staging.join(config::SOCKET);
    let listened = UnixListener::bind(&staged)
        .map_err(|err| format!("can't listen on {}: {err}", path.display()))
        .and_then(|listener| {
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))
                .map_err(|err| format!("{} couldn't be kept to you: {err}", path.display()))?;
            // One left by a daemon that stopped is replaced: the devices'
            // lock, taken before this, says no daemon has it now.
            std::fs::rename(&staged, path)
                .map_err(|err| format!("{} couldn't be put in place: {err}", path.display()))?;
            Ok(listener)
        });
    let _ = std::fs::remove_dir_all(&staging);
    Ok(Listener::Unix(listened?, path.clone()))
}

/// When the certificate's files were last changed, if both can be
/// looked at.
fn changed_at(certificate: &Path, key: &Path) -> Option<(SystemTime, SystemTime)> {
    let changed = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    };
    Some((changed(certificate)?, changed(key)?))
}

/// The TLS config for the certificate chain in `certificate` and its key
/// in `key`, both PEM files.
fn tls_config(certificate: &Path, key: &Path) -> Result<Arc<rustls::ServerConfig>, String> {
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let chain = CertificateDer::pem_file_iter(certificate)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .map_err(|err| format!("{} isn't a certificate: {err}", certificate.display()))?;
    if chain.is_empty() {
        return Err(format!("{} holds no certificate", certificate.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|err| format!("{} isn't a private key: {err}", key.display()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| err.to_string())?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|err| format!("the certificate and key don't go together: {err}"))?;
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  jack's iPhone \n"), "jack's iPhone");
        assert_eq!(clean_name("\u{1b}[31mred"), "[31mred");
        assert_eq!(clean_name(""), "a device");
        assert_eq!(clean_name(&"x".repeat(100)).len(), MOST_NAME);
        // Turned around, "iPhone" would read as "enohPi" in a window.
        assert_eq!(clean_name("\u{202E}enohPi\u{202C}"), "enohPi");
        assert_eq!(clean_name("a\u{200B}b\u{2066}c\u{FEFF}"), "abc");
        assert_eq!(clean_name("\u{2069}\u{200D}"), "a device");
    }

    #[test]
    fn waits_are_secret_and_compared_whole() {
        let wait = [7; 16];
        assert_eq!(unhex16(&hex(&wait)), Some(wait));
        assert!(unhex16("zz").is_none());
        assert!(same16(&wait, &[7; 16]));
        assert!(!same16(&wait, &[8; 16]));
    }
}
