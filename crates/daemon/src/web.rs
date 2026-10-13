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

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mio::Waker;
use mio::net::TcpListener;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::json;

pub use auth::hash_password;
pub use config::{WebConfig, WebMode};

pub(crate) use auth::Refused;
pub(crate) use conn::{HttpConn, REQUEST_WAIT, Read, State, WebLink};
pub(crate) use http::{Method, Request, Response};
pub(crate) use push::payload as push_payload;
pub(crate) use socket::Transport;

use push::{Gone, Push};

use auth::Bucket;
use devices::{Devices, Subscription, millis};
use push::{Pusher, Vapid};

/// How long a device asking to pair waits for an answer.
pub(crate) const ASK_WAIT: Duration = Duration::from_mins(2);

/// The most devices that may be asking at once.
const MOST_ASKING: usize = 4;

/// How many devices may ask in a burst, and how often one more may once
/// they're used up: a scanner can't keep every window asking.
const ASKS: u32 = 4;
const ASK_EVERY: Duration = Duration::from_mins(2);

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
    pub(crate) listener: TcpListener,
    pub(crate) tls: Option<Arc<rustls::ServerConfig>>,
    random: SystemRandom,
    pub(crate) devices: Devices,
    vapid: Arc<Vapid>,
    pusher: Pusher,
    pub(crate) asks: Vec<Ask>,
    /// The devices asking changed since the windows were told.
    pub(crate) asks_changed: bool,
    tries: Bucket,
    ask_tries: Bucket,
    /// The password being checked, if one is.
    checking: Option<Checking>,
    /// How many passwords have been checked, which numbers them.
    checks: u64,
    checked: Receiver<bool>,
    check_done: Sender<bool>,
    waker: Arc<Waker>,
    /// The connections, HTTP and WebSocket, by their tokens.
    pub(crate) conns: Vec<mio::Token>,
    /// When something next needs doing, at the latest.
    pub(crate) next_due: Option<Instant>,
    /// When devices unused too long were last let go.
    expired_at: Instant,
}

impl Web {
    /// Starts the web app as `config` says, listening, with `waker` to
    /// wake the event loop when a thread has done something.
    ///
    /// # Errors
    ///
    /// If it can't listen, or its directory or certificate can't be read.
    pub(crate) fn start(config: WebConfig, waker: Arc<Waker>) -> Result<Self, String> {
        let tls = match &config.mode {
            WebMode::Tailscale => None,
            WebMode::Public { certificate, key } => Some(tls_config(certificate, key)?),
        };
        let devices = Devices::open(&config.data)?;
        let random = SystemRandom::new();
        let vapid = Arc::new(Vapid::open(&config.data, &random)?);
        let listener = TcpListener::bind(config.address)
            .map_err(|err| format!("can't listen on {}: {err}", config.address))?;
        let pusher = {
            let waker = Arc::clone(&waker);
            Pusher::start(Arc::clone(&vapid), move || {
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for a notification failed: {err}");
                }
            })
        };
        let (check_done, checked) = mpsc::channel();
        let now = Instant::now();
        Ok(Self {
            config,
            listener,
            tls,
            random,
            devices,
            vapid,
            pusher,
            asks: Vec::new(),
            asks_changed: false,
            tries: Bucket::new(auth::TRIES, auth::TRY_EVERY, now),
            ask_tries: Bucket::new(ASKS, ASK_EVERY, now),
            checking: None,
            checks: 0,
            checked,
            check_done,
            waker,
            conns: Vec::new(),
            next_due: None,
            expired_at: now,
        })
    }

    /// Where it listens.
    pub(crate) fn address(&self) -> Option<SocketAddr> {
        self.listener.local_addr().ok()
    }

    /// Makes sure something is done by `at`.
    pub(crate) fn due_by(&mut self, at: Instant) {
        self.next_due = Some(self.next_due.map_or(at, |due| due.min(at)));
    }

    /// When each ask runs out.
    pub(crate) fn asks_until(&self) -> impl Iterator<Item = Instant> + '_ {
        self.asks.iter().map(|ask| ask.until)
    }

    /// The asks waiting for an answer, as windows show them.
    pub(crate) fn asking(&self) -> Vec<clackatoo_protocol::PairingAsk> {
        (self.asks.iter())
            .filter(|ask| ask.answer.is_none())
            .map(|ask| clackatoo_protocol::PairingAsk {
                name: ask.name.clone(),
                from: ask.from.clone(),
                until: ask.until_ms,
            })
            .collect()
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

    /// Whether `login`, whom Tailscale says a request is from, is let in:
    /// the one the settings name, or else the first device's, or anyone
    /// until a device has paired.
    fn login_allowed(&self, login: &str) -> bool {
        match &self.config.login {
            Some(allowed) => allowed == login,
            None => (self.devices.list.iter())
                .find_map(|device| device.login.as_deref())
                .is_none_or(|first| first == login),
        }
    }

    /// Whether a request came from a page of the app's own origin.
    fn is_from_app(&self, request: &Request) -> bool {
        let forwarded = if self.config.tailscale() {
            request.header("x-forwarded-host")
        } else {
            None
        };
        auth::origin_allowed(
            request.header("origin"),
            self.config.origin.as_deref(),
            request.header("host"),
            forwarded,
        )
    }

    /// The device `request` carries the token of, if it's paired and let
    /// in now, as `login` through Tailscale.
    fn device(&mut self, request: &Request, login: Option<&str>) -> Result<u64, Denied> {
        let token = request.cookie(auth::COOKIE).ok_or(Denied::Unpaired)?;
        let device = (self.devices.by_token(&auth::token_hash(token))).ok_or(Denied::Unpaired)?;
        let now = epoch_millis();
        let id = device.id;
        if device.used < now.saturating_sub(millis(self.config.expire_after)) {
            log::info!("device {id} ({}) expired, unused", device.name);
            self.devices.remove(id);
            return Err(Denied::Unpaired);
        }
        if let (Some(paired), Some(login)) = (&device.login, login)
            && paired != login
        {
            return Err(Denied::Refused(Refused::OtherLogin));
        }
        if let (Some(every), Some(_)) = (self.config.password_every, &self.config.password) {
            let given = device.password.unwrap_or(device.paired);
            if given.saturating_add(millis(every)) < now {
                return Err(Denied::Renew);
            }
        }
        Ok(id)
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
                "password": self.config.password.is_some(),
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
            return self.check_password(conn, token, password, pairing, now);
        }
        match (body["ask"].as_bool(), pairing) {
            (Some(true), Pairing::New { name, login }) => self.ask(conn, name, login, now),
            _ => Handled::Respond(Response::error(400, "pairing gives a password, or asks")),
        }
    }

    /// The device whose token `request` carries, if it has one, paired or
    /// not let in for now: to give its password again.
    fn renewing(&self, request: &Request) -> Option<u64> {
        let token = request.cookie(auth::COOKIE)?;
        Some(self.devices.by_token(&auth::token_hash(token))?.id)
    }

    /// Starts checking `password`, for `pairing`, on a thread, with `conn`
    /// waiting for the answer, if a try is allowed now.
    fn check_password(
        &mut self,
        conn: &mut HttpConn,
        token: mio::Token,
        password: &str,
        pairing: Pairing,
        now: Instant,
    ) -> Handled {
        let Some(hash) = self.config.password.clone() else {
            return Handled::Respond(Response::error(
                403,
                "no password is set: allow this device from a window instead",
            ));
        };
        if self.checking.is_some() {
            return Handled::Respond(too_many(Duration::from_secs(1)));
        }
        if let Err(wait) = self.tries.take(now) {
            log::warn!("a password try from {} refused: too many tries", conn.peer);
            return Handled::Respond(too_many(wait));
        }
        let password = password.to_owned();
        let done = self.check_done.clone();
        let waker = Arc::clone(&self.waker);
        let spawned = std::thread::Builder::new()
            .name("password".to_owned())
            .spawn(move || {
                let _ = done.send(auth::password_matches(&password, &hash));
                let _ = waker.wake();
            });
        if spawned.is_err() {
            return Handled::Respond(Response::error(503, "the password can't be checked now"));
        }
        self.checks += 1;
        let number = self.checks;
        self.checking = Some(Checking {
            token,
            number,
            pairing,
        });
        conn.state = State::Verifying(number);
        // As long as checking could take, and more.
        conn.until = now + Duration::from_secs(30);
        self.due_by(conn.until);
        Handled::Wait
    }

    /// The connection waiting for a password's check, and the answer for
    /// it, once the check is done, if `waiting` says the connection with
    /// the token waits for the check with the number still: one that has
    /// gone gets nothing, and pairs nothing.
    pub(crate) fn checked(
        &mut self,
        waiting: impl FnOnce(mio::Token, u64) -> bool,
    ) -> Option<(mio::Token, Response)> {
        let matches = self.checked.try_recv().ok()?;
        let checking = self.checking.take()?;
        if !waiting(checking.token, checking.number) {
            log::info!("a password was checked for a connection that has gone");
            return None;
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
        Some((checking.token, response))
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

    /// Asks every window to allow a device called `name`, as `login`.
    fn ask(
        &mut self,
        conn: &HttpConn,
        name: String,
        login: Option<String>,
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
        if let Err(wait) = self.ask_tries.take(now) {
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
        let from = login.clone().unwrap_or_else(|| conn.peer.ip().to_string());
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

    /// Unpairs the devices unused too long, at most once a minute. Returns
    /// those unpaired.
    pub(crate) fn expire(&mut self, now: Instant) -> Vec<u64> {
        if now.saturating_duration_since(self.expired_at) < Duration::from_mins(1) {
            return Vec::new();
        }
        self.expired_at = now;
        let before = epoch_millis().saturating_sub(millis(self.config.expire_after));
        self.devices.expire(before)
    }
}

/// Why a device isn't let in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Denied {
    /// It has no token, or one no device has.
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

/// A device's name, as it asked to be called: trimmed, without control
/// characters, and short.
fn clean_name(name: &str) -> String {
    let name: String = (name.chars())
        .filter(|c| !c.is_control())
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

/// The TLS config for the certificate chain in `certificate` and its key
/// in `key`, both PEM files.
fn tls_config(
    certificate: &std::path::Path,
    key: &std::path::Path,
) -> Result<Arc<rustls::ServerConfig>, String> {
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
