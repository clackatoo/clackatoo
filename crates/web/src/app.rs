//! The app in the browser: its WebSocket to the daemon, its requests to
//! pair and for notifications, the page's events, and drawing, once a
//! frame, what changed.
//!
//! Every tap on the page goes to one listener, which reads the tapped
//! element's `data-action`; menus' changes, and parts opened and folded,
//! to two more. So the page's elements need no closures of their own.
//! The few closures the app keeps (the WebSocket's, and the page's
//! listeners) live as long as the page, or as their socket.

use std::cell::RefCell;

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{ChatAction, PROTOCOL_VERSION, TerminalId};
use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Document, Element as DomElement, Event, HtmlInputElement, HtmlSelectElement,
    HtmlTextAreaElement, MessageEvent, Notification, NotificationPermission,
    PushSubscriptionOptionsInit, RequestInit, VisibilityState, WebSocket, Window,
};

use crate::action::{Action, Pick};
use crate::dom;
use crate::state::{Received, State};
use crate::ui::el;
use crate::view::{self, Notifications, pairing};

/// About the most of a conversation an agent the chat moves to hears, in
/// tokens: the window's `carryover` option's default.
const CARRYOVER: u32 = 20_000;

/// The longest the app waits before connecting again, in seconds.
const MOST_RETRY: u32 = 30;

/// How often taps say again that the user is there, in milliseconds.
const PRESENT_EVERY: f64 = 60_000.0;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Does `f` with the app, unless it's busy, as it can't be from within
/// itself: everything the browser calls back runs on its own.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|app| {
        let mut app = app.try_borrow_mut().ok()?;
        app.as_mut().map(f)
    })
}

/// What the daemon said of this device.
#[derive(Clone, Debug, Default)]
struct Session {
    /// What it is: paired, unpaired, renew or refused.
    state: String,
    password: bool,
    ask: bool,
    /// Why it's refused.
    reason: String,
    /// The daemon's key for notifications.
    key: String,
    notifications: bool,
}

/// What's shown.
#[derive(Debug)]
enum Screen {
    /// Finding out whether the device is paired.
    Boot,
    /// Pairing, or giving the password again.
    Pair,
    /// The chats.
    App,
}

/// Where pairing is.
#[derive(Debug)]
enum Pairing {
    Start(pairing::Ways),
    Asking(String),
    Renew,
    Failed(String),
    Refused(String),
}

/// The parts of the page showing a chat.
#[derive(Debug)]
struct ChatPage {
    header: DomElement,
    turns: DomElement,
    footer: DomElement,
    menus: DomElement,
    textarea: HtmlTextAreaElement,
    /// What each part last showed, so an unchanged part isn't drawn again,
    /// as a menu that's open would close if it were.
    shown_header: crate::ui::Element,
    shown_footer: crate::ui::Element,
    shown_menus: crate::ui::Element,
}

struct App {
    window: Window,
    document: Document,
    root: DomElement,
    screen: Screen,
    state: State,
    session: Session,
    socket: Option<Socket>,
    /// Seconds to wait before connecting again.
    retry: u32,
    page: Option<ChatPage>,
    /// A frame is asked for, to draw in.
    frame_asked: bool,
    /// The user chose to pair in Safari, not in the app on the home
    /// screen.
    pair_here: bool,
    /// When the daemon was last told the user is there.
    present_said: f64,
}

/// The WebSocket, and its closures, which live as long as it does.
struct Socket {
    ws: WebSocket,
    decoder: FrameDecoder,
    _closures: Vec<Closure<dyn FnMut(JsValue)>>,
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        let _ = self.ws.close();
    }
}

/// Starts the app.
pub(crate) fn start() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&info.to_string().into());
    }));
    let window = web_sys::window().expect("a window");
    let document = window.document().expect("a document");
    let root = document
        .get_element_by_id("app")
        .expect("the page has #app");
    APP.with(|app| {
        *app.borrow_mut() = Some(App {
            window: window.clone(),
            document: document.clone(),
            root: root.clone(),
            screen: Screen::Boot,
            state: State::default(),
            session: Session::default(),
            socket: None,
            retry: 1,
            page: None,
            frame_asked: false,
            pair_here: false,
            present_said: 0.0,
        });
    });
    listen_to_page(&window, &document, &root);
    spawn_local(boot());
}

/// Listens for what happens on the page: taps, picks from menus, parts
/// opened and folded, typing, the page hidden or shown, the address
/// changed, and notifications tapped.
fn listen_to_page(window: &Window, document: &Document, root: &DomElement) {
    listen(root, "click", false, |event| {
        let target = event
            .target()
            .and_then(|target| target.dyn_into::<DomElement>().ok());
        let action = (target.and_then(|target| target.closest("[data-action]").ok().flatten()))
            .and_then(|element| element.get_attribute("data-action"))
            .and_then(|action| Action::decode(&action));
        with_app(|app| {
            app.touched();
            if let Some(action) = action {
                app.act(action);
            }
        });
    });
    listen(root, "change", false, |event| {
        let Some(select) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlSelectElement>().ok())
        else {
            return;
        };
        let pick = (select.get_attribute("data-setting"))
            .and_then(|setting| Pick::decode(&setting, &select.value()));
        if let Some(pick) = pick {
            with_app(|app| app.picked(pick));
        }
    });
    listen(root, "focusin", false, |event| {
        let model = (event
            .target()
            .and_then(|target| target.dyn_into::<DomElement>().ok()))
        .and_then(|target| target.get_attribute("data-setting"))
        .is_some();
        if model {
            with_app(App::menu_opened);
        }
    });
    // `toggle` doesn't bubble: heard on the way down.
    listen(root, "toggle", true, |event| {
        let Some(details) = event
            .target()
            .and_then(|target| target.dyn_into::<DomElement>().ok())
        else {
            return;
        };
        if let Some(fold) = details.get_attribute("data-fold") {
            let open = details.has_attribute("open");
            with_app(|app| app.state.folded(&fold, open));
        }
    });
    listen(root, "input", false, |event| {
        if let Some(area) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlTextAreaElement>().ok())
        {
            grow(&area);
        }
    });
    listen(root, "keydown", false, |event| {
        let Some(key) = event.dyn_ref::<web_sys::KeyboardEvent>() else {
            return;
        };
        let in_message = (event
            .target()
            .and_then(|target| target.dyn_into::<DomElement>().ok()))
        .is_some_and(|target| target.id() == "message");
        if in_message && key.key() == "Enter" && (key.meta_key() || key.ctrl_key()) {
            event.prevent_default();
            with_app(|app| app.act(Action::Send));
        }
    });
    listen(document, "visibilitychange", false, |_| {
        with_app(App::visibility_changed);
    });
    listen(window, "hashchange", false, |_| {
        with_app(App::route);
    });
    if let Some(workers) = service_workers(window) {
        listen(&workers, "message", false, |event| {
            let chat = (event.dyn_ref::<MessageEvent>())
                .and_then(|message| Reflect::get(&message.data(), &"chat".into()).ok())
                .and_then(|chat| chat.as_f64());
            if let Some(chat) = chat {
                with_app(|app| app.go(&format!("#chat/{chat}")));
            }
        });
    }
}

/// Listens for `kind` on `target`, for as long as the page lives, on the
/// way down if `capture`.
fn listen(
    target: &web_sys::EventTarget,
    kind: &str,
    capture: bool,
    f: impl FnMut(Event) + 'static,
) {
    let closure = Closure::<dyn FnMut(Event)>::new(f);
    let _ = target.add_event_listener_with_callback_and_bool(
        kind,
        closure.as_ref().unchecked_ref(),
        capture,
    );
    closure.forget();
}

/// The service workers, if the browser has them, as browsers do only
/// over HTTPS.
fn service_workers(window: &Window) -> Option<web_sys::ServiceWorkerContainer> {
    let navigator = window.navigator();
    Reflect::has(&navigator, &"serviceWorker".into())
        .unwrap_or(false)
        .then(|| navigator.service_worker())
}

/// Makes the box a message is typed in as tall as what's in it.
fn grow(area: &HtmlTextAreaElement) {
    let style = area.style();
    let _ = style.set_property("height", "auto");
    let _ = style.set_property("height", &format!("{}px", area.scroll_height().min(240)));
}

/// Finds out whether the device is paired, and shows what follows.
async fn boot() {
    let session = session().await;
    with_app(|app| match session {
        Ok(session) => app.session_said(session),
        Err(why) => app.show_pairing(Pairing::Failed(why)),
    });
}

/// Asks the daemon about this device.
async fn session() -> Result<Session, String> {
    let (status, body) = fetch("GET", "/session", None).await?;
    if status != 200 {
        return Err(error_of(&body, status));
    }
    let text = |name: &str| {
        (Reflect::get(&body, &name.into()).ok())
            .and_then(|value| value.as_string())
            .unwrap_or_default()
    };
    let flag = |name: &str| {
        (Reflect::get(&body, &name.into()).ok())
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    };
    Ok(Session {
        state: text("state"),
        password: flag("password"),
        ask: flag("ask"),
        reason: text("reason"),
        key: text("key"),
        notifications: flag("notifications"),
    })
}

/// What went wrong, as an answer's JSON says, or its status.
fn error_of(body: &JsValue, status: u16) -> String {
    (Reflect::get(body, &"error".into()).ok())
        .and_then(|error| error.as_string())
        .unwrap_or_else(|| match status {
            429 => "Too many tries: wait a minute, then try again".to_owned(),
            0 => "The daemon couldn't be reached".to_owned(),
            status => format!("The daemon answered {status}"),
        })
}

/// Sends `method` to `path`, with `body` as JSON, and returns the status
/// and what came back, as JSON, or `null`.
async fn fetch(method: &str, path: &str, body: Option<String>) -> Result<(u16, JsValue), String> {
    let window = web_sys::window().ok_or("no window")?;
    let init = RequestInit::new();
    init.set_method(method);
    init.set_credentials(web_sys::RequestCredentials::SameOrigin);
    if let Some(body) = &body {
        init.set_body(&JsValue::from_str(body));
    }
    let request = web_sys::Request::new_with_str_and_init(path, &init)
        .map_err(|_| "the request couldn't be made")?;
    if body.is_some() {
        let _ = request.headers().set("content-type", "application/json");
    }
    let response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| "The daemon couldn't be reached".to_owned())?;
    let response: web_sys::Response = response.dyn_into().map_err(|_| "not a response")?;
    let status = response.status();
    let json = match response.json() {
        Ok(json) => JsFuture::from(json).await.unwrap_or(JsValue::NULL),
        Err(_) => JsValue::NULL,
    };
    Ok((status, json))
}

/// Now, in milliseconds since the Unix epoch, as the daemon counts them.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "milliseconds since 1970 are whole, never negative, and fit"
)]
fn epoch_millis() -> u64 {
    js_sys::Date::now() as u64
}

/// `text` as a JSON string.
fn json_string(text: &str) -> String {
    use std::fmt::Write as _;
    let mut json = String::with_capacity(text.len() + 2);
    json.push('"');
    for c in text.chars() {
        match c {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(json, "\\u{:04x}", u32::from(c));
            }
            c => json.push(c),
        }
    }
    json.push('"');
    json
}

/// Pairs with `password`, or gives it again if `renew`, then starts over.
async fn pair_with_password(name: String, password: String, renew: bool) {
    let body = format!(
        "{{\"name\":{},\"password\":{},\"renew\":{renew}}}",
        json_string(&name),
        json_string(&password)
    );
    match fetch("POST", "/pair", Some(body)).await {
        Ok((200, _)) => boot().await,
        Ok((status, body)) => {
            with_app(|app| app.show_pairing(Pairing::Failed(error_of(&body, status))));
        }
        Err(why) => {
            with_app(|app| app.show_pairing(Pairing::Failed(why)));
        }
    }
}

/// Asks a window to allow this device, and waits for the answer.
async fn ask(name: String) {
    let body = format!("{{\"name\":{},\"ask\":true}}", json_string(&name));
    let asked = match fetch("POST", "/pair", Some(body)).await {
        Ok((200, body)) => body,
        Ok((status, body)) => {
            with_app(|app| app.show_pairing(Pairing::Failed(error_of(&body, status))));
            return;
        }
        Err(why) => {
            with_app(|app| app.show_pairing(Pairing::Failed(why)));
            return;
        }
    };
    let text = |name: &str| {
        (Reflect::get(&asked, &name.into()).ok())
            .and_then(|value| value.as_string())
            .unwrap_or_default()
    };
    let (wait, code) = (text("wait"), text("code"));
    with_app(|app| app.show_pairing(Pairing::Asking(code)));
    let body = format!("{{\"wait\":{}}}", json_string(&wait));
    match fetch("POST", "/pair/wait", Some(body)).await {
        Ok((200, _)) => boot().await,
        Ok((status, body)) => {
            with_app(|app| app.show_pairing(Pairing::Failed(error_of(&body, status))));
        }
        Err(why) => {
            with_app(|app| app.show_pairing(Pairing::Failed(why)));
        }
    }
}

/// Asks for notifications: the browser's permission, a subscription from
/// its push service, signed for with the daemon's key, given to the
/// daemon.
async fn notify(key: String) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let workers = service_workers(&window).ok_or("this browser can't be sent notifications")?;
    let permission =
        JsFuture::from(Notification::request_permission().map_err(|_| "no notifications")?)
            .await
            .map_err(|_| "notifications weren't allowed")?;
    if permission.as_string().as_deref() != Some("granted") {
        return Err("notifications weren't allowed".into());
    }
    let _ = JsFuture::from(workers.register("/sw.js")).await;
    let ready = JsFuture::from(workers.ready().map_err(|_| "no service worker")?)
        .await
        .map_err(|_| "the service worker didn't start")?;
    let registration: web_sys::ServiceWorkerRegistration =
        ready.dyn_into().map_err(|_| "no service worker")?;
    let options = PushSubscriptionOptionsInit::new();
    options.set_user_visible_only(true);
    let key = base64url(&key).ok_or("the daemon's key isn't right")?;
    options.set_application_server_key_opt_u8_array(Some(&Uint8Array::from(&key[..])));
    let manager = registration.push_manager().map_err(|_| "no push service")?;
    let subscription = JsFuture::from(
        manager
            .subscribe_with_options(&options)
            .map_err(|_| "no push service")?,
    )
    .await
    .map_err(|_| "the push service didn't subscribe")?;
    let subscription: web_sys::PushSubscription =
        subscription.dyn_into().map_err(|_| "no subscription")?;
    let json = subscription.to_json().map_err(|_| "no subscription")?;
    let body = js_sys::JSON::stringify(&json)
        .map_err(|_| "no subscription")?
        .as_string()
        .unwrap_or_default();
    match fetch("POST", "/push", Some(body)).await? {
        (204, _) => Ok(()),
        (status, body) => Err(error_of(&body, status)),
    }
}

/// `text`, in base64url, decoded.
pub(crate) fn base64url(text: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut count = 0;
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    for c in text.trim_end_matches('=').bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push(u8::try_from((bits >> count) & 0xFF).ok()?);
        }
    }
    Some(bytes)
}

/// The kind of device, to name it by when it pairs.
fn device_name(window: &Window) -> String {
    let agent = window.navigator().user_agent().unwrap_or_default();
    ["iPhone", "iPad", "Android"]
        .into_iter()
        .find(|kind| agent.contains(kind))
        .unwrap_or("Browser")
        .to_owned()
}

/// Whether it's an iPhone or iPad, whose Safari keeps its storage apart
/// from apps added to the home screen.
fn ios(window: &Window) -> bool {
    let navigator = window.navigator();
    let agent = navigator.user_agent().unwrap_or_default();
    let ipad_as_mac =
        navigator.platform().unwrap_or_default() == "MacIntel" && navigator.max_touch_points() > 1;
    ["iPhone", "iPad", "iPod"]
        .iter()
        .any(|kind| agent.contains(kind))
        || ipad_as_mac
}

/// Whether the app runs from the home screen, rather than in a browser.
fn standalone(window: &Window) -> bool {
    let media = (window
        .match_media("(display-mode: standalone)")
        .ok()
        .flatten())
    .is_some_and(|query| query.matches());
    let ios = Reflect::get(&window.navigator(), &"standalone".into())
        .ok()
        .and_then(|standalone| standalone.as_bool())
        .unwrap_or(false);
    media || ios
}

impl App {
    /// The daemon said what this device is.
    fn session_said(&mut self, session: Session) {
        let ways = pairing::Ways {
            password: session.password,
            ask: session.ask,
        };
        let screen = match session.state.as_str() {
            "paired" => None,
            "renew" => Some(Pairing::Renew),
            "refused" => Some(Pairing::Refused(session.reason.clone())),
            _ => Some(Pairing::Start(ways)),
        };
        self.session = session;
        if let Some(pairing) = screen {
            self.socket = None;
            self.show_pairing(pairing);
            return;
        }
        self.screen = Screen::App;
        if let Some(workers) = service_workers(&self.window) {
            let _ = workers.register("/sw.js");
        }
        self.route();
        if self.socket.is_none() {
            self.connect();
        }
        self.draw_soon();
    }

    /// Shows `pairing`.
    fn show_pairing(&mut self, which: Pairing) {
        let tree = match which {
            Pairing::Start(ways) => {
                let add_first = ios(&self.window) && !standalone(&self.window) && !self.pair_here;
                pairing::start(ways, &device_name(&self.window), add_first)
            }
            Pairing::Asking(code) => pairing::asking(&code),
            Pairing::Renew => pairing::renew(),
            Pairing::Failed(why) => pairing::failed(&why),
            Pairing::Refused(why) => pairing::refused(&why),
        };
        self.screen = Screen::Pair;
        self.page = None;
        dom::replace_children(&self.document, &self.root, &el("div").child(tree));
    }

    /// Opens the WebSocket.
    fn connect(&mut self) {
        let location = self.window.location();
        let secure = location
            .protocol()
            .is_ok_and(|protocol| protocol == "https:");
        let host = location.host().unwrap_or_default();
        let url = format!("{}//{host}/ws", if secure { "wss:" } else { "ws:" });
        let Ok(socket) = WebSocket::new_with_str(&url, "clackatoo") else {
            self.lost();
            return;
        };
        socket.set_binary_type(web_sys::BinaryType::Arraybuffer);
        let on_open = Closure::<dyn FnMut(JsValue)>::new(|_| {
            with_app(|app| {
                app.retry = 1;
                app.state.connected();
                app.send();
                app.draw_soon();
            });
        });
        let on_message = Closure::<dyn FnMut(JsValue)>::new(|event: JsValue| {
            let Some(data) = event.dyn_ref::<MessageEvent>().map(MessageEvent::data) else {
                return;
            };
            let bytes = Uint8Array::new(&data).to_vec();
            with_app(|app| app.received(&bytes));
        });
        let on_close = Closure::<dyn FnMut(JsValue)>::new(|event: JsValue| {
            let code = event
                .dyn_ref::<web_sys::CloseEvent>()
                .map_or(0, web_sys::CloseEvent::code);
            with_app(|app| app.closed(code));
        });
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        self.socket = Some(Socket {
            ws: socket,
            decoder: FrameDecoder::new(),
            _closures: vec![on_open, on_message, on_close],
        });
        self.state.online = crate::state::Online::Connecting;
    }

    /// Takes in what came over the WebSocket.
    fn received(&mut self, bytes: &[u8]) {
        let Some(socket) = &mut self.socket else {
            return;
        };
        socket.decoder.push(bytes);
        loop {
            // The daemon serves the app, so they're always of one build.
            let message = match socket.decoder.decode(PROTOCOL_VERSION) {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(err) => {
                    web_sys::console::error_1(
                        &format!("the daemon sent a bad frame: {err}").into(),
                    );
                    self.socket = None;
                    self.lost();
                    return;
                }
            };
            if self.state.receive(message) == Received::Reload {
                let _ = self.window.location().reload();
                return;
            }
        }
        self.send();
        self.draw_soon();
    }

    /// The WebSocket closed, with `code`.
    fn closed(&mut self, code: u16) {
        self.socket = None;
        // The device was unpaired, its password is to be given again, or the
        // daemon refused it: the daemon says which, and the app shows it.
        if (4001..=4003).contains(&code) {
            self.state = State::default();
            spawn_local(boot());
            return;
        }
        self.lost();
    }

    /// The connection is lost: tries again in a while, once the daemon
    /// says the device is still let in.
    fn lost(&mut self) {
        let retry = self.retry;
        self.retry = (self.retry * 2).min(MOST_RETRY);
        self.state.lost(retry);
        self.draw_soon();
        let again = Closure::once_into_js(move || {
            spawn_local(async {
                match session().await {
                    Ok(session) if session.state == "paired" => {
                        with_app(|app| {
                            app.session = session;
                            if app.socket.is_none() {
                                app.connect();
                            }
                        });
                    }
                    Ok(session) => {
                        with_app(|app| app.session_said(session));
                    }
                    Err(_) => {
                        with_app(App::lost);
                    }
                }
            });
        });
        let _ = self
            .window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                again.unchecked_ref(),
                i32::try_from(retry * 1000).unwrap_or(i32::MAX),
            );
    }

    /// Sends what's waiting, all in one WebSocket message.
    fn send(&mut self) {
        let messages = self.state.take_outbox();
        let Some(socket) = &self.socket else {
            return;
        };
        if messages.is_empty() {
            return;
        }
        let mut bytes = Vec::new();
        for message in &messages {
            match frame::encode(PROTOCOL_VERSION, message) {
                Ok(frame) => bytes.extend_from_slice(&frame),
                Err(err) => web_sys::console::error_1(&format!("{err}").into()),
            }
        }
        let _ = socket.ws.send_with_u8_array(&bytes);
    }

    /// Asks for a frame to draw what changed in, if one isn't asked for.
    fn draw_soon(&mut self) {
        if self.frame_asked {
            return;
        }
        self.frame_asked = true;
        let draw = Closure::once_into_js(|| {
            with_app(App::draw);
        });
        let _ = self.window.request_animation_frame(draw.unchecked_ref());
    }

    /// Draws what changed, then lets the daemon send more.
    fn draw(&mut self) {
        self.frame_asked = false;
        if !matches!(self.screen, Screen::App) {
            return;
        }
        let now = epoch_millis();
        if self.state.open.is_some() {
            self.draw_chat(now);
        } else if self.state.dirty.home || self.page.is_some() {
            self.page = None;
            let home = view::home(&self.state, self.notifications(), now);
            dom::replace_children(&self.document, &self.root, &el("div").child(home));
        }
        self.state.drawn();
        self.send();
    }

    /// Draws what changed in the chat shown.
    fn draw_chat(&mut self, now: u64) {
        let Some(open) = &self.state.open else {
            return;
        };
        let fresh = self.page.is_none();
        if fresh {
            self.page = Some(self.chat_page());
        }
        let Some(page) = &mut self.page else {
            return;
        };
        let scroller = self.document.scrolling_element();
        let at_bottom = scroller.as_ref().is_none_or(|scroller| {
            scroller.scroll_top() + scroller.client_height() >= scroller.scroll_height() - 80
        });
        let header = view::chat_header(&self.state, open);
        if fresh || header != page.shown_header {
            dom::replace_children(
                &self.document,
                &page.header,
                &el("div").child(header.clone()),
            );
            page.shown_header = header;
        }
        let node = open.view.as_ref().and_then(|view| view.chat());
        let dirty = &self.state.dirty;
        if let Some(node) = node {
            let drawn = page.turns.child_element_count() as usize;
            if fresh || dirty.all_turns {
                page.turns.set_text_content(None);
                for index in 0..node.turns.len() {
                    let turn = view::turn(node, open, index, now);
                    let _ = page
                        .turns
                        .append_child(&dom::element_of(&self.document, &turn));
                }
            } else {
                for &index in &dirty.turns {
                    if index >= node.turns.len() {
                        continue;
                    }
                    let turn = dom::element_of(&self.document, &view::turn(node, open, index, now));
                    match page
                        .turns
                        .children()
                        .item(u32::try_from(index).unwrap_or(u32::MAX))
                    {
                        Some(old) if index < drawn => {
                            let _ = page.turns.replace_child(&turn, &old);
                        }
                        _ => {
                            // A turn added: those before it are drawn already.
                            let _ = page.turns.append_child(&turn);
                        }
                    }
                }
            }
        } else if fresh {
            page.turns.set_text_content(None);
            let _ = page.turns.append_child(&dom::element_of(
                &self.document,
                &el("p").class("muted").text("Opening…"),
            ));
        }
        let footer = view::footer(&self.state, node);
        if fresh || footer != page.shown_footer {
            dom::replace_children(
                &self.document,
                &page.footer,
                &el("div").child(footer.clone()),
            );
            page.shown_footer = footer;
        }
        let menus = node.map_or_else(|| el("div"), view::menus);
        if fresh || menus != page.shown_menus {
            dom::replace_children(&self.document, &page.menus, &el("div").child(menus.clone()));
            page.shown_menus = menus;
        }
        if (at_bottom || fresh)
            && let Some(scroller) = scroller
        {
            scroller.set_scroll_top(scroller.scroll_height());
        }
    }

    /// The page showing a chat: its parts, empty, and the box a message is
    /// typed in.
    fn chat_page(&self) -> ChatPage {
        let document = &self.document;
        let main = dom::element_of(document, &el("main").class("chat-page"));
        let part = |class: &str| dom::element_of(document, &el("div").class(class));
        let header = part("header-slot");
        let turns = part("turns");
        let footer = part("footer-slot");
        let composer = part("composer");
        let menus = part("menus-slot");
        let row = part("row");
        let textarea: HtmlTextAreaElement = document
            .create_element("textarea")
            .expect("textarea")
            .unchecked_into();
        textarea.set_id("message");
        textarea.set_rows(1);
        let _ = textarea.set_attribute("placeholder", "Message");
        let _ = textarea.set_attribute("enterkeyhint", "enter");
        let send = dom::element_of(
            document,
            &el("button")
                .class("send")
                .action(&Action::Send)
                .text("Send"),
        );
        let _ = row.append_child(&textarea);
        let _ = row.append_child(&send);
        let _ = composer.append_child(&menus);
        let _ = composer.append_child(&row);
        for part in [&header, &turns, &footer, &composer] {
            let _ = main.append_child(part);
        }
        self.root.set_text_content(None);
        let _ = self.root.append_child(&main);
        ChatPage {
            header,
            turns,
            footer,
            menus,
            textarea,
            shown_header: el("div"),
            shown_footer: el("div"),
            shown_menus: el("div"),
        }
    }

    /// What the device can do about notifications.
    fn notifications(&self) -> Notifications {
        let supported = service_workers(&self.window).is_some()
            && Reflect::has(&self.window, &"PushManager".into()).unwrap_or(false)
            && Reflect::has(&self.window, &"Notification".into()).unwrap_or(false);
        if !supported {
            return Notifications::Unsupported {
                ios: ios(&self.window) && !standalone(&self.window),
            };
        }
        match Notification::permission() {
            NotificationPermission::Denied => Notifications::Blocked,
            NotificationPermission::Granted if self.session.notifications => Notifications::On,
            _ => Notifications::Off,
        }
    }

    /// Does what was tapped.
    fn act(&mut self, action: Action) {
        match action {
            Action::Home => self.go(""),
            Action::Open(chat) => self.go(&format!("#chat/{chat}")),
            Action::Answer { question, choice } => {
                let option = (self.state.open.as_ref())
                    .and_then(|open| open.view.as_ref()?.chat()?.question.clone())
                    .filter(|asked| asked.id == question)
                    .and_then(|asked| asked.choices.get(choice).map(|choice| choice.id.clone()));
                if let Some(option) = option {
                    let answer = ChatAction::Answer {
                        question,
                        option: Some(option),
                    };
                    self.state.chat(answer, "answering");
                }
            }
            Action::Stop => self.state.chat(ChatAction::Stop, "stopping the agent"),
            Action::Send => {
                let Some(page) = &self.page else {
                    return;
                };
                let text = page.textarea.value();
                if text.trim().is_empty() {
                    return;
                }
                page.textarea.set_value("");
                grow(&page.textarea);
                self.state
                    .chat(ChatAction::Send(text), "sending the message");
            }
            Action::Notify => {
                let key = self.session.key.clone();
                spawn_local(async move {
                    let outcome = notify(key).await;
                    with_app(|app| {
                        match outcome {
                            Ok(()) => {
                                app.session.notifications = true;
                                app.state.note = None;
                            }
                            Err(why) => app.state.note = Some(why),
                        }
                        app.state.dirty.home = true;
                        app.draw_soon();
                    });
                });
            }
            Action::Pair | Action::Renew => {
                let name = self
                    .input("device-name")
                    .unwrap_or_else(|| device_name(&self.window));
                let password = self.input("password").unwrap_or_default();
                spawn_local(pair_with_password(name, password, action == Action::Renew));
            }
            Action::Ask => {
                let name = self
                    .input("device-name")
                    .unwrap_or_else(|| device_name(&self.window));
                spawn_local(ask(name));
            }
            Action::Again => {
                self.screen = Screen::Boot;
                spawn_local(boot());
            }
            Action::PairHere => {
                self.pair_here = true;
                let ways = pairing::Ways {
                    password: self.session.password,
                    ask: self.session.ask,
                };
                self.show_pairing(Pairing::Start(ways));
            }
        }
        self.send();
        self.draw_soon();
    }

    /// The value of the input with `id`, if there's one.
    fn input(&self, id: &str) -> Option<String> {
        let input = self.document.get_element_by_id(id)?;
        Some(input.dyn_into::<HtmlInputElement>().ok()?.value())
    }

    /// Something was picked from a chat's menus.
    fn picked(&mut self, pick: Pick) {
        let action = match pick {
            Pick::Set { setting, value } => ChatAction::Set { setting, value },
            Pick::Switch { agent, model } => ChatAction::Switch {
                agent,
                model,
                budget: CARRYOVER,
            },
        };
        self.state.chat(action, "changing the setting");
        self.send();
    }

    /// A chat's menu is opening: the models of other agents not known yet
    /// are asked for.
    fn menu_opened(&mut self) {
        let unknown = (self.state.open.as_ref())
            .and_then(|open| open.view.as_ref()?.chat())
            .is_some_and(|node| {
                (node.providers.iter())
                    .any(|provider| provider.models == clackatoo_protocol::ProviderModels::Unknown)
            });
        if unknown {
            self.state
                .chat(ChatAction::LearnModels, "asking for the models");
            self.send();
        }
    }

    /// Goes to `hash`: a chat, or the list.
    fn go(&self, hash: &str) {
        let _ = self.window.location().set_hash(hash);
    }

    /// Shows what the address says: a chat, or the list.
    fn route(&mut self) {
        let hash = self.window.location().hash().unwrap_or_default();
        let chat = hash
            .strip_prefix("#chat/")
            .and_then(|chat| chat.parse().ok())
            .map(TerminalId);
        if let Some(chat) = chat {
            self.state.open_chat(chat);
        } else {
            self.state.close_chat();
            self.page = None;
        }
        self.send();
        self.draw_soon();
    }

    /// The page was hidden or shown: the daemon hears whether the user is
    /// looking, and a connection lost while hidden is tried again at once.
    fn visibility_changed(&mut self) {
        let here = self.document.visibility_state() == VisibilityState::Visible;
        self.state.present(here);
        self.present_said = if here { js_sys::Date::now() } else { 0.0 };
        if here && matches!(self.screen, Screen::App) && self.socket.is_none() {
            self.retry = 1;
            self.connect();
        }
        self.send();
    }

    /// The user touched the page: the daemon hears they're there, if it
    /// hasn't lately.
    fn touched(&mut self) {
        let now = js_sys::Date::now();
        if now - self.present_said > PRESENT_EVERY {
            self.present_said = now;
            self.state.present(true);
            self.send();
        }
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::ClientMessage;

    use super::*;

    #[test]
    fn keys_are_read_from_base64url() {
        assert_eq!(base64url("AQID").unwrap(), [1, 2, 3]);
        assert_eq!(base64url("_-8").unwrap(), [0xFF, 0xEF]);
        assert_eq!(base64url("AQ==").unwrap(), [1]);
        assert!(base64url("A*").is_none());
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }

    #[test]
    fn chat_messages_go_in_frames() {
        // What `send` writes: frames back to back, as the daemon reads them.
        let messages = [
            ClientMessage::Present { here: true },
            ClientMessage::FrameDone {
                terminal: TerminalId(1),
            },
        ];
        let mut bytes = Vec::new();
        for message in &messages {
            bytes.extend(frame::encode(PROTOCOL_VERSION, message).unwrap());
        }
        let mut decoder = FrameDecoder::new();
        decoder.push(&bytes);
        assert_eq!(
            decoder.decode::<ClientMessage>(PROTOCOL_VERSION).unwrap(),
            Some(messages[0].clone())
        );
        assert_eq!(
            decoder.decode::<ClientMessage>(PROTOCOL_VERSION).unwrap(),
            Some(messages[1].clone())
        );
    }
}
