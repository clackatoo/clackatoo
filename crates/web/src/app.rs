//! The app in the browser: its WebSocket to the daemon, its requests to
//! pair and for notifications, the page's events, and drawing, once a
//! frame, what changed.
//!
//! Every tap on the page goes to one listener, which reads the tapped
//! element's `data-action`; menus' changes, and parts opened and folded,
//! to two more. So the page's elements need no closures of their own.
//! The few closures the app keeps (the WebSocket's, and the page's
//! listeners) live as long as the page, or as their socket.
//!
//! Keys typed into a terminal come from the box under it, as the phone's
//! keyboard types them: text as it's put in, Enter and Backspace, and
//! keys a keyboard attached to the phone has, each sent at once.

use std::cell::RefCell;

mod terminal;

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{ChatAction, PROTOCOL_VERSION, TerminalId};
use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AddEventListenerOptions, Document, Element as DomElement, Event, HtmlInputElement,
    HtmlSelectElement, HtmlTextAreaElement, InputEvent, KeyboardEvent, MessageEvent, Notification,
    NotificationPermission, PushSubscriptionOptionsInit, RequestInit, TouchEvent, VisibilityState,
    WebSocket, Window,
};

use self::terminal::TermPage;
use crate::action::{Action, Pick, RowKey};
use crate::dom;
use crate::keys::{Key, Modifiers};
use crate::state::{Received, State};
use crate::term::Fit;
use crate::ui::el;
use crate::view::{self, Notifications, pairing};

/// About the most of a conversation an agent the chat moves to hears, in
/// tokens: the window's `carryover` option's default.
const CARRYOVER: u32 = 20_000;

/// The longest the app waits before connecting again, in seconds.
const MOST_RETRY: u32 = 30;

/// How often taps say again that the user is there, in milliseconds.
const PRESENT_EVERY: f64 = 60_000.0;

/// How often the list of terminals is asked for again while it's shown,
/// in milliseconds, for what each is running and where; and how often a
/// fit is looked at, to ask for it again.
const LIST_EVERY: i32 = 5_000;

/// How long the phone's screen stays the same size, in milliseconds,
/// before a fitted terminal is fitted again to it.
const REFIT_AFTER: i32 = 300;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Does `f` with the app, unless it's busy, as it can't be from within
/// itself: everything the browser calls back runs on its own.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|app| {
        let mut app = app.try_borrow_mut().ok()?;
        let app = app.as_mut()?;
        app.state.now = js_sys::Date::now();
        Some(f(app))
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
    /// The page showing a terminal, while one is shown.
    term_page: Option<TermPage>,
    /// When the app looks again at whether keys go, if it's to.
    check_at: f64,
    /// The key on the row of keys a finger is on, and where it came down.
    pressed: Option<(RowKey, (f64, f64))>,
    /// Counts the phone's screen changes, so the terminal is fitted again
    /// only once it settles.
    refits: u32,
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
            term_page: None,
            check_at: 0.0,
            pressed: None,
            refits: 0,
            frame_asked: false,
            pair_here: false,
            present_said: 0.0,
        });
    });
    listen_to_page(&window, &document, &root);
    listen_for_typing(&root);
    let tick = Closure::<dyn FnMut()>::new(|| {
        with_app(App::tick);
    });
    let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(
        tick.as_ref().unchecked_ref(),
        LIST_EVERY,
    );
    tick.forget();
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
        let Some(key) = event.dyn_ref::<KeyboardEvent>() else {
            return;
        };
        let in_message = target_id(&event).is_some_and(|id| id == "message");
        if in_message && key.key() == "Enter" && (key.meta_key() || key.ctrl_key()) {
            event.prevent_default();
            with_app(|app| app.act(Action::Send));
        }
    });
    // Scrolling doesn't bubble: heard on the way down.
    listen(root, "scroll", true, |_| {
        with_app(App::scrolled);
    });
    listen(window, "resize", false, |_| {
        with_app(App::viewport_changed);
    });
    if let Some(viewport) = window.visual_viewport() {
        for kind in ["resize", "scroll"] {
            listen(&viewport, kind, false, |_| {
                with_app(App::viewport_changed);
            });
        }
    }
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

/// The ID of the element `event` happened to, if it has one.
fn target_id(event: &Event) -> Option<String> {
    let target = event.target()?.dyn_into::<DomElement>().ok()?;
    Some(target.id())
}

/// Listens for what's typed into the box under a terminal: keys a
/// keyboard attached to the phone has, as they're pressed; text, Enter
/// and Backspace as the phone's keyboard puts them in, and what it puts
/// in after composing, as accents and other scripts are typed. The box
/// holds only a zero-width space, so the keyboard has nothing to correct.
fn listen_for_typing(root: &DomElement) {
    listen(root, "keydown", false, |event| {
        let Some(key) = event.dyn_ref::<KeyboardEvent>() else {
            return;
        };
        // Mid-composition, or a phone's keyboard saying only that a key
        // was pressed: what it types comes as input.
        if target_id(&event).as_deref() != Some("keys")
            || key.is_composing()
            || key.key_code() == 229
        {
            return;
        }
        let name = key.key();
        let modifiers = Modifiers {
            shift: key.shift_key(),
            ctrl: key.ctrl_key(),
            alt: key.alt_key(),
        };
        let typed = match Key::named(&name) {
            Some(named) => Some(named),
            // Text with Ctrl or Alt held, which input never carries. Cmd
            // is the system's, as Cmd+V to paste.
            None if (modifiers.ctrl || modifiers.alt)
                && !key.meta_key()
                && name.chars().count() == 1 =>
            {
                Some(Key::Text(name))
            }
            None => None,
        };
        if let Some(typed) = typed {
            event.prevent_default();
            with_app(|app| app.typed(&typed, modifiers));
        }
    });
    listen(root, "beforeinput", false, |event| {
        let Some(input) = event.dyn_ref::<InputEvent>() else {
            return;
        };
        // What can't be kept out of the box, mid-composition or otherwise,
        // is sent once it's landed there (see `input` below), and only
        // then, so nothing goes twice.
        if target_id(&event).as_deref() != Some("keys")
            || input.is_composing()
            || !event.cancelable()
        {
            return;
        }
        let data = input.data().unwrap_or_default();
        let none = Modifiers::default();
        let ctrl = Modifiers { ctrl: true, ..none };
        let typed = match input.input_type().as_str() {
            "insertText" | "insertReplacementText" => Some((Key::Text(data), none)),
            "insertLineBreak" | "insertParagraph" => Some((Key::Enter, none)),
            "deleteContentBackward" => Some((Key::Backspace, none)),
            "deleteContentForward" => Some((Key::Delete, none)),
            // As shells take Ctrl+W.
            "deleteWordBackward" => Some((Key::Text("w".to_owned()), ctrl)),
            "insertFromPaste" | "insertFromDrop" | "insertFromPasteAsQuotation" => {
                let text = (input.data_transfer())
                    .and_then(|transfer| transfer.get_data("text/plain").ok())
                    .filter(|text| !text.is_empty())
                    .unwrap_or(data);
                event.prevent_default();
                with_app(|app| app.pasted(&text));
                return;
            }
            _ => None,
        };
        // Nothing is left in the box: it's what's sent that counts.
        event.prevent_default();
        if let Some((typed, modifiers)) = typed {
            with_app(|app| app.typed(&typed, modifiers));
        }
    });
    listen_for_landed_text(root);
}

/// Listens for what lands in the box under a terminal all the same, as
/// composing ends, or as a phone types what can't be kept out, and keeps
/// the one character the box always holds.
fn listen_for_landed_text(root: &DomElement) {
    // The box always holds one character, for Backspace to take, with the
    // caret after it: put back as it's focused or tapped, and again once
    // the browser has placed the caret itself.
    for kind in ["focusin", "click"] {
        listen(root, kind, false, |event| {
            let Some(input) = event
                .target()
                .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
                .filter(|input| input.id() == "keys")
            else {
                return;
            };
            terminal::keep_sentinel(&input);
            let again = Closure::once_into_js(move || terminal::keep_sentinel(&input));
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    again.unchecked_ref(),
                    0,
                );
            }
        });
    }
    // What got into the box all the same, as composing ends or as a phone
    // types what can't be kept out: sent as it landed, and taken out.
    for kind in ["input", "compositionend"] {
        listen(root, kind, false, |event| {
            let Some(input) = event
                .target()
                .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
                .filter(|input| input.id() == "keys")
            else {
                return;
            };
            let typed = event.dyn_ref::<InputEvent>();
            if typed.is_some_and(InputEvent::is_composing) {
                return;
            }
            let value = input.value();
            terminal::keep_sentinel(&input);
            // Something deleted what the box holds: the key that did.
            let deleted = typed
                .map(InputEvent::input_type)
                .and_then(|kind| match kind.as_str() {
                    "deleteContentBackward" => Some((Key::Backspace, Modifiers::default())),
                    "deleteContentForward" => Some((Key::Delete, Modifiers::default())),
                    "deleteWordBackward" => Some((
                        Key::Text("w".to_owned()),
                        Modifiers {
                            ctrl: true,
                            ..Modifiers::default()
                        },
                    )),
                    _ => None,
                });
            if let Some((key, modifiers)) = deleted.filter(|_| value != terminal::SENTINEL) {
                with_app(|app| app.typed(&key, modifiers));
                return;
            }
            let text = value.replace(terminal::SENTINEL, "");
            if !text.is_empty() {
                with_app(|app| app.typed(&Key::Text(text), Modifiers::default()));
            }
        });
    }
}

/// How far a finger on a key may move, in CSS pixels, and still press it
/// when it lifts: further, and it was a swipe.
const PRESS_SLOP: f64 = 10.0;

/// Listens for fingers on a terminal's page, for as long as the page
/// lives: two on the rows, `scroller`, pinch to zoom them, and one on the
/// row of keys, `keys`, presses a key, without taking the keyboard away,
/// as a tap would. Only these elements wait to hear about touches before
/// scrolling, so chats and the list scroll as the browser scrolls them.
fn touch_listeners(
    scroller: &web_sys::EventTarget,
    keys: &web_sys::EventTarget,
) -> Vec<Closure<dyn FnMut(Event)>> {
    let apart = |event: &TouchEvent| {
        let touches = event.touches();
        let (Some(a), Some(b)) = (touches.item(0), touches.item(1)) else {
            return None;
        };
        let dx = f64::from(a.client_x() - b.client_x());
        let dy = f64::from(a.client_y() - b.client_y());
        Some(dx.hypot(dy))
    };
    let first = |event: &TouchEvent| {
        let touch = event.touches().item(0)?;
        Some((f64::from(touch.client_x()), f64::from(touch.client_y())))
    };
    let mut kept = Vec::new();
    kept.push(listen_kept(scroller, "touchstart", move |event| {
        let Some(apart) = event.dyn_ref::<TouchEvent>().and_then(apart) else {
            return;
        };
        with_app(|app| {
            if let Some(page) = &mut app.term_page {
                page.pinch_started(apart);
            }
        });
    }));
    kept.push(listen_kept(scroller, "touchmove", move |event| {
        let Some(apart) = event.dyn_ref::<TouchEvent>().and_then(apart) else {
            return;
        };
        let pinching =
            with_app(|app| (app.term_page.as_mut()).is_some_and(|page| page.pinched(apart)));
        if pinching == Some(true) {
            event.prevent_default();
        }
    }));
    for kind in ["touchend", "touchcancel"] {
        kept.push(listen_kept(scroller, kind, |_| {
            with_app(|app| {
                if let Some(page) = &mut app.term_page {
                    page.pinch_ended();
                }
            });
        }));
    }
    // iOS zooms the page on a pinch whatever `touch-action` says.
    kept.push(listen_kept(scroller, "gesturestart", |event| {
        event.prevent_default();
    }));
    kept.push(listen_kept(keys, "touchstart", move |event| {
        let key = (event.target())
            .and_then(|target| target.dyn_into::<DomElement>().ok())
            .and_then(|target| target.closest(".key").ok().flatten())
            .and_then(|key| key.get_attribute("data-action"))
            .and_then(|action| match Action::decode(&action) {
                Some(Action::Key(key)) => Some(key),
                _ => None,
            });
        let at = event.dyn_ref::<TouchEvent>().and_then(first);
        if let (Some(key), Some(at)) = (key, at) {
            // Keeps the box focused, and the keyboard up.
            event.prevent_default();
            with_app(|app| app.pressed = Some((key, at)));
        }
    }));
    kept.push(listen_kept(keys, "touchmove", move |event| {
        let Some((x, y)) = event.dyn_ref::<TouchEvent>().and_then(first) else {
            return;
        };
        with_app(|app| {
            let moved = (app.pressed)
                .is_some_and(|(_, (from_x, from_y))| (x - from_x).hypot(y - from_y) > PRESS_SLOP);
            if moved {
                app.pressed = None;
            }
        });
    }));
    for kind in ["touchend", "touchcancel"] {
        kept.push(listen_kept(keys, kind, |event| {
            let ended = event.type_() == "touchend";
            with_app(|app| {
                // On the finger's lifting, which lets Paste read the
                // clipboard.
                if let Some((key, _)) = app.pressed.take()
                    && ended
                {
                    app.act(Action::Key(key));
                }
            });
        }));
    }
    // With a mouse, a key pressed keeps the box focused too.
    kept.push(listen_kept(keys, "mousedown", |event| {
        let on_key = (event.target())
            .and_then(|target| target.dyn_into::<DomElement>().ok())
            .and_then(|target| target.closest(".key").ok().flatten())
            .is_some();
        if on_key {
            event.prevent_default();
        }
    }));
    kept
}

/// Listens for `kind` on `target`, able to keep the browser from doing
/// what it does by default, as scrolling, which it otherwise may not wait
/// to hear about. The listener lasts as long as what's returned is kept.
fn listen_kept(
    target: &web_sys::EventTarget,
    kind: &str,
    f: impl FnMut(Event) + 'static,
) -> Closure<dyn FnMut(Event)> {
    let closure = Closure::<dyn FnMut(Event)>::new(f);
    let options = AddEventListenerOptions::new();
    options.set_passive(false);
    let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
        kind,
        closure.as_ref().unchecked_ref(),
        &options,
    );
    closure
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
        self.check_keys_later();
    }

    /// Looks again at whether keys go once what's waited for has gone
    /// unanswered too long, if it has by then, to say so: a terminal shown
    /// says keys aren't sent before any is turned down.
    fn check_keys_later(&mut self) {
        if self.state.term.is_none() {
            return;
        }
        let Some(at) = self.state.keys_stop_at() else {
            return;
        };
        if at <= self.check_at {
            return;
        }
        self.check_at = at;
        let check = Closure::once_into_js(|| {
            with_app(|app| {
                if let Some(term) = &mut app.state.term {
                    term.dirty.header = true;
                }
                app.draw_soon();
            });
        });
        let wait = (at - self.state.now).max(0.0) + 20.0;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "milliseconds, at most a few thousand"
        )]
        let wait = wait as i32;
        let _ = self
            .window
            .set_timeout_with_callback_and_timeout_and_arguments_0(check.unchecked_ref(), wait);
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
        if self.state.term.is_some() {
            self.page = None;
            self.draw_terminal();
        } else if self.state.open.is_some() {
            self.term_page = None;
            self.draw_chat(now);
        } else if self.state.dirty.home || self.page.is_some() || self.term_page.is_some() {
            self.page = None;
            self.term_page = None;
            let home = view::home(&self.state, self.notifications(), now);
            dom::replace_children(&self.document, &self.root, &el("div").child(home));
        }
        self.state.drawn();
        self.send();
    }

    /// Draws what changed in the terminal shown, then asks for older
    /// history if the user is near the top of what's held.
    fn draw_terminal(&mut self) {
        let Some(term) = &self.state.term else {
            return;
        };
        let fresh = self.term_page.is_none();
        let page =
            (self.term_page).get_or_insert_with(|| TermPage::new(&self.document, &self.root));
        if fresh && let Some(viewport) = self.window.visual_viewport() {
            page.place(viewport.offset_top(), viewport.height());
        }
        page.draw(&self.document, &self.state, term);
        if page.near_top() {
            self.state.older_history();
        }
    }

    /// The terminal's rows were scrolled: older history is asked for as
    /// the top of what's held nears.
    fn scrolled(&mut self) {
        if self.term_page.as_ref().is_some_and(TermPage::near_top) {
            self.state.older_history();
            self.send();
        }
    }

    /// The phone's screen changed size, or its keyboard came or went: the
    /// terminal's page is put above the keyboard, its font fitted again.
    fn viewport_changed(&mut self) {
        let Some(page) = &mut self.term_page else {
            return;
        };
        if let Some(viewport) = self.window.visual_viewport() {
            page.place(viewport.offset_top(), viewport.height());
        }
        page.fit_font();
        // Fitted, it's fitted again to the space left, once the screen
        // settles: the keyboard coming up takes rows away.
        if !self
            .state
            .term
            .as_ref()
            .is_some_and(|term| term.fit != Fit::Off)
        {
            return;
        }
        self.refits = self.refits.wrapping_add(1);
        let refit = self.refits;
        let settled = Closure::once_into_js(move || {
            with_app(|app| {
                if app.refits != refit {
                    return;
                }
                if let Some(size) = app.term_page.as_ref().and_then(TermPage::phone_size) {
                    app.state.refit(size);
                    app.send();
                    app.draw_soon();
                }
            });
        });
        let _ = self
            .window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                settled.unchecked_ref(),
                REFIT_AFTER,
            );
    }

    /// `key` was typed into the terminal shown, with `modifiers` held.
    fn typed(&mut self, key: &Key, modifiers: Modifiers) {
        self.state.type_key(key, modifiers);
        self.send();
        self.draw_soon();
    }

    /// `text` was pasted into the terminal shown. Lines a program would
    /// take as typed, one command each, go only once the user says so.
    fn pasted(&mut self, text: &str) {
        if let Some(lines) = self.state.paste_runs_lines(text) {
            let ask = format!(
                "Paste {lines} lines? The program hasn't asked for pastes to be marked, so \
                 each line runs as if typed."
            );
            if self.window.confirm_with_message(&ask) != Ok(true) {
                return;
            }
        }
        self.state.paste(text);
        self.send();
        self.draw_soon();
    }

    /// Pastes what's on the clipboard into the terminal shown, as the
    /// browser lets it be read, once the user says so.
    fn paste_clipboard(&mut self) {
        let read = self.window.navigator().clipboard().read_text();
        spawn_local(async move {
            let text = JsFuture::from(read).await;
            with_app(|app| {
                if let Some(text) = text.ok().and_then(|text| text.as_string()) {
                    app.pasted(&text);
                    return;
                }
                app.state.note =
                    Some("The clipboard couldn't be read: paste into the box instead".to_owned());
                if let Some(term) = &mut app.state.term {
                    term.dirty.header = true;
                }
                app.draw_soon();
            });
        });
    }

    /// Asks for the list of terminals again, every so often, while it's
    /// shown.
    fn tick(&mut self) {
        let home = self.state.term.is_none() && self.state.open.is_none();
        let shown = self.document.visibility_state() == VisibilityState::Visible;
        if home && shown && matches!(self.screen, Screen::App) {
            self.state.list_terminals();
        }
        // A fit lasts only while it's asked for again.
        if shown {
            self.state.renew_fit();
        }
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
            Action::OpenTerminal(terminal) => self.go(&format!("#term/{terminal}")),
            Action::Fit => {
                let size = self.term_page.as_ref().and_then(TermPage::phone_size);
                if size.is_some() {
                    self.state.fit(size);
                }
            }
            Action::Unfit => self.state.fit(None),
            Action::Type => {
                if let Some(page) = &self.term_page {
                    let _ = page.input.focus();
                }
            }
            Action::Key(RowKey::Ctrl) => self.state.tap_ctrl(),
            Action::Key(RowKey::Paste) => self.paste_clipboard(),
            Action::Key(key) => {
                if let Some((typed, modifiers)) = key.key() {
                    self.state.type_key(&typed, modifiers);
                }
            }
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

    /// Shows what the address says: a chat, a terminal, or the list.
    fn route(&mut self) {
        let hash = self.window.location().hash().unwrap_or_default();
        let id = |prefix: &str| {
            (hash.strip_prefix(prefix))
                .and_then(|id| id.parse().ok())
                .map(TerminalId)
        };
        if let Some(chat) = id("#chat/") {
            self.state.open_chat(chat);
        } else if let Some(terminal) = id("#term/") {
            if self
                .state
                .term
                .as_ref()
                .is_none_or(|term| term.id != terminal)
            {
                self.term_page = None;
            }
            self.state.open_terminal(terminal);
        } else {
            self.state.close_chat();
            self.state.close_terminal();
            self.state.list_terminals();
            self.page = None;
            self.term_page = None;
        }
        self.send();
        self.draw_soon();
    }

    /// The page was hidden or shown: the daemon hears whether the user is
    /// looking, and a connection lost while hidden is tried again at once.
    fn visibility_changed(&mut self) {
        let here = self.document.visibility_state() == VisibilityState::Visible;
        self.state.present(here);
        self.state.terminal_hidden(!here);
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
