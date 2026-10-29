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

mod photos;
mod terminal;
mod viewport;

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{ChatAction, FormReply, PROTOCOL_VERSION, TerminalId};
use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AddEventListenerOptions, Document, Element as DomElement, Event, HtmlElement, HtmlInputElement,
    HtmlTextAreaElement, InputEvent, KeyboardEvent, MessageEvent, Notification,
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
    photos: photos::Controls,
    main: HtmlElement,
    scroller: DomElement,
    latest: HtmlElement,
    history: HtmlElement,
    send_button: HtmlElement,
    steer_button: HtmlElement,
    intent: HtmlElement,
    queue: DomElement,
    shown_queue: crate::ui::Element,
    header: DomElement,
    turns: DomElement,
    /// The line saying what the agent is doing, while it works, and the
    /// two parts of it that change: made once, so its dots, which the
    /// style sheet moves, never start again as the seconds count.
    working: HtmlElement,
    doing: DomElement,
    seconds: DomElement,
    /// What it last said, if it showed.
    shown_working: Option<(String, String)>,
    footer: DomElement,
    menus: DomElement,
    textarea: HtmlTextAreaElement,
    /// What each part last showed, so an unchanged part isn't drawn again,
    /// as a menu that's open would close if it were.
    shown_header: crate::ui::Element,
    shown_footer: crate::ui::Element,
    shown_menus: crate::ui::Element,
}

impl ChatPage {
    fn draw_controls(
        &mut self,
        document: &Document,
        state: &State,
        open: &crate::state::OpenChat,
    ) -> bool {
        let node = open.view.as_ref().and_then(|v| v.chat());
        self.photos.draw(document, state, open.id);
        let draft = state.drafts.get(&open.id).cloned().unwrap_or_default();
        let queue = node.map_or_else(
            || el("div"),
            |n| {
                el("div")
                    .child(view::commands(n, &draft))
                    .child(view::plan(n))
                    .child(view::background(n))
                    .child(view::queue(n, state.online))
            },
        );
        if queue != self.shown_queue {
            dom::replace_children(document, &self.queue, &el("div").child(queue.clone()));
            self.shown_queue = queue;
        }
        let changed = self.textarea.value() != draft;
        if changed {
            self.textarea.set_value(&draft);
            grow(&self.textarea);
        }
        let busy = node.is_some_and(|n| {
            matches!(
                n.state,
                clackatoo_protocol::ChatState::Answering | clackatoo_protocol::ChatState::Starting
            )
        });
        let sending = state.submitting.contains(&open.id);
        let empty = draft.trim().is_empty() && state.images.get(&open.id).is_none_or(Vec::is_empty);
        let disabled = empty
            || sending
            || state.loading_images.contains(&open.id)
            || state.online != crate::state::Online::Open
            || node.is_none();
        for button in [&self.send_button, &self.steer_button] {
            if disabled {
                let _ = button.set_attribute("disabled", "");
            } else {
                let _ = button.remove_attribute("disabled");
            }
        }
        let _ = self.send_button.set_attribute(
            "aria-label",
            if sending {
                "Sending message"
            } else if busy {
                "Queue message"
            } else {
                "Send message"
            },
        );
        let _ = self
            .send_button
            .class_list()
            .toggle_with_force("queue-send", busy);
        self.intent.set_hidden(!busy || empty);
        self.send_button.set_text_content(Some(if sending {
            "…"
        } else if busy {
            "Queue"
        } else {
            "↑"
        }));
        self.steer_button.set_hidden(!busy || empty);
        self.steer_button.set_text_content(Some(
            if node.is_some_and(|n| n.delivery.native_steering)
                && state.images.get(&open.id).is_none_or(Vec::is_empty)
            {
                "Steer now"
            } else {
                "Stop & continue"
            },
        ));
        changed
    }

    fn draw_turns(
        &mut self,
        document: &Document,
        open: &crate::state::OpenChat,
        dirty: &crate::state::Dirty,
        fresh: bool,
        now: u64,
    ) {
        let node = open.view.as_ref().and_then(|view| view.chat());
        if let Some(node) = node {
            let drawn = self.turns.child_element_count() as usize;
            if fresh || dirty.all_turns {
                self.turns.set_text_content(None);
                for index in 0..node.turns.len() {
                    let turn = view::turn(node, open, index, now);
                    let _ = self.turns.append_child(&dom::element_of(document, &turn));
                }
            } else {
                for index in (0..dirty.prepended.min(node.turns.len())).rev() {
                    let turn = dom::element_of(document, &view::turn(node, open, index, now));
                    let _ = self
                        .turns
                        .insert_before(&turn, self.turns.first_child().as_ref());
                }
                for &absolute in &dirty.turns {
                    let Some(index) = absolute.checked_sub(node.first_turn as usize) else {
                        continue;
                    };
                    if index >= node.turns.len() {
                        continue;
                    }
                    let turn = dom::element_of(document, &view::turn(node, open, index, now));
                    match self
                        .turns
                        .children()
                        .item(u32::try_from(index).unwrap_or(u32::MAX))
                    {
                        Some(old) if index < drawn + dirty.prepended => {
                            let _ = self.turns.replace_child(&turn, &old);
                        }
                        _ => {
                            // A turn added: those before it are drawn already.
                            let _ = self.turns.append_child(&turn);
                        }
                    }
                }
            }
        } else if fresh {
            self.turns.set_text_content(None);
            let _ = self.turns.append_child(&dom::element_of(
                document,
                &el("p").class("muted").text("Opening…"),
            ));
        }
    }
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
    /// The form question shown, by its chat and number, and when it came,
    /// in milliseconds: for [`FORM_PAUSE`] after, its buttons don't answer
    /// it, as a tap meant for what was there before mustn't.
    form_since: Option<(TerminalId, u64, f64)>,
    /// When the chat shown is next drawn for its seconds to count, while
    /// its agent works, in milliseconds since the Unix epoch.
    count_at: f64,
    /// The terminal control a finger is on, and where it came down.
    pressed: Option<(Action, (f64, f64))>,
    /// Counts the phone's screen changes, so the terminal is fitted again
    /// only once it settles.
    refits: u32,
    /// A frame is asked for, to draw in.
    frame_asked: bool,
    viewport: viewport::Tracking,
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

fn history_button(document: &Document) -> HtmlElement {
    dom::element_of(
        document,
        &el("button")
            .class("history-more")
            .action(&Action::OlderChat)
            .attr("hidden", "")
            .text("Load earlier messages"),
    )
    .unchecked_into()
}

fn near_bottom(scroller: &DomElement) -> bool {
    scroller.scroll_top() + scroller.client_height() >= scroller.scroll_height() - 80
}

fn composer_inputs(document: &Document) -> (HtmlTextAreaElement, HtmlElement, HtmlElement) {
    let textarea: HtmlTextAreaElement = document
        .create_element("textarea")
        .expect("textarea")
        .unchecked_into();
    textarea.set_id("message");
    textarea.set_rows(1);
    let _ = textarea.set_attribute("placeholder", "Message your agent…");
    let _ = textarea.set_attribute("enterkeyhint", "enter");
    let _ = textarea.set_attribute("aria-label", "Message your agent");
    let send_button: HtmlElement = dom::element_of(
        document,
        &el("button").class("send").action(&Action::Send).text("↑"),
    )
    .unchecked_into();
    let steer_button: HtmlElement = dom::element_of(
        document,
        &el("button")
            .class("quiet steer")
            .attr("hidden", "")
            .action(&Action::Steer)
            .text("Steer now"),
    )
    .unchecked_into();
    (textarea, send_button, steer_button)
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
            form_since: None,
            count_at: 0.0,
            pressed: None,
            refits: 0,
            frame_asked: false,
            viewport: viewport::Tracking::default(),
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

fn listen_for_chat_input(root: &DomElement) {
    listen(root, "compositionend", false, |event| {
        if target_id(&event).is_some_and(|id| id == "model-search") {
            with_app(App::draw_soon);
        }
    });
    listen(root, "input", false, |event| {
        if target_id(&event).is_some_and(|id| id == "model-search") {
            with_app(|app| {
                let query = app.input("model-search").unwrap_or_default();
                if let Some(open) = &mut app.state.open {
                    open.picker.query = query;
                }
                if !event
                    .dyn_ref::<InputEvent>()
                    .is_some_and(InputEvent::is_composing)
                {
                    app.draw_soon();
                }
            });
        }
        if target_id(&event).is_some_and(|id| id == "home-search") {
            with_app(|app| {
                app.state.home_query = app.input("home-search").unwrap_or_default();
                app.state.dirty.home = true;
                app.draw_soon();
            });
        }
        if let Some(area) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlTextAreaElement>().ok())
        {
            grow(&area);
            with_app(|app| {
                if let Some(open) = &app.state.open {
                    app.state.drafts.insert(open.id, area.value());
                    app.save_draft(open.id);
                    app.draw_soon();
                }
            });
        }
    });
    listen(root, "keydown", false, |event| {
        let Some(key) = event.dyn_ref::<KeyboardEvent>() else {
            return;
        };
        if key.is_composing() {
            return;
        }
        let handled = with_app(|app| app.sheet_key(key)).unwrap_or(false);
        if handled {
            return;
        }
        if matches!(key.key().as_str(), "Enter" | " ") {
            let target = event.target().and_then(|t| t.dyn_into::<DomElement>().ok());
            if let Some(target) =
                target.filter(|t| t.get_attribute("role").as_deref() == Some("button"))
                && let Some(action) = target
                    .get_attribute("data-action")
                    .and_then(|a| Action::decode(&a))
            {
                event.prevent_default();
                with_app(|app| app.act(action));
                return;
            }
        }
        let in_message = target_id(&event).is_some_and(|id| id == "message");
        if in_message && key.key() == "Enter" && (key.meta_key() || key.ctrl_key()) {
            event.prevent_default();
            with_app(|app| {
                let busy = app
                    .state
                    .open
                    .as_ref()
                    .and_then(|o| o.view.as_ref()?.chat())
                    .is_some_and(|n| n.state == clackatoo_protocol::ChatState::Answering);
                app.act(if busy { Action::Steer } else { Action::Send });
            });
        }
    });
}

/// How long after a form comes, in milliseconds, before its buttons answer
/// it, as in the window.
const FORM_PAUSE: f64 = 500.0;

/// What's filled in in `within`'s inputs, by their ids: each one's value
/// and whether it's ticked.
fn kept_inputs(within: &DomElement) -> Vec<(String, String, bool)> {
    let mut kept = Vec::new();
    let Ok(inputs) = within.query_selector_all("input[id]") else {
        return kept;
    };
    for index in 0..inputs.length() {
        if let Some(input) =
            (inputs.item(index)).and_then(|i| i.dyn_into::<HtmlInputElement>().ok())
        {
            kept.push((input.id(), input.value(), input.checked()));
        }
    }
    kept
}

/// Puts `kept` back in `within`'s inputs of the same ids.
fn put_back_inputs(within: &DomElement, kept: &[(String, String, bool)]) {
    for (id, value, checked) in kept {
        let found = within
            .query_selector(&format!("input[id=\"{id}\"]"))
            .ok()
            .flatten()
            .and_then(|input| input.dyn_into::<HtmlInputElement>().ok());
        if let Some(input) = found {
            input.set_value(value);
            input.set_checked(*checked);
        }
    }
}

/// Listens for what happens on the page: taps, picks from menus, parts
/// opened and folded, typing, the page hidden or shown, the address
/// changed, and notifications tapped.
fn listen_to_page(window: &Window, document: &Document, root: &DomElement) {
    listen(root, "click", false, |event| {
        let target = event
            .target()
            .and_then(|target| target.dyn_into::<DomElement>().ok());
        if let Some(element) = target.as_ref().and_then(|t| {
            t.closest("[data-pick], [data-picker-tab], [data-model-provider], [data-favorite]")
                .ok()
                .flatten()
        }) {
            with_app(|app| app.picker_clicked(&element));
            return;
        }
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
    listen_for_chat_input(root);
    photos::listen(root);
    // Scrolling doesn't bubble: heard on the way down.
    listen(root, "scroll", true, |_| {
        with_app(App::scrolled);
    });
    viewport::listen(window, document);
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
        let key = terminal_control_target(&event)
            .and_then(|key| key.get_attribute("data-action"))
            .and_then(|action| Action::decode(&action));
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
                    app.act(key);
                }
            });
        }));
    }
    // With a mouse, a key pressed keeps the box focused too.
    kept.push(listen_kept(keys, "mousedown", |event| {
        let on_key = terminal_control_target(&event).is_some();
        if on_key {
            event.prevent_default();
        }
    }));
    kept
}

fn terminal_control_target(event: &Event) -> Option<DomElement> {
    event
        .target()?
        .dyn_into::<DomElement>()
        .ok()?
        .closest(".key, .terminal-hide, .term-controls-toggle, .terminal-controls button")
        .ok()
        .flatten()
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

/// How many minutes ahead of UTC the phone's clock is at `at`, in
/// milliseconds since the Unix epoch: summer time, or not, as it is then.
fn zone_offset(at: u64) -> i64 {
    #[allow(clippy::cast_precision_loss, reason = "milliseconds since 1970 fit")]
    let at = js_sys::Date::new(&JsValue::from_f64(at as f64));
    #[allow(clippy::cast_possible_truncation, reason = "minutes, less than a day")]
    let behind = at.get_timezone_offset() as i64;
    -behind
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
        let submitting = self.state.submitting.clone();
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
        // Acknowledgments may arrive after navigation. Persist the cleared
        // draft for that chat too, so a later reload cannot resurrect it.
        for chat in submitting {
            if !self.state.submitting.contains(&chat) {
                self.save_draft(chat);
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
                Err(err) => {
                    web_sys::console::error_1(&format!("{err}").into());
                    self.lost();
                    return;
                }
            }
        }
        if socket.ws.send_with_u8_array(&bytes).is_err() {
            self.lost();
        } else {
            self.check_keys_later();
        }
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
            if self.document.get_element_by_id("chat-stage").is_none()
                && let Some(results) = self.document.get_element_by_id("home-results")
            {
                let content = crate::dashboard::results(&self.state, self.notifications(), now);
                dom::replace_children(&self.document, &results, &el("div").child(content));
                if let Ok(Some(old)) = self.root.query_selector(".home-bar .online")
                    && let Some(parent) = old.parent_node()
                {
                    let _ = parent.replace_child(
                        &dom::element_of(&self.document, &view::online(self.state.online)),
                        &old,
                    );
                }
            } else {
                let home = view::home(&self.state, self.notifications(), now);
                dom::replace_children(&self.document, &self.root, &el("div").child(home));
            }
        }
        if self.state.open.is_some() && self.state.dirty.home {
            self.draw_sidebar(now);
        }
        self.state.drawn();
        self.send();
    }

    fn draw_sidebar(&self, now: u64) {
        if let Some(results) = self.document.get_element_by_id("home-results") {
            let content = crate::dashboard::results(&self.state, Notifications::On, now);
            dom::replace_children(&self.document, &results, &el("div").child(content));
        }
        if let Ok(Some(old)) = self.root.query_selector(".home-bar .online")
            && let Some(parent) = old.parent_node()
        {
            let _ = parent.replace_child(
                &dom::element_of(&self.document, &view::online(self.state.online)),
                &old,
            );
        }
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
        if let Some(page) = &self.page {
            page.latest.set_hidden(near_bottom(&page.scroller));
            if page.scroller.scroll_top() < 240 {
                self.state.older_chat();
                self.send();
            }
        }
        if self.term_page.as_ref().is_some_and(TermPage::near_top) {
            self.state.older_history();
            self.send();
        }
    }

    /// The phone's screen changed size, or its keyboard came or went: the
    /// terminal's page is put above the keyboard, its font fitted again.
    fn viewport_changed(&mut self) {
        if let Some(page) = &self.page {
            let bottom = near_bottom(&page.scroller);
            if let Some(viewport) = self.window.visual_viewport() {
                let _ = page
                    .main
                    .style()
                    .set_property("--viewport-top", &format!("{}px", viewport.offset_top()));
                let layout_height = self
                    .window
                    .inner_height()
                    .ok()
                    .and_then(|h| h.as_f64())
                    .unwrap_or(viewport.height());
                let layout_height = self
                    .document
                    .document_element()
                    .map_or(layout_height, |root| {
                        layout_height.max(f64::from(root.client_height()))
                    });
                let obscured = viewport.height() < layout_height - 1.0;
                page.main.set_class_name(if obscured {
                    "chat-page keyboard-open"
                } else {
                    "chat-page"
                });
                // Always fit the visible rectangle. Safari can pan it to the
                // layout's bottom, or report innerHeight already reduced, while
                // 100dvh still extends behind the keyboard. Neither proves that
                // the keyboard closed. Dismissal supplies a new visible height.
                let _ = page
                    .main
                    .style()
                    .set_property("--viewport-height", &format!("{}px", viewport.height()));
                let _ = page
                    .main
                    .style()
                    .set_property("height", &format!("{}px", viewport.height()));
                let _ = page
                    .main
                    .style()
                    .set_property("top", &format!("{}px", viewport.offset_top()));
            }
            if bottom {
                page.scroller.set_scroll_top(page.scroller.scroll_height());
            }
        }
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
            let ask = if lines == 1 {
                "Paste this? The program hasn't asked for pastes to be marked, so the line \
                 before the last runs as if typed."
                    .to_owned()
            } else {
                format!(
                    "Paste this? The program hasn't asked for pastes to be marked, so the \
                     {lines} lines before the last run as if typed."
                )
            };
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
        self.note_form_shown();
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
        let at_bottom = near_bottom(&page.scroller);
        let old_height = page.scroller.scroll_height();
        let old_top = page.scroller.scroll_top();
        let prepended = self.state.dirty.prepended > 0;
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
        page.history
            .set_hidden(node.is_none_or(|n| n.first_turn == 0));
        if open.history_pending.is_some() {
            let _ = page.history.set_attribute("disabled", "");
        } else {
            let _ = page.history.remove_attribute("disabled");
        }
        page.history
            .set_text_content(Some(if open.history_pending.is_some() {
                "Loading earlier messages…"
            } else {
                "Load earlier messages"
            }));
        page.draw_turns(&self.document, open, &self.state.dirty, fresh, now);
        let online = self.state.online;
        let working = node.and_then(|node| view::working(node, online, now));
        let next_second = node.and_then(|node| view::next_second(node, online, now));
        if working != page.shown_working {
            page.working.set_hidden(working.is_none());
            if let Some((doing, seconds)) = &working {
                page.doing.set_text_content(Some(doing));
                page.seconds.set_text_content(Some(seconds));
            }
            page.shown_working.clone_from(&working);
        }
        let footer = view::footer(&self.state, node);
        if fresh || footer != page.shown_footer {
            // A form being filled in keeps what's filled in, as the rest
            // of what's under the turns is drawn again; not into another
            // chat's, drawn afresh.
            let kept = if fresh {
                Vec::new()
            } else {
                kept_inputs(&page.footer)
            };
            dom::replace_children(
                &self.document,
                &page.footer,
                &el("div").child(footer.clone()),
            );
            put_back_inputs(&page.footer, &kept);
            page.shown_footer = footer;
        }
        let menus = node.map_or_else(
            || el("div"),
            |node| view::box_bar(node, open, now, &zone_offset),
        );
        if fresh || menus != page.shown_menus {
            dom::replace_panel(&self.document, &page.menus, &el("div").child(menus.clone()));
            page.shown_menus = menus;
        }
        let draft_changed = page.draw_controls(&self.document, &self.state, open);
        if prepended && !fresh {
            page.scroller
                .set_scroll_top(old_top + page.scroller.scroll_height() - old_height);
        } else if at_bottom || fresh {
            page.scroller.set_scroll_top(page.scroller.scroll_height());
        }
        page.latest.set_hidden(near_bottom(&page.scroller));
        let needs_history = page.scroller.scroll_height() <= page.scroller.client_height() + 1;
        let id = open.id;
        if needs_history {
            self.state.older_chat();
            self.send();
        }
        if draft_changed {
            self.save_draft(id);
        }
        self.focus_sheet();
        if fresh {
            self.viewport_changed();
        }
        if let Some(at) = next_second {
            self.count_later(at);
        }
    }

    /// Draws the chat shown again at `at`, in milliseconds since the Unix
    /// epoch, for the seconds its agent has been working to count: once,
    /// however many draws ask, and not while the page isn't seen, as the
    /// page is drawn again when it is. Nothing asks once the agent stops,
    /// so nothing runs.
    fn count_later(&mut self, at: u64) {
        #[allow(clippy::cast_precision_loss, reason = "a time in milliseconds")]
        let at = at as f64;
        if at <= self.count_at || self.document.visibility_state() != VisibilityState::Visible {
            return;
        }
        self.count_at = at;
        let count = Closure::once_into_js(|| {
            with_app(App::draw_soon);
        });
        let wait = (at - js_sys::Date::now()).max(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "milliseconds, at most a second"
        )]
        let wait = wait as i32;
        let _ = self
            .window
            .set_timeout_with_callback_and_timeout_and_arguments_0(count.unchecked_ref(), wait);
    }

    /// Draws the list again once `wait` milliseconds, and a moment more,
    /// have gone: as a second tap's time to dismiss a background tab runs
    /// out, so its button asks no more.
    fn redraw_home_after(&self, wait: f64) {
        let redraw = Closure::once_into_js(|| {
            with_app(|app| {
                app.state.dirty.home = true;
                app.draw_soon();
            });
        });
        #[allow(clippy::cast_possible_truncation, reason = "a few seconds")]
        let wait = (wait + 50.0) as i32;
        let _ = self
            .window
            .set_timeout_with_callback_and_timeout_and_arguments_0(redraw.unchecked_ref(), wait);
    }

    /// The page showing a chat: its parts, empty, and the box a message is
    /// typed in.
    fn chat_page(&self) -> ChatPage {
        let document = &self.document;
        let main: HtmlElement =
            dom::element_of(document, &el("main").class("chat-page")).unchecked_into();
        let part = |class: &str| dom::element_of(document, &el("div").class(class));
        let header = part("header-slot");
        let turns = part("turns");
        let footer = part("footer-slot");
        let scroller = part("conversation");
        let queue = part("queue-slot");
        let latest: HtmlElement = dom::element_of(
            document,
            &el("button")
                .class("latest")
                .attr("hidden", "")
                .action(&Action::Latest)
                .text("↓ Latest"),
        )
        .unchecked_into();
        let dock = part("dock");
        let composer = part("composer");
        let photos = photos::Controls::new(document);
        let stage = dom::element_of(
            document,
            &el("section").class("chat-stage").attr("id", "chat-stage"),
        );
        let sidebar = dom::element_of(
            document,
            &el("aside")
                .class("session-sidebar")
                .attr("aria-label", "Sessions")
                .child(crate::dashboard::home(
                    &self.state,
                    Notifications::On,
                    epoch_millis(),
                )),
        );
        let intent: HtmlElement = part("composer-intent").unchecked_into();
        intent.set_hidden(true);
        let intent_text = dom::element_of(document, &el("span").text("Send to queue"));
        let _ = intent.append_child(&intent_text);
        let menus = part("menus-slot");
        let row = part("row");
        let (textarea, send_button, steer_button) = composer_inputs(document);
        let _ = row.append_child(&menus);
        photos.mount(&row, &composer);
        let _ = intent.append_child(&steer_button);
        let _ = row.append_child(&send_button);
        // At the top of the box's bar, which stays at the screen's foot,
        // so it's seen wherever the turns are scrolled to. Hidden until
        // the agent works.
        let working: HtmlElement = dom::element_of(
            document,
            &el("p")
                .class("working")
                .attr("hidden", "")
                .child(view::dots(epoch_millis())),
        )
        .unchecked_into();
        let doing = dom::element_of(document, &el("span").class("doing"));
        let seconds = dom::element_of(document, &el("span").class("seconds"));
        let _ = working.append_child(&doing);
        let _ = working.append_child(&seconds);
        let _ = dock.append_child(&footer);
        let _ = dock.append_child(&working);
        let _ = dock.append_child(&queue);
        let _ = composer.append_child(&intent);
        let _ = composer.append_child(&textarea);
        let _ = composer.append_child(&row);
        let history = history_button(document);
        let _ = scroller.append_child(&history);
        let _ = scroller.append_child(&turns);

        let _ = dock.append_child(&composer);
        for part in [&header, &scroller, latest.unchecked_ref(), &dock] {
            let _ = stage.append_child(part);
        }
        let _ = main.append_child(&sidebar);
        let _ = main.append_child(&stage);
        self.root.set_text_content(None);
        let _ = self.root.append_child(&main);
        ChatPage {
            photos,
            main,
            scroller,
            latest,
            history,
            send_button,
            steer_button,
            intent,
            queue,
            shown_queue: el("div"),
            header,
            turns,
            working,
            doing,
            seconds,
            shown_working: None,
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
            Action::Fit
            | Action::Unfit
            | Action::Type
            | Action::HideKeyboard
            | Action::TerminalControls
            | Action::SmallerText
            | Action::LargerText
            | Action::ResetText => self.terminal_control(action),
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
            Action::Fill(question) => self.fill(question),
            Action::Decline(question) => self.answer_form(question, FormReply::Decline),
            // The link opens by itself, as links do.
            Action::OpenLink(question) => self.open_link(question),
            Action::CancelActivity(index) => self.state.cancel_activity(index),
            Action::Usage | Action::Settings => self.toggle_sheet(action),
            Action::Filter(filter) => self.state.filter(filter),
            Action::Bring(tab) => {
                if let Some(terminal) = self.state.bring_back(tab) {
                    self.go(&format!("#term/{terminal}"));
                }
            }
            Action::Dismiss(tab) => {
                self.state.dismiss(tab);
                if self.state.confirming(tab) {
                    self.redraw_home_after(crate::state::DISMISS_FOR);
                }
            }
            Action::OlderChat => {
                self.state.older_chat();
            }
            Action::Latest => {
                if let Some(page) = &self.page {
                    page.scroller.set_scroll_top(page.scroller.scroll_height());
                }
            }
            Action::RemoveQueued(id) => self
                .state
                .chat(ChatAction::RemoveQueued { id }, "removing queued message"),
            Action::SendQueued(id) => self.state.chat(
                ChatAction::SendQueued { id, steer: true },
                "delivering queued message",
            ),
            Action::Stop => self.state.chat(ChatAction::Stop, "stopping the agent"),
            Action::Command(at) => self.put_command(at),
            Action::AttachPhotos => self.attach_photos(),
            Action::RemovePhoto(id) => self.remove_photo(id),
            Action::Send | Action::Steer => {
                self.state.submit(action == Action::Steer);
            }
            Action::Notify => self.enable_notifications(),
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

    fn toggle_sheet(&mut self, action: Action) {
        if action != Action::Usage {
            let saved = self
                .window
                .local_storage()
                .ok()
                .flatten()
                .and_then(|s| s.get_item("clackatoo.model-favorites").ok().flatten())
                .and_then(|text| js_sys::JSON::parse(&text).ok())
                .filter(js_sys::Array::is_array)
                .map(|v| {
                    js_sys::Array::from(&v)
                        .iter()
                        .take(512)
                        .filter_map(|v| v.as_string())
                        .collect()
                });
            if let Some(open) = &mut self.state.open {
                open.picker.tab = crate::settings::Tab::Model;
                open.picker.query.clear();
                if let Some(saved) = saved {
                    open.picker.favorites = saved;
                }
                open.picker.provider = if open.picker.favorites.is_empty() {
                    open.view
                        .as_ref()
                        .and_then(|v| v.chat())
                        .map_or_else(String::new, |n| n.agent.clone())
                } else {
                    "Favorites".into()
                };
            }
        }
        let fold = if action == Action::Usage {
            view::USAGE_FOLD
        } else {
            view::SETTINGS_FOLD
        };
        self.state.toggle_fold(fold);
        if let Some(open) = &mut self.state.open {
            let other = if fold == view::USAGE_FOLD {
                view::SETTINGS_FOLD
            } else {
                view::USAGE_FOLD
            };
            open.folds.insert(other.into(), false);
        }
        let opening = self
            .state
            .open
            .as_ref()
            .is_some_and(|o| o.folds.get(fold).copied().unwrap_or(false));
        if opening && action == Action::Usage {
            self.state
                .chat(ChatAction::RefreshUsage { force: true }, "refreshing usage");
        } else if opening {
            self.menu_opened();
        }
        if !opening {
            self.focus_settings_trigger();
        }
        self.draw_soon();
    }

    fn picker_clicked(&mut self, element: &DomElement) {
        if let Some(value) = element.get_attribute("data-pick") {
            if let Some(pick) = element
                .get_attribute("data-setting")
                .and_then(|id| Pick::decode(&id, &value))
            {
                self.picked(pick);
            }
        } else if let Some(open) = &mut self.state.open {
            if let Some(tab) = element.get_attribute("data-picker-tab") {
                open.picker.tab = match tab.as_str() {
                    "permissions" => crate::settings::Tab::Permissions,
                    "tools" => crate::settings::Tab::Tools,
                    _ => crate::settings::Tab::Model,
                };
            } else if let Some(provider) = element.get_attribute("data-model-provider") {
                open.picker.provider = provider;
                open.picker.query.clear();
            } else if let Some(key) = element.get_attribute("data-favorite") {
                if !open.picker.favorites.remove(&key) && open.picker.favorites.len() < 512 {
                    open.picker.favorites.insert(key);
                }
                if let Ok(Some(storage)) = self.window.local_storage() {
                    let values: Vec<_> = open
                        .picker
                        .favorites
                        .iter()
                        .map(|s| json_string(s))
                        .collect();
                    let _ = storage.set_item(
                        "clackatoo.model-favorites",
                        &format!("[{}]", values.join(",")),
                    );
                }
            }
        }
        if element.get_attribute("data-picker-tab").as_deref() == Some("tools") {
            self.state.inspect_capabilities();
            self.send();
        }
        self.draw_soon();
    }

    fn enable_notifications(&mut self) {
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

    /// Puts the agent's slash command at `at` in the open chat's list in
    /// the message being typed, with a space after it, as a tap on it in
    /// the list `/` shows does.
    fn put_command(&mut self, at: usize) {
        let Some(open) = &self.state.open else {
            return;
        };
        let id = open.id;
        let name = (open.view.as_ref().and_then(|view| view.chat()))
            .and_then(|node| node.commands.get(at))
            .map(|command| command.name.clone());
        if let Some(name) = name {
            self.state.drafts.insert(id, format!("/{name} "));
            self.save_draft(id);
            self.state.dirty.footer = true;
            self.draw_soon();
        }
    }

    fn save_draft(&self, chat: TerminalId) {
        if let Ok(Some(storage)) = self.window.session_storage() {
            let key = format!("clackatoo.draft.{chat}");
            match self.state.drafts.get(&chat).filter(|s| !s.is_empty()) {
                Some(text) => {
                    let _ = storage.set_item(&key, text);
                }
                None => {
                    let _ = storage.remove_item(&key);
                }
            }
        }
    }

    fn focus_settings_trigger(&self) {
        if let Some(trigger) = self.document.get_element_by_id("settings-trigger")
            && let Some(html) = trigger.dyn_ref::<HtmlElement>()
        {
            let _ = html.focus();
        }
    }

    fn focus_sheet(&self) {
        if let Ok(Some(sheet)) = self.root.query_selector("[role=dialog]") {
            let active = self.document.active_element();
            if !active.is_some_and(|active| sheet.contains(Some(&active)))
                && let Ok(Some(close)) = sheet.query_selector(".sheet-close")
                && let Some(html) = close.dyn_ref::<HtmlElement>()
            {
                let _ = html.focus();
            }
        }
    }

    fn sheet_key(&mut self, key: &KeyboardEvent) -> bool {
        let Ok(Some(sheet)) = self.root.query_selector("[role=dialog]") else {
            return false;
        };
        if key.key() == "Escape" {
            key.prevent_default();
            if let Some(open) = &mut self.state.open {
                open.folds.insert(view::SETTINGS_FOLD.into(), false);
                open.folds.insert(view::USAGE_FOLD.into(), false);
            }
            self.focus_settings_trigger();
            self.draw_soon();
            return true;
        }
        if key.key() == "Tab"
            && let Ok(items) = sheet.query_selector_all(
                "button:not([disabled]), select:not([disabled]), input:not([disabled])",
            )
        {
            let first = items.item(0).and_then(|n| n.dyn_into::<HtmlElement>().ok());
            let last = items
                .item(items.length().saturating_sub(1))
                .and_then(|n| n.dyn_into::<HtmlElement>().ok());
            let active = self.document.active_element();
            let target = if key.shift_key() && active.as_ref() == first.as_ref().map(AsRef::as_ref)
            {
                last
            } else if !key.shift_key() && active.as_ref() == last.as_ref().map(AsRef::as_ref) {
                first
            } else {
                None
            };
            if let Some(target) = target {
                key.prevent_default();
                let _ = target.focus();
            }
        }
        false
    }

    /// The form question `question` asks, if it's still the one asked.
    fn form_asked(&self, question: u64) -> Option<clackatoo_protocol::Form> {
        let asked = (self.state.open.as_ref())
            .and_then(|open| open.view.as_ref()?.chat()?.question.clone())?;
        (asked.id == question).then_some(asked.form).flatten()
    }

    /// Submits the form question `question` asks, as its inputs are filled
    /// in, if that fits it; or says why not, under it.
    fn fill(&mut self, question: u64) {
        let Some(form) = self.form_asked(question) else {
            return;
        };
        let answers =
            view::form_answers(question, &form, |id| self.input(id), |id| self.checked(id));
        if let Some(why) = form.problem(&answers) {
            if let Some(line) = self.document.get_element_by_id(view::FORM_PROBLEM) {
                line.set_text_content(Some(&format!("Can't submit yet: {why}")));
            }
            return;
        }
        self.answer_form(question, FormReply::Accept(answers));
    }

    /// Notes when the form question shown came, if it's a new one.
    fn note_form_shown(&mut self) {
        let asked = self.state.open.as_ref().and_then(|open| {
            let question = open.view.as_ref()?.chat()?.question.as_ref()?;
            question.form.is_some().then_some((open.id, question.id))
        });
        if asked != self.form_since.map(|(chat, question, _)| (chat, question)) {
            self.form_since = asked.map(|(chat, question)| (chat, question, js_sys::Date::now()));
        }
    }

    /// Answers the form question `question` asks with `reply`, if it's
    /// still the one asked, and came long enough ago.
    fn answer_form(&mut self, question: u64, reply: FormReply) {
        if self.just_asked(question) {
            return;
        }
        if self.form_asked(question).is_some() {
            self.state
                .chat(ChatAction::Fill { question, reply }, "answering");
        }
    }

    /// Whether the form question `question` came too short a time ago to
    /// be answered: if so, says to tap again.
    fn just_asked(&self, question: u64) -> bool {
        let just = self.form_since.is_some_and(|(_, asked, since)| {
            asked == question && js_sys::Date::now() - since < FORM_PAUSE
        });
        if just && let Some(line) = self.document.get_element_by_id(view::FORM_PROBLEM) {
            line.set_text_content(Some("A question just came: tap again to answer it."));
        }
        just
    }

    /// Opens the link question `question` asks to be opened, in a tab of
    /// its own, and answers it, unless it came too short a time ago, or
    /// isn't a plain web link (checked here too: the daemon may not be
    /// this one).
    fn open_link(&mut self, question: u64) {
        if self.just_asked(question) {
            return;
        }
        let Some(clackatoo_protocol::Form {
            ask: clackatoo_protocol::FormAsk::Url(url),
            ..
        }) = self.form_asked(question)
        else {
            return;
        };
        if !clackatoo_protocol::plain_web_link(&url) {
            return;
        }
        // In the tap's own handler, so a phone's browser lets it open.
        let _ = self.window.open_with_url_and_target_and_features(
            &url,
            "_blank",
            "noopener,noreferrer",
        );
        self.answer_form(question, FormReply::Accept(Vec::new()));
    }

    /// Whether the box or button with `id` is ticked.
    fn checked(&self, id: &str) -> bool {
        (self.document.get_element_by_id(id))
            .and_then(|input| input.dyn_into::<HtmlInputElement>().ok())
            .is_some_and(|input| input.checked())
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
            if self.state.open.as_ref().is_none_or(|open| open.id != chat) {
                self.page = None;
            }
            if !self.state.drafts.contains_key(&chat)
                && let Ok(Some(storage)) = self.window.session_storage()
                && let Ok(Some(draft)) = storage.get_item(&format!("clackatoo.draft.{chat}"))
            {
                self.state.drafts.insert(chat, draft);
            }
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
        // Seen again: drawn, so an agent's seconds count on from now.
        if here {
            self.viewport_settling();
            self.draw_soon();
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
