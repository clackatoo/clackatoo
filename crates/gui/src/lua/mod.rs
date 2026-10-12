//! Config as code: the client's `init.lua`, in Luau, run on a thread of its
//! own so that nothing it does holds up drawing.
//!
//! The window and the Lua thread only send each other messages, so neither
//! ever waits on the other for anything but a key. The window tells Lua
//! what the world looks like whenever it changes ([`World`]), so Lua reads
//! fields such as a pane's kind from its own copy. Lua tells the window
//! what it binds, what it listens for, and what it does.
//!
//! A key that Lua listens for, or is bound to Lua, waits for Lua to say
//! whether it goes on, which takes a few microseconds, and at most
//! [`DECISION_TIME`]. Everything Lua did before saying so arrives first,
//! so it's done before the key goes on.

mod runtime;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use clackatoo_protocol::{PaneId, Side, TabId, Workspaces};

use crate::command::Kind;
use crate::desk::Shown;
use crate::keymap::{Chord, KeyMode};
use crate::options::{Info, Value};

/// How long a key waits for Lua to say whether it goes on, before it goes
/// on as if Lua had said nothing.
const DECISION_TIME: Duration = Duration::from_millis(300);

/// The only window, until one process opens every window.
pub(crate) const WINDOW: u32 = 1;

/// Wakes the window when Lua has something for it.
pub(crate) type Wake = Box<dyn Fn() + Send>;

/// Where the config is.
#[derive(Clone, Debug)]
pub(crate) enum Source {
    /// The config directory, holding `client/init.lua`, and the modules
    /// it requires in `client/lua/` and `lua/`.
    Dir(PathBuf),
    /// Config given whole, for tests.
    #[cfg(test)]
    Text(String),
}

impl Source {
    /// The config's `init.lua`, if it has one.
    fn init(&self) -> Option<(String, String)> {
        match self {
            Self::Dir(dir) => {
                let path = dir.join("client/init.lua");
                let text = std::fs::read_to_string(&path).ok()?;
                Some((path.display().to_string(), text))
            }
            #[cfg(test)]
            Self::Text(text) => Some(("init.lua".into(), text.clone())),
        }
    }
}

/// What's listened to: everywhere, or one window, tab or pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    Client,
    Window(u32),
    Tab(TabId),
    Pane(PaneId),
}

/// A pane, as Lua sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneInfo {
    pub(crate) chat: bool,
    pub(crate) title: Option<String>,
}

/// What the window tells Lua the world looks like.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct World {
    pub(crate) workspaces: Workspaces,
    /// What the window shows.
    pub(crate) shown: Option<Shown>,
    /// The panes the window knows more about, having shown them.
    pub(crate) panes: HashMap<PaneId, PaneInfo>,
}

/// A key, as Lua sees it.
#[derive(Clone, Debug)]
pub(crate) struct KeyInfo {
    /// The key, as it's written in a binding.
    pub(crate) key: String,
    /// For a binding, all of its keys.
    pub(crate) keys: Option<String>,
    /// The text it types, if any.
    pub(crate) text: Option<String>,
    pub(crate) mode: KeyMode,
    /// The pane it was pressed in.
    pub(crate) pane: Option<PaneId>,
}

/// What the window tells Lua.
#[derive(Debug)]
pub(crate) enum ToLua {
    /// The world looks like this now.
    World(World),
    /// Run these keypress handlers, in order, and say whether the key was
    /// stopped.
    Key {
        seq: u64,
        key: KeyInfo,
        handlers: Vec<u64>,
    },
    /// Run this binding, and say whether the key was let go on.
    Bound {
        seq: u64,
        key: KeyInfo,
        binding: u64,
    },
    /// What came of a request a handler waits on: the pane made, or why
    /// not.
    Answer {
        request: u64,
        result: Result<PaneId, String>,
    },
    /// An option is set to this now, whoever set it.
    Option { option: &'static Info, value: Value },
}

/// What Lua tells the window.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FromLua {
    /// Bind these keys to a function of Lua's.
    Map {
        modes: Vec<KeyMode>,
        keys: Vec<Chord>,
        binding: u64,
    },
    Unmap {
        modes: Vec<KeyMode>,
        keys: Vec<Chord>,
    },
    /// Send keypresses in `scope` to a handler of Lua's.
    Subscribe {
        handler: u64,
        scope: Scope,
    },
    Unsubscribe {
        handler: u64,
    },
    /// Whether the key `seq` stops.
    Decided {
        seq: u64,
        stop: bool,
    },
    /// Split a pane, answering with [`ToLua::Answer`].
    Split {
        request: u64,
        pane: PaneId,
        side: Side,
        kind: Kind,
    },
    Close {
        pane: PaneId,
    },
    Focus {
        pane: PaneId,
    },
    /// Set an option, to a value it can hold.
    Set {
        option: &'static Info,
        value: Value,
    },
    /// Something went wrong, as this says.
    Error(String),
    /// `init.lua` has run.
    Ready,
}

/// The window's end of the Lua thread.
#[derive(Debug)]
pub(crate) struct Lua {
    to: Sender<ToLua>,
    from: Receiver<FromLua>,
    seq: u64,
}

impl Lua {
    /// Starts running `source`'s `init.lua` on a thread of its own, or
    /// returns `None` if there's no `init.lua`.
    pub(crate) fn start(source: Source, wake: Wake) -> Option<Self> {
        let (path, text) = source.init()?;
        let (to, to_receiver) = mpsc::channel();
        let (from_sender, from) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("lua".into())
            .spawn(move || runtime::run(&source, &path, &text, &to_receiver, from_sender, wake));
        if let Err(err) = spawned {
            log::warn!("couldn't start Lua: {err}");
            return None;
        }
        Some(Self { to, from, seq: 0 })
    }

    /// Tells Lua something. If its thread has ended, nobody hears.
    pub(crate) fn send(&self, message: ToLua) {
        let _ = self.to.send(message);
    }

    /// What Lua has said since last asked.
    pub(crate) fn received(&self) -> Vec<FromLua> {
        self.from.try_iter().collect()
    }

    /// Sends the key `message` makes from a new sequence number, and waits
    /// for Lua to say whether it stops. Returns that, if Lua said in time,
    /// and everything else Lua said meanwhile, in order.
    pub(crate) fn decide(
        &mut self,
        message: impl FnOnce(u64) -> ToLua,
    ) -> (Option<bool>, Vec<FromLua>) {
        self.seq += 1;
        let seq = self.seq;
        let mut said = Vec::new();
        if self.to.send(message(seq)).is_err() {
            return (None, said);
        }
        let deadline = Instant::now() + DECISION_TIME;
        loop {
            match self
                .from
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(FromLua::Decided { seq: decided, stop }) if decided == seq => {
                    return (Some(stop), said);
                }
                // An answer that came too late.
                Ok(FromLua::Decided { .. }) => {}
                Ok(message) => said.push(message),
                Err(RecvTimeoutError::Timeout) => {
                    log::warn!("Lua took too long to say whether a key goes on");
                    return (None, said);
                }
                Err(RecvTimeoutError::Disconnected) => return (None, said),
            }
        }
    }

    /// The next thing Lua says, for tests.
    #[cfg(test)]
    pub(crate) fn next(&self) -> FromLua {
        self.from
            .recv_timeout(Duration::from_secs(5))
            .expect("Lua said something")
    }

    /// Waits until `init.lua` has run, for tests, returning what Lua said
    /// meanwhile.
    #[cfg(test)]
    pub(crate) fn ready(&self) -> Vec<FromLua> {
        let mut said = Vec::new();
        loop {
            match self.from.recv_timeout(Duration::from_secs(5)) {
                Ok(FromLua::Ready) => return said,
                Ok(message) => said.push(message),
                Err(err) => panic!("init.lua never finished: {err}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Axis, Layout, Pane, Tab, Workspace, WorkspaceId};

    use super::*;
    use crate::keymap::parse_keys;

    fn start(config: &str) -> (Lua, Vec<FromLua>) {
        let lua = Lua::start(Source::Text(config.into()), Box::new(|| {})).expect("Lua started");
        let said = lua.ready();
        lua.send(ToLua::World(world(&[1, 2])));
        (lua, said)
    }

    /// One workspace, "work", with a tab of these panes side by side,
    /// showing the first. Pane 2 is a chat.
    fn world(panes: &[u32]) -> World {
        let parts = panes
            .iter()
            .map(|&id| {
                Layout::Pane(Pane {
                    id: PaneId(id),
                    content: clackatoo_protocol::Content::Terminal(clackatoo_protocol::TerminalId(
                        id,
                    )),
                })
            })
            .collect();
        let tab = Tab {
            id: TabId(1),
            focused: PaneId(panes[0]),
            layout: Layout::Split {
                axis: Axis::LeftToRight,
                shares: vec![1; panes.len()],
                parts,
            },
            name: None,
        };
        World {
            workspaces: Workspaces {
                active: Some(WorkspaceId(1)),
                list: vec![Workspace {
                    id: WorkspaceId(1),
                    name: "work".into(),
                    active_tab: TabId(1),
                    tabs: vec![tab],
                }],
            },
            shown: Some(Shown {
                workspace: WorkspaceId(1),
                tab: TabId(1),
                pane: PaneId(panes[0]),
            }),
            panes: HashMap::from([(
                PaneId(2),
                PaneInfo {
                    chat: true,
                    title: Some("Claude".into()),
                },
            )]),
        }
    }

    fn key(key: &str, pane: u32) -> KeyInfo {
        KeyInfo {
            key: key.into(),
            keys: Some(key.into()),
            text: None,
            mode: KeyMode::Normal,
            pane: Some(PaneId(pane)),
        }
    }

    fn bound(lua: &mut Lua, binding: u64, pane: u32) -> (Option<bool>, Vec<FromLua>) {
        lua.decide(|seq| ToLua::Bound {
            seq,
            key: key("k", pane),
            binding,
        })
    }

    fn pressed(lua: &mut Lua, name: &str) -> (Option<bool>, Vec<FromLua>) {
        lua.decide(|seq| ToLua::Key {
            seq,
            key: key(name, 1),
            handlers: vec![1, 5, 3],
        })
    }

    #[test]
    fn a_binding_stops_its_keys_unless_it_lets_them_go_on() {
        let (mut lua, said) = start(
            r#"
            client.keys.map("n", "<C-b>t", function(ev) ev.pane:focus() end)
            client.keys.map("ni", "x", function(ev)
                if ev.pane.kind ~= "chat" then ev:propagate() end
            end)
            client.keys.unmap("niv", "<C-b>x")
            "#,
        );
        assert_eq!(
            said,
            [
                FromLua::Map {
                    modes: vec![KeyMode::Normal],
                    keys: parse_keys("<C-b>t").unwrap(),
                    binding: 1,
                },
                FromLua::Map {
                    modes: vec![KeyMode::Normal, KeyMode::Insert],
                    keys: parse_keys("x").unwrap(),
                    binding: 2,
                },
                FromLua::Unmap {
                    modes: vec![KeyMode::Normal, KeyMode::Insert, KeyMode::Visual],
                    keys: parse_keys("<C-b>x").unwrap(),
                },
            ]
        );
        assert_eq!(
            bound(&mut lua, 1, 1),
            (Some(true), vec![FromLua::Focus { pane: PaneId(1) }]),
            "what it does comes before whether the key stops"
        );
        assert_eq!(bound(&mut lua, 2, 1), (Some(false), vec![]));
        assert_eq!(bound(&mut lua, 2, 2), (Some(true), vec![]), "in a chat");
    }

    #[test]
    fn keypress_handlers_run_in_order_until_one_stops() {
        let (mut lua, said) = start(
            r#"
            client.on("keypress", function(ev) ev.pane:focus() end)
            -- Nothing's shown while init.lua runs, so this waits for a key.
            client.keys.map("n", "p", function(ev)
                ev.pane.tab.focused:on("keypress", function(ev)
                    if ev.key == "q" then ev:stop() end
                end)
            end)
            client.windows[1]:on("keypress", function(ev) client.focused:close() end)
            client.on("keypress", function() end):off()
            "#,
        );
        let (_, mut more) = bound(&mut lua, 2, 1);
        let mut said = said;
        said.append(&mut more);
        assert_eq!(
            said,
            [
                FromLua::Subscribe {
                    handler: 1,
                    scope: Scope::Client
                },
                FromLua::Map {
                    modes: vec![KeyMode::Normal],
                    keys: parse_keys("p").unwrap(),
                    binding: 2,
                },
                FromLua::Subscribe {
                    handler: 3,
                    scope: Scope::Window(WINDOW)
                },
                FromLua::Subscribe {
                    handler: 4,
                    scope: Scope::Client
                },
                FromLua::Unsubscribe { handler: 4 },
                FromLua::Subscribe {
                    handler: 5,
                    scope: Scope::Pane(PaneId(1))
                },
            ]
        );
        assert_eq!(
            pressed(&mut lua, "q"),
            (Some(true), vec![FromLua::Focus { pane: PaneId(1) }])
        );
        assert_eq!(
            pressed(&mut lua, "w"),
            (
                Some(false),
                vec![
                    FromLua::Focus { pane: PaneId(1) },
                    FromLua::Close { pane: PaneId(1) }
                ]
            )
        );
    }

    #[test]
    fn a_split_waits_for_its_pane_after_the_key_is_decided() {
        let (mut lua, _) = start(
            r#"
            client.keys.map("n", "s", function(ev)
                local new = ev.pane:split("right")
                new:focus()
                ev:stop()
            end)
            client.keys.map("n", "S", function(ev) ev.pane:split("sideways") end)
            "#,
        );
        let (stop, said) = bound(&mut lua, 1, 1);
        assert_eq!(stop, Some(true));
        let [
            FromLua::Split {
                request,
                pane: PaneId(1),
                side: Side::Right,
                kind: Kind::Terminal,
            },
        ] = said[..]
        else {
            panic!("{said:?}")
        };
        lua.send(ToLua::World(world(&[1, 2, 3])));
        lua.send(ToLua::Answer {
            request,
            result: Ok(PaneId(3)),
        });
        assert_eq!(lua.next(), FromLua::Focus { pane: PaneId(3) });
        assert!(
            matches!(lua.next(), FromLua::Error(err) if err.contains("too late to stop")),
            "stopping after waiting is a mistake"
        );
        // A chat splits into a chat; nonsense is reported.
        let (_, said) = bound(&mut lua, 1, 2);
        assert!(matches!(
            said[..],
            [FromLua::Split {
                kind: Kind::Agent,
                ..
            }]
        ));
        let (stop, said) = bound(&mut lua, 2, 1);
        assert_eq!(stop, Some(true));
        assert!(matches!(&said[..], [FromLua::Error(err)] if err.contains("`sideways`")));
    }

    #[test]
    fn a_handler_that_runs_too_long_is_stopped() {
        let (mut lua, _) = start(
            r#"
            client.keys.map("n", "l", function() while true do end end)
            client.keys.map("n", "f", function(ev) ev.pane:focus() end)
            "#,
        );
        let started = Instant::now();
        let (stop, said) = bound(&mut lua, 1, 1);
        assert!(started.elapsed() < DECISION_TIME, "{:?}", started.elapsed());
        assert_eq!(stop, Some(true));
        assert!(matches!(&said[..], [FromLua::Error(err)] if err.contains("ran too long")));
        assert_eq!(
            bound(&mut lua, 2, 1).1,
            [FromLua::Focus { pane: PaneId(1) }],
            "and Lua carries on"
        );
    }

    #[test]
    fn options_are_set_read_and_checked() {
        use crate::options::find;
        let (mut lua, mut said) = start(
            r#"
            client.opt.nu = false
            client.opt.numberwidth = client.opt.numberwidth + 1
            if client.opt.number == false then client.opt.rnu = false end
            client.keys.map("n", "k", function()
                client.opt.number = client.opt.numberwidth == 9
            end)
            "#,
        );
        assert!(matches!(said.pop(), Some(FromLua::Map { .. })));
        let set = |name, value| FromLua::Set {
            option: find(name).unwrap(),
            value,
        };
        assert_eq!(
            said,
            [
                set("number", Value::Bool(false)),
                set("numberwidth", Value::Number(5)),
                set("relativenumber", Value::Bool(false)),
            ]
        );
        // What the window sets, Lua reads.
        lua.send(ToLua::Option {
            option: find("numberwidth").unwrap(),
            value: Value::Number(9),
        });
        assert_eq!(bound(&mut lua, 1, 1).1, [set("number", Value::Bool(true))]);
        for (config, wanted) in [
            (
                "client.opt.wrap = true",
                "init.lua:1: there's no option called `wrap`",
            ),
            ("client.opt.nu = 1", "init.lua:1: number is true or false"),
            (
                "client.opt.nuw = 2.5",
                "init.lua:1: numberwidth is a whole number from 1 to 20",
            ),
            (
                "\nclient.opt.nuw = 0",
                "init.lua:2: numberwidth is a whole number from 1 to 20",
            ),
            (
                "print(client.opt.list)",
                "init.lua:1: there's no option called `list`",
            ),
        ] {
            let (_, said) = start(config);
            assert!(
                matches!(&said[..], [FromLua::Error(err)] if err.starts_with(wanted)),
                "{config}: {said:?}"
            );
        }
    }

    #[test]
    fn handles_read_the_world_and_mistakes_are_reported() {
        let (mut lua, said) = start(
            r#"
            client.keys.map("n", "i", function(ev)
                local tab = ev.pane.tab
                if ev.window.workspace.name == "work" and #tab.panes == 2
                    and tab.workspace == ev.window.workspace
                    and tab.panes[2].title == "Claude" then
                    client.windows[1].pane:close()
                end
            end)
            "#,
        );
        assert_eq!(said.len(), 1);
        assert_eq!(
            bound(&mut lua, 1, 1).1,
            [FromLua::Close { pane: PaneId(1) }]
        );
        // A pane that's gone can't be read.
        assert!(matches!(
            &bound(&mut lua, 1, 9).1[..],
            [FromLua::Error(err)] if err.contains("pane 9 has closed")
        ));
        // A mistake says where it is, and stops init.lua there.
        for (config, wanted) in [
            ("client.keys.map(", "init.lua:1:"),
            (
                "\nclient.keys.map('x', 'a', function() end)\nclient.keys.map('n', 'a', print)",
                "init.lua:2: there's no mode `x`: the modes are n, i, v and c",
            ),
            (
                "client.on('click', print)",
                "init.lua:1: there's no event called `click`: the events are keypress",
            ),
        ] {
            let (_, said) = start(config);
            assert!(
                matches!(&said[..], [FromLua::Error(err)] if err.starts_with(wanted)),
                "{said:?}"
            );
        }
    }
}
