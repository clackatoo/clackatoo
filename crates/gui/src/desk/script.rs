//! The desk's side of the client's Lua config: telling it about the world,
//! asking it about keys, and doing what it says.

use std::fmt;

use clackatoo_protocol::{NewAgent, NewTerminal, PaneId, PaneTerminal, Request};

use super::{Asked, Desk};
use crate::command::Kind;
use crate::input::{KeptPress, KeyPress};
use crate::keymap::{self, Chord};
use crate::lua::{FromLua, KeyInfo, Lua, PaneInfo, Scope, ToLua, WINDOW, World};

use super::bindings::Binding;

/// Starts the config's Lua, if there's a config.
pub(crate) struct LuaStart(pub(crate) Box<dyn Fn() -> Option<Lua>>);

impl fmt::Debug for LuaStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LuaStart")
    }
}

/// The desk's part in the config's Lua.
#[derive(Debug, Default)]
pub(super) struct Script {
    lua: Option<Lua>,
    start: Option<LuaStart>,
    /// Lua's keypress handlers, in the order they were registered.
    handlers: Vec<(u64, Scope)>,
    /// The world as Lua was last told it.
    told: Option<World>,
    /// The keys waiting for the rest of a binding, as pressed.
    waiting: Vec<(KeptPress, Chord)>,
}

impl Desk {
    /// Starts the config's Lua with `start`, which a reload starts again.
    pub(crate) fn start_lua(&mut self, start: LuaStart) {
        self.script.lua = (start.0)();
        self.script.start = Some(start);
    }

    /// Runs the config again from the start, in a Lua of its own: what the
    /// old one bound, or listened for, goes with it.
    pub(super) fn reload_lua(&mut self) {
        let start = self.script.start.take();
        // The new Lua numbers its requests afresh, so answers to the old
        // one's go nowhere.
        for asker in &mut self.asked {
            if let super::Asker::Window(Asked::Lua(_)) = asker {
                *asker = super::Asker::Window(Asked::Quiet);
            }
        }
        self.script = super::script::Script::default();
        self.keymap = super::bindings::Keys::default();
        self.script.lua = start.as_ref().and_then(|start| (start.0)());
        self.message = Some(match &self.script.lua {
            Some(_) => "reloaded the config".into(),
            None => {
                "there's no config to reload: it's client/init.lua in clackatoo's config directory"
                    .into()
            }
        });
        self.script.start = start;
    }

    /// Tells Lua what the world looks like, if that's changed since it was
    /// last told.
    pub(crate) fn sync_lua(&mut self) {
        if self.script.lua.is_none() {
            return;
        }
        let panes = (self.panes.iter())
            .filter_map(|(&terminal, pane)| {
                let id = self.workspaces.pane_showing(terminal)?;
                let info = PaneInfo {
                    chat: pane.is_chat(),
                    title: pane.title().map(str::to_owned),
                };
                Some((id, info))
            })
            .collect();
        let world = World {
            workspaces: self.workspaces.clone(),
            shown: self.shown,
            panes,
        };
        if self.script.told.as_ref() == Some(&world) {
            return;
        }
        if let Some(lua) = &self.script.lua {
            lua.send(ToLua::World(world.clone()));
        }
        self.script.told = Some(world);
    }

    /// Does what Lua has said since last asked. Returns whether the window
    /// must be redrawn.
    pub(crate) fn lua_said(&mut self) -> bool {
        let said = self
            .script
            .lua
            .as_ref()
            .map(Lua::received)
            .unwrap_or_default();
        let redraw = !said.is_empty();
        for message in said {
            self.lua_says(message);
        }
        redraw
    }

    /// Asks Lua whether the key `message` describes stops, doing whatever
    /// it does first. `None` if there's no Lua, or it didn't say in time.
    fn ask_lua(&mut self, message: impl FnOnce(u64) -> ToLua) -> Option<bool> {
        self.sync_lua();
        let (stop, said) = self.script.lua.as_mut()?.decide(message);
        for message in said {
            self.lua_says(message);
        }
        stop
    }

    fn key_info(&self, chord: Chord, keys: Option<String>, press: &KeyPress<'_>) -> KeyInfo {
        KeyInfo {
            key: chord.to_string(),
            keys,
            text: press.text.map(str::to_owned),
            mode: self.key_mode(),
            pane: self.shown.map(|shown| shown.pane),
        }
    }

    /// Runs Lua's keypress handlers for the focused pane, if it has any.
    /// Returns whether one stopped the key.
    pub(super) fn keypress_handled(&mut self, chord: Chord, press: &KeyPress<'_>) -> bool {
        let shown = self.shown;
        let handlers: Vec<u64> = (self.script.handlers.iter())
            .filter(|(_, scope)| match *scope {
                Scope::Client => true,
                Scope::Window(window) => window == WINDOW,
                Scope::Tab(tab) => shown.is_some_and(|shown| shown.tab == tab),
                Scope::Pane(pane) => shown.is_some_and(|shown| shown.pane == pane),
            })
            .map(|&(id, _)| id)
            .collect();
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

    /// A binding to Lua: runs it, and if it lets the keys go on, gives the
    /// focused pane each of them in order.
    pub(super) fn run_lua_binding(&mut self, binding: u64, chord: Chord, press: &KeyPress<'_>) {
        let mut pressed = std::mem::take(&mut self.script.waiting);
        pressed.push((KeptPress::of(press), chord));
        let chords: Vec<Chord> = pressed.iter().map(|&(_, chord)| chord).collect();
        let key = self.key_info(chord, Some(keymap::display(&chords)), press);
        let stop = self
            .ask_lua(|seq| ToLua::Bound { seq, key, binding })
            .unwrap_or(true);
        if stop {
            return;
        }
        for (press, _) in &pressed {
            if let Some(pane) = self.focused_pane() {
                pane.key(&press.press());
            }
        }
    }

    /// Answers a request Lua is waiting on, telling it the world first, so
    /// a pane made is in it.
    pub(super) fn answer_lua(&mut self, request: u64, result: Result<PaneId, String>) {
        self.sync_lua();
        if let Some(lua) = &self.script.lua {
            lua.send(ToLua::Answer { request, result });
        }
    }

    /// Forgets Lua's handlers of panes and tabs that have gone.
    pub(super) fn forget_gone_handlers(&mut self) {
        let workspaces = &self.workspaces;
        self.script.handlers.retain(|(_, scope)| match *scope {
            Scope::Client | Scope::Window(_) => true,
            Scope::Tab(tab) => workspaces.find_tab(tab).is_some(),
            Scope::Pane(pane) => workspaces.find_pane(pane).is_some(),
        });
    }

    /// Starts Lua on `config`, and does what it says as it runs, for tests.
    #[cfg(test)]
    pub(super) fn lua_from(&mut self, config: &str) {
        let config = config.to_owned();
        self.start_lua(LuaStart(Box::new(move || {
            Lua::start(crate::lua::Source::Text(config.clone()), Box::new(|| {}))
        })));
        self.lua_ready();
    }

    /// Waits for Lua's `init.lua` to have run, doing what it says, for
    /// tests.
    #[cfg(test)]
    pub(super) fn lua_ready(&mut self) {
        let said = self.script.lua.as_ref().expect("Lua started").ready();
        for message in said {
            self.lua_says(message);
        }
    }

    /// Does one thing Lua says.
    fn lua_says(&mut self, message: FromLua) {
        match message {
            FromLua::Map {
                modes,
                keys,
                binding,
            } => {
                self.keymap.bind(&modes, &keys, Binding::Lua(binding), None);
                self.forget_waiting();
            }
            FromLua::Unmap { modes, keys } => {
                self.keymap.unbind(&modes, &keys);
                self.forget_waiting();
            }
            FromLua::Subscribe { handler, scope } => self.script.handlers.push((handler, scope)),
            FromLua::Unsubscribe { handler } => {
                self.script.handlers.retain(|&(id, _)| id != handler);
            }
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
                let terminal = match kind {
                    Kind::Terminal => PaneTerminal::New(NewTerminal {
                        size,
                        command: None,
                        cwd: None,
                        record_to: None,
                    }),
                    Kind::Agent => PaneTerminal::Agent(NewAgent { size, cwd: None }),
                };
                self.request(
                    Request::Split {
                        pane,
                        side,
                        terminal,
                    },
                    Asked::Lua(request),
                );
            }
            FromLua::Close { pane } => {
                self.request(Request::ClosePane { pane }, Asked::Command);
            }
            FromLua::Focus { pane } => self.show_pane(pane),
            FromLua::Error(text) => {
                log::warn!("config: {text}");
                self.message = Some(text);
            }
            FromLua::Decided { .. } | FromLua::Ready => {}
        }
    }
}
