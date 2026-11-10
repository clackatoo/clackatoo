//! The client, as config calls it: the process of windows, which runs one
//! Lua for every window, keeps one store of options and one set of
//! bindings, and tells each window what's for it.
//!
//! The config loads once for the process, not once a window: a window that
//! opens later joins the Lua already running, and goes by the bindings,
//! handlers and options set so far, without the config running again.
//!
//! Everything here is the event loop's, shared by the windows on it as an
//! [`Rc`]. Lua tells the windows things on one channel, each addressed (see
//! [`To`]): whichever window reads the channel next keeps what's for the
//! client as a whole once, here, and queues what's for each window for it
//! to take. What's kept here changes for every window at once: each looks
//! at the counts of changes ([`Client::keys_changed`], the options' own) to
//! know when to follow.

use std::cell::{Cell, Ref, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Instant;

use clackatoo_protocol::MachineId;

use crate::desk::{Binding, Keys};
use crate::lua::{
    self, DECISION_TIME, FromLua, Listen, Lua, Origin, Said, Scope, Source, To, ToLua,
};
use crate::options::settings_lua::Mapping;
use crate::options::{SetBy, Shared, Store};

/// How many daemon events are remembered, to tell each one once.
const HEARD: usize = 256;

/// The process of windows, as config sees it.
#[derive(Clone, Default)]
pub(crate) struct Client(Rc<Inner>);

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Client")
    }
}

#[derive(Default)]
struct Inner {
    /// Where the config is, if anywhere.
    source: RefCell<Option<Source>>,
    /// Makes what wakes the windows, when Lua has something for them.
    wake: RefCell<Option<Box<dyn Fn() -> lua::Wake>>>,
    /// The config's Lua, if there's a config.
    lua: RefCell<Option<Lua>>,
    /// The last key Lua was asked about.
    seq: Cell<u64>,
    /// The windows, by number, each with what Lua has said for it that it
    /// hasn't taken yet.
    windows: RefCell<BTreeMap<u32, Vec<Said>>>,
    /// The windows a chat has Control of now: what the config asks of
    /// every window isn't kept for windows opened later while one is.
    guarded: RefCell<std::collections::BTreeSet<u32>>,
    /// The last window's number.
    numbered: Cell<u32>,
    /// The window with the desktop's focus, if one has.
    focused: Cell<Option<u32>>,
    /// The options, as every window shares them, and each window's own.
    options: OptionsCell,
    /// What keys are bound to, and what bound each of the config's: what
    /// each window's keymap is a copy of.
    keys: RefCell<Keys>,
    /// How many times the bindings have changed.
    keys_changed: Cell<u64>,
    /// Lua's keypress handlers, in the order they were added.
    handlers: RefCell<Vec<(u64, Scope)>>,
    /// Lua's handlers of chats' turns ending.
    turn_handlers: RefCell<Vec<u64>>,
    /// When the config started loading, until it's said it's loaded.
    running: Cell<Option<Instant>>,
    /// How many times the config has been run again from the start.
    reloads: Cell<u64>,
    /// The config was run again by itself last, as its files changed, not
    /// by `:reload` (see [`crate::lua::Origin::unattended`]).
    unattended: Cell<bool>,
    /// What the config asked of each window's daemon, `client.connect`'s
    /// machines and `client.forward`'s ports, for windows that open later.
    hosts: RefCell<Vec<(String, Option<bool>)>>,
    forwards: RefCell<Vec<clackatoo_daemon::forward::Spec>>,
    /// The daemon events told Lua lately, each by its daemon and what it
    /// was, as windows on the same daemon each hear them.
    heard: RefCell<VecDeque<Heard>>,
}

/// The store of options, made when it's first needed.
#[derive(Default)]
struct OptionsCell(RefCell<Option<Shared>>);

/// A daemon event, as told once: a chat's turn that ended, by the machine
/// the chat is on (if its daemon says), when it ended, and the chat's
/// title. Not by the chat's number: a daemon numbers another machine's
/// chats afresh as it passes them on, so windows on two daemons know one
/// chat by two numbers; but each hears when its turn ended as the chat's
/// own daemon said.
pub(crate) type Heard = (Option<MachineId>, u64, String);

impl Client {
    /// The client, with the options as the process starts them.
    pub(crate) fn new(options: Store) -> Self {
        Self::sharing(options.shared())
    }

    /// The client, with `options`, as every window shares them.
    pub(crate) fn sharing(options: Shared) -> Self {
        let client = Self::default();
        *client.0.options.0.borrow_mut() = Some(options);
        client
    }

    /// The options every window shares, and each window's own.
    pub(crate) fn options(&self) -> Shared {
        let mut options = self.0.options.0.borrow_mut();
        options
            .get_or_insert_with(|| Store::default().shared())
            .clone()
    }

    /// Runs the config in `source`, waking the windows with what `wake`
    /// makes when Lua has something for them.
    pub(crate) fn start(&self, source: Source, wake: Box<dyn Fn() -> lua::Wake>) {
        *self.0.source.borrow_mut() = Some(source);
        *self.0.wake.borrow_mut() = Some(wake);
        self.start_lua();
    }

    /// Starts Lua on the config, if there's one, telling it the windows.
    fn start_lua(&self) {
        let lua = {
            let source = self.0.source.borrow();
            let wake = self.0.wake.borrow();
            let (Some(source), Some(wake)) = (source.as_ref(), wake.as_ref()) else {
                return;
            };
            let options = (**self.options().borrow()).clone();
            let windows = self.0.windows.borrow().keys().copied().collect();
            let unattended = self.0.unattended.get();
            Lua::start_as(source.clone(), options, windows, wake(), unattended)
        };
        self.0.running.set(lua.as_ref().map(|_| Instant::now()));
        *self.0.lua.borrow_mut() = lua;
    }

    /// Whether there's a config's Lua.
    pub(crate) fn has_lua(&self) -> bool {
        self.0.lua.borrow().is_some()
    }

    /// The config's directory, if there's one.
    pub(crate) fn config_dir(&self) -> Option<std::path::PathBuf> {
        self.0.source.borrow().as_ref()?.dir().map(Into::into)
    }

    /// What wakes the windows, if anything does.
    pub(crate) fn waker(&self) -> Option<lua::Wake> {
        self.0.wake.borrow().as_ref().map(|wake| wake())
    }

    /// Makes a window, numbering it. It goes by what's been bound, set and
    /// listened for so far, and is asked what the config asked of every
    /// window's daemon.
    pub(crate) fn join(&self) -> u32 {
        let window = self.0.numbered.get() + 1;
        self.0.numbered.set(window);
        let mut asked: Vec<Said> = (self.0.hosts.borrow().iter())
            .map(|(host, share)| {
                let host = host.clone();
                (
                    Origin::default(),
                    FromLua::Connect {
                        host,
                        share: *share,
                    },
                )
            })
            .collect();
        asked.extend(
            (self.0.forwards.borrow().iter())
                .map(|spec| (Origin::default(), FromLua::Forward(spec.clone()))),
        );
        self.0.windows.borrow_mut().insert(window, asked);
        self.send(ToLua::Open { window });
        window
    }

    /// `window` has closed: what was its alone goes with it.
    pub(crate) fn leave(&self, window: u32) {
        self.0.windows.borrow_mut().remove(&window);
        self.0.guarded.borrow_mut().remove(&window);
        (self.0.handlers.borrow_mut()).retain(|(_, scope)| scope.window() != Some(window));
        self.options().borrow_mut().forget(window);
        if self.0.focused.get() == Some(window) {
            self.0.focused.set(None);
        }
        self.send(ToLua::Closed { window });
    }

    /// `window` has the desktop's focus now, or lost it.
    pub(crate) fn focus(&self, window: u32, focused: bool) {
        if focused {
            self.0.focused.set(Some(window));
        } else if self.0.focused.get() == Some(window) {
            self.0.focused.set(None);
        }
    }

    /// Notes whether a chat has Control of `window` now (see
    /// `Desk::controlled`).
    pub(crate) fn guard(&self, window: u32, guarded: bool) {
        let mut windows = self.0.guarded.borrow_mut();
        if guarded {
            windows.insert(window);
        } else {
            windows.remove(&window);
        }
    }

    /// Tells Lua something. Without a Lua, or one whose thread has ended,
    /// nobody hears.
    pub(crate) fn send(&self, message: ToLua) {
        if let Some(lua) = &*self.0.lua.borrow() {
            lua.send(message);
        }
    }

    /// What Lua has said for `window` since it last asked.
    pub(crate) fn take(&self, window: u32) -> Vec<Said> {
        let received = match &*self.0.lua.borrow() {
            Some(lua) => lua.received(),
            None => Vec::new(),
        };
        for (to, from, message) in received {
            self.route(to, from, message);
        }
        self.queued(window)
    }

    /// What's queued for `window`, taken.
    fn queued(&self, window: u32) -> Vec<Said> {
        (self.0.windows.borrow_mut().get_mut(&window))
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Sends the key `message` makes from a new sequence number, from
    /// `window`, and waits for Lua to say whether it stops. Returns that,
    /// if Lua said in time, and everything else Lua said for the window
    /// meanwhile, in order.
    pub(crate) fn decide(
        &self,
        window: u32,
        message: impl FnOnce(u64) -> ToLua,
    ) -> (Option<bool>, Vec<Said>) {
        let seq = self.0.seq.get() + 1;
        self.0.seq.set(seq);
        let sent = match &*self.0.lua.borrow() {
            Some(lua) => lua.send_now(message(seq)),
            None => false,
        };
        if !sent {
            return (None, self.queued(window));
        }
        let deadline = Instant::now() + DECISION_TIME;
        loop {
            let next = match &*self.0.lua.borrow() {
                Some(lua) => lua.next_within(deadline.saturating_duration_since(Instant::now())),
                None => Err(RecvTimeoutError::Disconnected),
            };
            match next {
                Ok((To::Window(to), _, FromLua::Decided { seq: decided, stop }))
                    if to == window && decided == seq =>
                {
                    return (Some(stop), self.queued(window));
                }
                // An answer that came too late.
                Ok((_, _, FromLua::Decided { .. })) => {}
                Ok((to, from, message)) => self.route(to, from, message),
                Err(RecvTimeoutError::Timeout) => {
                    log::warn!("Lua took too long to say whether a key goes on");
                    return (None, self.queued(window));
                }
                Err(RecvTimeoutError::Disconnected) => return (None, self.queued(window)),
            }
        }
    }

    /// Keeps what Lua said for the client, or queues it for the windows
    /// it's for, with where it came from.
    fn route(&self, to: To, from: Origin, message: FromLua) {
        let mut windows = self.0.windows.borrow_mut();
        match to {
            To::Client => {
                drop(windows);
                self.keep(message);
            }
            To::Window(window) => {
                if let Some(queue) = windows.get_mut(&window) {
                    queue.push((from, message));
                }
            }
            To::All => {
                // Kept for windows opened later only if no window refuses
                // it now, as one a chat has Control of does.
                let refused =
                    from.guarded || from.unattended || !self.0.guarded.borrow().is_empty();
                match &message {
                    _ if refused => {}
                    FromLua::Connect { host, share } => {
                        let mut hosts = self.0.hosts.borrow_mut();
                        match hosts.iter_mut().find(|(asked, _)| asked == host) {
                            Some(asked) => asked.1 = *share,
                            None => hosts.push((host.clone(), *share)),
                        }
                    }
                    FromLua::Disconnect(host) => {
                        self.0.hosts.borrow_mut().retain(|(asked, _)| asked != host);
                    }
                    FromLua::Forward(spec) => self.0.forwards.borrow_mut().push(spec.clone()),
                    _ => {}
                }
                for queue in windows.values_mut() {
                    queue.push((from, message.clone()));
                }
            }
            To::Focused => {
                let focused = (self.0.focused.get())
                    .filter(|window| windows.contains_key(window))
                    .or_else(|| windows.keys().next().copied());
                if let Some(queue) = focused.and_then(|window| windows.get_mut(&window)) {
                    queue.push((from, message));
                }
            }
        }
    }

    /// Keeps what Lua said for the client as a whole, once.
    fn keep(&self, message: FromLua) {
        match message {
            FromLua::Map {
                modes,
                keys,
                binding,
                desc,
                propagate,
                by,
            } => {
                let desc = desc.as_deref().and_then(said_in_panel);
                let mut map = self.0.keys.borrow_mut();
                let binding = Binding::Lua { binding, propagate };
                map.bind(&modes, &keys, binding, desc.as_deref());
                map.set_by(&modes, &keys, &by);
                drop(map);
                self.keys_changed();
            }
            FromLua::Remap {
                modes,
                keys,
                to,
                desc,
                by,
            } => {
                let desc = desc.as_deref().and_then(said_in_panel);
                let mut map = self.0.keys.borrow_mut();
                map.bind(&modes, &keys, Binding::Keys(to), desc.as_deref());
                map.set_by(&modes, &keys, &by);
                drop(map);
                self.keys_changed();
            }
            FromLua::Group { modes, keys, name } => {
                let name = name.as_deref().and_then(said_in_panel);
                (self.0.keys.borrow_mut()).name_group(&modes, &keys, name.as_deref());
                self.keys_changed();
            }
            FromLua::Unmap { modes, keys, by } => {
                let mut map = self.0.keys.borrow_mut();
                map.unbind(&modes, &keys);
                map.set_by(&modes, &keys, &by);
                drop(map);
                self.keys_changed();
            }
            FromLua::Subscribe {
                handler,
                scope,
                listen: Listen::Keypress,
            } => self.0.handlers.borrow_mut().push((handler, scope)),
            FromLua::Subscribe {
                handler,
                listen: Listen::TurnEnd,
                ..
            } => self.0.turn_handlers.borrow_mut().push(handler),
            FromLua::Unsubscribe { handler } => {
                self.0
                    .handlers
                    .borrow_mut()
                    .retain(|&(id, _)| id != handler);
                self.0
                    .turn_handlers
                    .borrow_mut()
                    .retain(|&id| id != handler);
            }
            FromLua::Set {
                option,
                value,
                by,
                window,
            } => {
                let options = self.options();
                let mut store = options.borrow_mut();
                // A handler's, after loading, sets it as `:set` would in
                // the window whose event it handles.
                if let Some(window) = window.filter(|_| !by.loading) {
                    store.drop_local(window, option);
                }
                let _ = store.set(option, value, by);
                let value = store.get(option);
                drop(store);
                // Lua hears every change, even its own, so its copy ends
                // up as the windows' whatever order changes cross in.
                self.send(ToLua::Option { option, value });
            }
            FromLua::Ready => self.0.running.set(None),
            other => log::warn!("Lua said {other:?} to no window"),
        }
    }

    fn keys_changed(&self) {
        self.0.keys_changed.set(self.0.keys_changed.get() + 1);
    }

    /// The bindings every window goes by, and what bound each.
    pub(crate) fn keys(&self) -> Ref<'_, Keys> {
        self.0.keys.borrow()
    }

    /// Binds `mapping`'s keys to its keys to type, as the settings menu
    /// and `:mappersist` do, noting `by` as what bound them; or if not
    /// `bound`, puts them back as the window starts them, as nothing had
    /// bound them.
    pub(crate) fn map_keys(&self, mapping: &Mapping, bound: bool, by: &SetBy) {
        let Mapping {
            modes,
            keys,
            to,
            desc,
        } = mapping;
        let mut map = self.0.keys.borrow_mut();
        if bound {
            let desc = desc.as_deref().and_then(said_in_panel);
            map.bind(modes, keys, Binding::Keys(to.clone()), desc.as_deref());
        } else {
            let defaults = crate::desk::Bindings::default();
            for &mode in modes {
                match defaults.get(mode, keys) {
                    Some((binding, label)) => map.bind(&[mode], keys, binding.clone(), label),
                    None => {
                        map.unbind(&[mode], keys);
                    }
                }
            }
        }
        let by = if bound { by.clone() } else { SetBy::DEFAULT };
        map.set_by(modes, keys, &by);
        drop(map);
        self.keys_changed();
    }

    /// How many times the bindings have changed, for a window to know when
    /// to follow.
    pub(crate) fn keys_changes(&self) -> u64 {
        self.0.keys_changed.get()
    }

    /// The keypress handlers for a key pressed in `window`, in `tab` and
    /// `pane` if it shows any, in the order they were added.
    pub(crate) fn keypress_handlers(
        &self,
        window: u32,
        shown: Option<crate::desk::Shown>,
    ) -> Vec<u64> {
        (self.0.handlers.borrow().iter())
            .filter(|(_, scope)| match *scope {
                Scope::Client => true,
                Scope::Window(of) => of == window,
                Scope::Tab(of, tab) => of == window && shown.is_some_and(|shown| shown.tab == tab),
                Scope::Pane(of, pane) => {
                    of == window && shown.is_some_and(|shown| shown.pane == pane)
                }
            })
            .map(|&(id, _)| id)
            .collect()
    }

    /// Forgets the keypress handlers of `window`'s tabs and panes that
    /// `alive` says have gone.
    pub(crate) fn forget_gone_handlers(&self, window: u32, alive: impl Fn(Scope) -> bool) {
        (self.0.handlers.borrow_mut())
            .retain(|&(_, scope)| scope.window() != Some(window) || alive(scope));
    }

    /// Whether Lua listens for chats' turns ending.
    pub(crate) fn hears_turns(&self) -> bool {
        !self.0.turn_handlers.borrow().is_empty()
    }

    /// Tells Lua of a daemon event, `heard`, unless another window on the
    /// same daemon has told it already. Returns whether it's told.
    pub(crate) fn tell_once(&self, heard: Heard, message: ToLua) -> bool {
        let mut told = self.0.heard.borrow_mut();
        if told.contains(&heard) {
            return false;
        }
        if told.len() == HEARD {
            told.pop_front();
        }
        told.push_back(heard);
        drop(told);
        self.send(message);
        true
    }

    /// When the config started loading, if it's still loading.
    pub(crate) fn running(&self) -> Option<Instant> {
        self.0.running.get()
    }

    /// How many times the config has been run again from the start.
    pub(crate) fn reloads(&self) -> u64 {
        self.0.reloads.get()
    }

    /// Runs the config again from the start, for every window, in a Lua of
    /// its own: what the old one bound, listened for, or set goes with it,
    /// and the options are as the process started them. Returns whether
    /// there's a config to run.
    pub(crate) fn reload(&self) -> bool {
        self.0.unattended.set(false);
        self.run_again()
    }

    /// Runs the config again, as [`reload`](Self::reload) does, by itself,
    /// as its files changed: what it runs then, but for keys of the
    /// user's, waits for `:reload`, as a file anyone wrote never starts a
    /// program by itself.
    pub(crate) fn reload_unattended(&self) -> bool {
        self.0.unattended.set(true);
        self.run_again()
    }

    fn run_again(&self) -> bool {
        // Its thread ends once it lets go of its end.
        *self.0.lua.borrow_mut() = None;
        for queue in self.0.windows.borrow_mut().values_mut() {
            queue.clear();
        }
        *self.0.keys.borrow_mut() = Keys::default();
        self.keys_changed();
        self.0.handlers.borrow_mut().clear();
        self.0.turn_handlers.borrow_mut().clear();
        self.0.hosts.borrow_mut().clear();
        self.0.forwards.borrow_mut().clear();
        self.options().borrow_mut().reset();
        self.0.reloads.set(self.0.reloads.get() + 1);
        self.start_lua();
        self.has_lua()
    }

    /// Waits for the config to have loaded, for tests, then returns what
    /// Lua said for `window` meanwhile.
    #[cfg(test)]
    pub(crate) fn ready(&self, window: u32) -> Vec<Said> {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while self.0.running.get().is_some() {
            let next = match &*self.0.lua.borrow() {
                Some(lua) => lua.next_within(deadline.saturating_duration_since(Instant::now())),
                None => break,
            };
            match next {
                Ok((to, from, message)) => self.route(to, from, message),
                Err(err) => panic!("init.lua never finished: {err}"),
            }
        }
        self.queued(window)
    }
}

/// `text`, from the config, as the panel of keys can show it: on one
/// line, with nothing that changes which way text runs; or nothing, if
/// nothing's left.
fn said_in_panel(text: &str) -> Option<String> {
    let said = clackatoo_term::visible(
        (text.chars()).map(|ch| if ch.is_control() { ' ' } else { ch }),
        usize::MAX,
    );
    let said = said.trim();
    (!said.is_empty()).then(|| said.to_owned())
}
