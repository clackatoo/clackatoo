//! The desk's side of the client's Lua config: telling it about the world,
//! asking it about keys, and doing what it says.
//!
//! One Lua runs for every window of the process (see [`crate::client`]);
//! the desk is one window in it, by its number. What Lua binds, listens
//! for and sets is the client's, kept once for every window: the desk
//! follows it, as its count of changes moves on ([`Desk::follow_client`]).
//! What Lua does in a window, as splitting a pane or running a command,
//! comes to that window alone.

use std::time::{Duration, Instant};

use clackatoo_protocol::{ChatState, ChatSummary, PaneId, Request};

use super::{Asked, Desk};
use crate::client::Client;
use crate::input::{KeptPress, KeyPress};
use crate::keymap::{self, Chord};
#[cfg(test)]
use crate::lua::Source;
use crate::lua::{FromLua, INIT_TIME, KeyInfo, PaneInfo, Scope, ToLua, TurnEnd, World};

/// The desk's part in the config's Lua.
#[derive(Debug, Default)]
pub(super) struct Script {
    /// The client the window is in, and its number there: until it joins
    /// one, as in tests, one made for it alone, as it's first needed.
    joined: Option<(Client, u32)>,
    /// The world as Lua was last told it.
    told: Option<World>,
    /// The keys waiting for the rest of a binding, as pressed.
    waiting: Vec<(KeptPress, Chord)>,
    /// Keys a binding to keys is typing are being handled.
    pub(super) typing: bool,
    /// The client's counts of changes to the bindings, and of reloads, as
    /// the window last followed them.
    keys_seen: u64,
    reloads_seen: u64,
}

/// How long the window takes `init.lua` to be running, at most: as long as
/// Lua lets it run, and a little. Past that, Lua has gone wrong, as by its
/// thread dying, and the window stops waiting for it.
const RUNNING_TIME: Duration = INIT_TIME.saturating_add(Duration::from_secs(1));

impl Desk {
    /// Makes the window one of `client`'s: it goes by the client's
    /// bindings, handlers and options from now on.
    pub(crate) fn join(&mut self, client: Client) {
        let window = client.join();
        self.options = crate::options::View::new(client.options(), window);
        self.script.joined = Some((client, window));
        self.follow_client();
    }

    /// The client the window is in, and its number there.
    pub(super) fn client(&mut self) -> (Client, u32) {
        let shared = self.options.shared();
        let (client, window) = self.script.joined.get_or_insert_with(|| {
            let client = Client::sharing(shared);
            let window = client.join();
            (client, window)
        });
        (client.clone(), *window)
    }

    /// Whether the window is in a client yet.
    pub(super) fn in_client(&self) -> bool {
        self.script.joined.is_some()
    }

    /// The client the window is in, if it's in one yet.
    pub(super) fn client_ref(&self) -> Option<&Client> {
        self.joined().map(|(client, _)| client)
    }

    /// The client the window is in, if it's in one yet.
    fn joined(&self) -> Option<(&Client, u32)> {
        let (client, window) = self.script.joined.as_ref()?;
        Some((client, *window))
    }

    /// The window has closed: the client forgets it.
    pub(crate) fn leave_client(&mut self) {
        if let Some((client, window)) = self.script.joined.take() {
            client.leave(window);
        }
    }

    /// Whether `init.lua` is running, on the config's first run or a
    /// reload, at `now`.
    pub(super) fn config_running(&self, now: Instant) -> bool {
        (self.joined().and_then(|(client, _)| client.running()))
            .is_some_and(|since| now < since + RUNNING_TIME)
    }

    /// When the window stops waiting for `init.lua` to finish, if it's
    /// running still, so it's woken then to do what waited.
    pub(crate) fn config_ends(&self) -> Option<Instant> {
        let now = Instant::now();
        (self.joined().and_then(|(client, _)| client.running()))
            .map(|since| since + RUNNING_TIME)
            .filter(|&ends| now < ends)
    }

    /// Runs the config again from the start, for every window, in a Lua of
    /// its own: what the old one bound, or listened for, goes with it.
    pub(super) fn reload_lua(&mut self) {
        let (client, _) = self.client();
        let reloaded = client.reload();
        self.follow_client();
        self.say(if reloaded {
            "reloaded the config".into()
        } else {
            "there's no config to reload: it's client/init.lua in clackatoo's config directory"
                .into()
        });
    }

    /// Follows what's changed in the client since the window last looked:
    /// a reload, the bindings, and the options. Returns whether anything
    /// had, so the window must be redrawn.
    pub(crate) fn follow_client(&mut self) -> bool {
        let Some(client) = self.joined().map(|(client, _)| client.clone()) else {
            return false;
        };
        let (reloads, keys) = (client.reloads(), client.keys_changes());
        let mut changed = false;
        if reloads != self.script.reloads_seen {
            self.script.reloads_seen = reloads;
            // The new Lua numbers its requests afresh, so answers to the
            // old one's go nowhere.
            for asker in &mut self.asked {
                if let super::Asker::Window(Asked::Lua(_)) = asker {
                    // And no macro waits for them: they're as good as
                    // answered.
                    self.macros.request_answered(asker);
                    *asker = super::Asker::Window(Asked::Quiet);
                }
            }
            // It hears the world afresh.
            self.script.told = None;
            changed = true;
        }
        if keys != self.script.keys_seen {
            self.script.keys_seen = keys;
            self.keymap = client.keys().map.clone();
            self.forget_waiting();
            changed = true;
        }
        if self.options.follow() {
            self.options_changed();
            changed = true;
        }
        changed
    }

    /// Tells Lua what the world looks like through the window, if that's
    /// changed since it was last told.
    pub(crate) fn sync_lua(&mut self) {
        let Some((client, window)) = self.joined() else {
            return;
        };
        client.focus(window, !self.unfocused);
        client.guard(window, self.controlled);
        if !client.has_lua() {
            return;
        }
        let terminals =
            (self.panes.iter()).map(|(&terminal, pane)| (terminal, false, pane.title()));
        let chats =
            (self.chats.iter()).map(|(&terminal, chat)| (terminal, true, Some(chat.title())));
        let panes = (terminals.chain(chats))
            .filter_map(|(terminal, chat, title)| {
                let id = self.workspaces.pane_showing(terminal)?;
                let info = PaneInfo {
                    chat,
                    title: title.map(str::to_owned),
                };
                Some((id, info))
            })
            .collect();
        let own = {
            let store = self.options.store();
            (crate::options::ALL.iter())
                .filter_map(|info| Some((info, store.local(window, info)?.0.clone())))
                .collect()
        };
        let world = World {
            workspaces: self.workspaces.clone(),
            shown: self.shown,
            panes,
            focused: !self.unfocused,
            own,
        };
        if self.script.told.as_ref() == Some(&world) {
            return;
        }
        client.send(ToLua::World {
            window,
            world: world.clone(),
        });
        self.script.told = Some(world);
    }

    /// Tells Lua something, if there's a Lua.
    pub(super) fn tell_lua(&self, message: ToLua) {
        if let Some((client, _)) = self.joined() {
            client.send(message);
        }
    }

    /// Does what Lua has said for the window since last asked, and follows
    /// what it's changed for every window. Returns whether the window must
    /// be redrawn.
    pub(crate) fn lua_said(&mut self) -> bool {
        let said = match self.joined() {
            Some((client, window)) => {
                // Whether it's to refuse what's asked of every window, as
                // it's told.
                client.guard(window, self.controlled);
                client.take(window)
            }
            None => Vec::new(),
        };
        let followed = self.follow_client();
        let redraw = !said.is_empty() || followed;
        let guarded = self.controlled;
        for (from, message) in said {
            self.lua_says(message, guarded || from.guarded);
        }
        // Settings saved come back the same way.
        self.settings_saved() || redraw
    }

    /// Asks Lua whether the key `message` describes stops, doing whatever
    /// it does first. `None` if there's no Lua, or it didn't say in time.
    fn ask_lua(&mut self, message: impl FnOnce(u64) -> ToLua) -> Option<bool> {
        self.sync_lua();
        let (client, window) = self.client();
        let (stop, said) = client.decide(window, message);
        self.follow_client();
        // For a key an agent pressed, it's the agent's; and what Lua had
        // queued by itself comes too, which a chat with Control may have
        // had it say.
        let guarded = self.automating || self.controlled;
        for (from, message) in said {
            self.lua_says(message, guarded || from.guarded);
        }
        stop
    }

    fn key_info(&mut self, chord: Chord, keys: Option<String>, press: &KeyPress<'_>) -> KeyInfo {
        let (_, window) = self.client();
        KeyInfo {
            key: chord.to_string(),
            keys,
            text: press.text.map(str::to_owned),
            mode: self.key_mode(),
            window,
            // What its handlers do is refused in every window then.
            guarded: self.automating || self.controlled,
            pane: self.shown.map(|shown| shown.pane),
            count: None,
        }
    }

    /// Runs Lua's keypress handlers for the focused pane, if it has any.
    /// Returns whether one stopped the key.
    pub(super) fn keypress_handled(&mut self, chord: Chord, press: &KeyPress<'_>) -> bool {
        let handlers = match self.joined() {
            Some((client, window)) if client.has_lua() => {
                client.keypress_handlers(window, self.shown)
            }
            _ => return false,
        };
        if handlers.is_empty() {
            return false;
        }
        let key = self.key_info(chord, None, press);
        self.ask_lua(|seq| ToLua::Key { seq, key, handlers })
            .unwrap_or(false)
    }

    /// Keeps a key that waits for the rest of a binding, to let it go on
    /// if the binding does.
    pub(super) fn keep_waiting(&mut self, chord: Chord, press: &KeyPress<'_>) {
        self.script.waiting.push((KeptPress::of(press), chord));
    }

    pub(super) fn forget_waiting(&mut self) {
        self.script.waiting.clear();
    }

    /// A binding to Lua, after `count` if one was typed: runs it, telling
    /// it the count, and if it lets the keys go on, gives the focused pane
    /// each of them in order, after the count, so `5x` bound to a function
    /// that lets it go on still deletes five. One that said it `propagate`s
    /// lets them go on at once, without waiting for Lua, which runs it
    /// after.
    pub(super) fn run_lua_binding(
        &mut self,
        binding: u64,
        propagate: bool,
        count: Option<usize>,
        chord: Chord,
        press: &KeyPress<'_>,
    ) {
        let mut pressed = std::mem::take(&mut self.script.waiting);
        pressed.push((KeptPress::of(press), chord));
        let chords: Vec<Chord> = pressed.iter().map(|&(_, chord)| chord).collect();
        let key = KeyInfo {
            count,
            ..self.key_info(chord, Some(keymap::display(&chords)), press)
        };
        let stop = if propagate {
            self.sync_lua();
            self.tell_lua(ToLua::Run { key, binding });
            false
        } else {
            self.ask_lua(|seq| ToLua::Bound { seq, key, binding })
                .unwrap_or(true)
        };
        if stop {
            return;
        }
        let digits = count.map(|count| count.to_string()).unwrap_or_default();
        let kept: Vec<KeptPress> = (digits.chars())
            .map(|digit| Chord::typed(digit).kept())
            .chain(pressed.into_iter().map(|(press, _)| press))
            .collect();
        self.type_into_pane(&kept);
    }

    /// Types `presses` into the focused pane, or its editor, as if
    /// pressed, past the bindings.
    pub(super) fn type_into_pane(&mut self, presses: &[KeptPress]) {
        for press in presses {
            let press = press.press();
            if self.focused_editor().is_some() {
                self.editor_key(&press);
            } else {
                self.pane_key(&press);
            }
        }
    }

    /// Keys that began a binding, which the key after went on to none of:
    /// they go to the focused pane as if no binding had them, as Vim's do,
    /// so binding `gi` leaves `gg` as it was, and the key after is to be
    /// looked up again on its own. Keys that began with Ctrl or Alt held,
    /// as Ctrl+B's do, are the window's, and go nowhere, nor does the key
    /// after. Returns whether it's to be looked up.
    pub(super) fn type_dropped(&mut self) -> bool {
        let pressed = std::mem::take(&mut self.script.waiting);
        if pressed
            .first()
            .is_some_and(|(_, first)| first.ctrl || first.alt)
        {
            return false;
        }
        let kept: Vec<KeptPress> = pressed.into_iter().map(|(press, _)| press).collect();
        self.type_into_pane(&kept);
        true
    }

    /// Keys a binding types that make up one of the config's bindings,
    /// ending with `press`: they go to the focused pane as they are, as
    /// Vim's `noremap` would have them.
    pub(super) fn type_bound(&mut self, chord: Chord, press: &KeyPress<'_>) {
        let mut pressed = std::mem::take(&mut self.script.waiting);
        pressed.push((KeptPress::of(press), chord));
        let kept: Vec<KeptPress> = pressed.into_iter().map(|(press, _)| press).collect();
        self.type_into_pane(&kept);
    }

    /// Answers a request Lua is waiting on, telling it the world first, so
    /// a pane made is in it.
    pub(super) fn answer_lua(&mut self, request: u64, result: Result<PaneId, String>) {
        self.sync_lua();
        self.tell_lua(ToLua::Answer { request, result });
    }

    /// Tells Lua of chats whose turn has `ended`, as the daemon said, if it
    /// listens: once, though every window on the daemon heard it.
    pub(super) fn tell_turns_ended(&mut self, ended: Vec<ChatSummary>) {
        if ended.is_empty()
            || !self
                .joined()
                .is_some_and(|(client, _)| client.hears_turns())
        {
            return;
        }
        // So the pane showing it is in Lua's world.
        self.sync_lua();
        let Some((client, window)) = self.joined() else {
            return;
        };
        let machine = self.workspaces.machine.as_ref().map(|machine| machine.id);
        for chat in ended {
            let failed = match chat.state {
                ChatState::Failed(_) => Some(true),
                _ => chat.failed,
            };
            let turn = TurnEnd {
                chat: chat.chat,
                pane: self.workspaces.pane_showing(chat.chat),
                title: chat.title,
                agent: chat.agent,
                failed,
                window,
                guarded: self.controlled,
            };
            client.tell_once((machine, chat.chat, chat.active), ToLua::TurnEnded(turn));
        }
    }

    /// Forgets Lua's handlers of the window's panes and tabs that have
    /// gone.
    pub(super) fn forget_gone_handlers(&mut self) {
        let Some((client, window)) = self.joined() else {
            return;
        };
        let workspaces = &self.workspaces;
        client.forget_gone_handlers(window, |scope| match scope {
            Scope::Tab(_, tab) => workspaces.find_tab(tab).is_some(),
            Scope::Pane(_, pane) => workspaces.find_pane(pane).is_some(),
            Scope::Client | Scope::Window(_) => true,
        });
    }

    /// Starts Lua on `config`, and does what it says as it runs, for tests.
    #[cfg(test)]
    pub(super) fn lua_from(&mut self, config: &str) {
        self.lua_in(Source::Text(config.to_owned()));
    }

    /// Starts Lua on the config in `source`, and does what it says as it
    /// runs, for tests.
    #[cfg(test)]
    pub(super) fn lua_in(&mut self, source: Source) {
        let (client, _) = self.client();
        client.start(source, Box::new(|| Box::new(|| {})));
        self.lua_ready();
    }

    /// Waits for Lua's `init.lua` to have run, doing what it says, for
    /// tests.
    #[cfg(test)]
    pub(super) fn lua_ready(&mut self) {
        let (client, window) = self.client();
        for (from, message) in client.ready(window) {
            self.lua_says(message, from.guarded);
        }
        self.follow_client();
    }

    /// Does one thing Lua says for the window, unless `guarded` and it
    /// runs commands or changes panes or machines: then it says why it
    /// didn't (see [`Desk::controlled`]).
    pub(super) fn lua_says(&mut self, message: FromLua, guarded: bool) {
        if guarded
            && let FromLua::Split { .. }
            | FromLua::Close { .. }
            | FromLua::Command { .. }
            | FromLua::Forward(_)
            | FromLua::Connect { .. }
            | FromLua::Disconnect(_) = message
        {
            let why = "the config's command wasn't run: an agent controls this window";
            if let FromLua::Split { request, .. } = message {
                self.answer_lua(request, Err(why.to_owned()));
            }
            self.warn(why.to_owned());
            return;
        }
        match message {
            FromLua::Split {
                request,
                pane,
                side,
                kind,
            } => {
                if self.workspaces.find_pane(pane).is_none() {
                    self.answer_lua(request, Err(format!("pane {} has closed", pane.0)));
                    return;
                }
                let size = self.new_pane_size(Some(side));
                let terminal = super::new_pane(kind, None, size, &self.options);
                self.request(
                    Request::Split {
                        pane,
                        side,
                        terminal,
                        on: None,
                    },
                    Asked::Lua(request),
                );
            }
            FromLua::Close { pane } => {
                self.request(Request::ClosePane { pane }, Asked::Command);
            }
            FromLua::Focus { pane } => self.show_pane(pane),
            FromLua::Command { line, by } => match crate::command::parse(&line) {
                // Not from the config being run.
                Ok(crate::command::Command::Reload) => {
                    self.say("the config can't reload itself".into());
                }
                Ok(command) => {
                    // What it sets, the config's line set.
                    self.set_by = Some(by);
                    self.run(command);
                    self.set_by = None;
                }
                Err(message) => self.warn(message),
            },
            FromLua::Forward(spec) => {
                self.forwards_asked
                    .push(crate::command::Forwarding::Config(spec));
            }
            FromLua::Connect { host, share } => self.config_connects(host, share),
            FromLua::Disconnect(host) => self.config_disconnects(host),
            FromLua::Error(text) => {
                log::warn!("config: {text}");
                self.say(text);
            }
            // The client keeps these, for every window.
            FromLua::Map { .. }
            | FromLua::Remap { .. }
            | FromLua::Unmap { .. }
            | FromLua::Group { .. }
            | FromLua::Subscribe { .. }
            | FromLua::Unsubscribe { .. }
            | FromLua::Set { .. }
            | FromLua::Ready
            | FromLua::Decided { .. } => {}
        }
    }
}
