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

pub(crate) use runtime::INIT_TIME;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use clackatoo_protocol::{PaneId, Side, TabId, Workspaces};

use crate::command::Kind;
use crate::desk::Shown;
use crate::keymap::{Chord, KeyMode};
use crate::options::provenance::Source as SetFrom;
use crate::options::{Info, Options, SetBy, Value};

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
    /// The config directory, holding `client/settings.lua` and
    /// `client/init.lua`, and the modules they require in `client/lua/`
    /// and `lua/`.
    Dir(PathBuf),
    /// An `init.lua` given whole, for tests.
    #[cfg(test)]
    Text(String),
}

/// One file of the config, run in turn as it loads.
pub(crate) struct Chunk {
    pub(crate) path: String,
    /// Its text, or why it couldn't be read.
    pub(crate) text: Result<String, String>,
    /// What it sets is set by, as provenance says.
    pub(crate) from: SetFrom,
}

impl Source {
    /// The config's files, in the order they run: `settings.lua`, which
    /// clackatoo writes, then the user's own `init.lua`, which outdoes it;
    /// each if it's there, and one that can't be read, to say why.
    fn chunks(&self) -> Vec<Chunk> {
        match self {
            Self::Dir(dir) => [
                ("client/settings.lua", SetFrom::Settings),
                ("client/init.lua", SetFrom::Config),
            ]
            .into_iter()
            .filter_map(|(file, from)| {
                let path = dir.join(file);
                let text = match std::fs::read_to_string(&path) {
                    Ok(text) => Ok(text),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
                    Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                        Err("it isn't UTF-8 text".into())
                    }
                    Err(err) => Err(err.to_string()),
                };
                Some(Chunk {
                    path: path.display().to_string(),
                    text,
                    from,
                })
            })
            .collect(),
            #[cfg(test)]
            Self::Text(text) => vec![Chunk {
                path: "init.lua".into(),
                text: Ok(text.clone()),
                from: SetFrom::Config,
            }],
        }
    }

    /// The config directory, if it's one.
    #[allow(clippy::unnecessary_wraps, reason = "tests' configs have none")]
    pub(crate) fn dir(&self) -> Option<&Path> {
        match self {
            Self::Dir(dir) => Some(dir),
            #[cfg(test)]
            Self::Text(_) => None,
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
    /// For a binding, the count typed before it, as Vim's `v:count`.
    pub(crate) count: Option<usize>,
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
        /// What it does, for the panel of keys.
        desc: Option<String>,
        /// Where the config bound it.
        by: SetBy,
    },
    Unmap {
        modes: Vec<KeyMode>,
        keys: Vec<Chord>,
        by: SetBy,
    },
    /// Binds `keys` in `modes` to typing `to`.
    Remap {
        modes: Vec<KeyMode>,
        keys: Vec<Chord>,
        to: Vec<Chord>,
        desc: Option<String>,
        by: SetBy,
    },
    /// Names the bindings `keys` start in `modes`, for the panel of keys,
    /// as which-key.nvim's `group` does; or with no name, stops naming
    /// them.
    Group {
        modes: Vec<KeyMode>,
        keys: Vec<Chord>,
        name: Option<String>,
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
    /// Run a command, as if typed after `:`, as the config's line `by`
    /// asked.
    Command {
        line: String,
        by: SetBy,
    },
    /// Forward a port, as `:portforward` does, but saying nothing in a
    /// window on this machine, which has none to forward: a config runs in
    /// every window.
    Forward(clackatoo_daemon::forward::Spec),
    /// Have the daemon connect to another machine, as `:connect HOST`
    /// does, but saying nothing unless it fails: a config runs in every
    /// window. `share` is `false` for `{share = false}`, as
    /// `--no-share`, and `true` for `{share = true}`.
    Connect {
        host: String,
        share: Option<bool>,
    },
    /// Have the daemon end its connection to another machine, as
    /// `:disconnect HOST` does.
    Disconnect(String),
    /// Set an option, to a value it can hold, as the config's line `by`
    /// asked.
    Set {
        option: &'static Info,
        value: Value,
        by: SetBy,
    },
    /// Something went wrong, as this says.
    Error(String),
    /// The config has loaded: `settings.lua`, then `init.lua`.
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
    /// Starts running `source`'s config on a thread of its own, its
    /// `settings.lua` then its `init.lua`, with `options` as the window's
    /// are, or returns `None` if it has neither.
    pub(crate) fn start(source: Source, options: Options, wake: Wake) -> Option<Self> {
        let chunks = source.chunks();
        if chunks.is_empty() {
            return None;
        }
        let (to, to_receiver) = mpsc::channel();
        let (from_sender, from) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("lua".into())
            .spawn(move || {
                let channels = (&to_receiver, from_sender);
                runtime::run(&source, &chunks, options, channels, wake);
            });
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

    /// What Lua said, with what set each thing left out, as these tests
    /// don't look at it: `provenance` tests do.
    fn plain(said: Vec<FromLua>) -> Vec<FromLua> {
        (said.into_iter())
            .map(|mut message| {
                if let FromLua::Map { by, .. }
                | FromLua::Remap { by, .. }
                | FromLua::Unmap { by, .. }
                | FromLua::Set { by, .. }
                | FromLua::Command { by, .. } = &mut message
                {
                    *by = SetBy::DEFAULT;
                }
                message
            })
            .collect()
    }

    fn start(config: &str) -> (Lua, Vec<FromLua>) {
        let lua = Lua::start(
            Source::Text(config.into()),
            Options::default(),
            Box::new(|| {}),
        )
        .expect("Lua started");
        let said = plain(lua.ready());
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
                    home: None,
                    key: None,
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
                home: None,
                restored: Vec::new(),
                active: Some(WorkspaceId(1)),
                list: vec![Workspace {
                    id: WorkspaceId(1),
                    name: "work".into(),
                    active_tab: TabId(1),
                    tabs: vec![tab],
                    home: None,
                    lent: None,
                    background: Vec::new(),
                }],
                passed: Vec::new(),
                machine: None,
                others: Vec::new(),
                kept: Vec::new(),
                homes: Vec::new(),
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
            count: None,
        }
    }

    fn bound(lua: &mut Lua, binding: u64, pane: u32) -> (Option<bool>, Vec<FromLua>) {
        let (stop, said) = lua.decide(|seq| ToLua::Bound {
            seq,
            key: key("k", pane),
            binding,
        });
        (stop, plain(said))
    }

    fn pressed(lua: &mut Lua, name: &str) -> (Option<bool>, Vec<FromLua>) {
        let (stop, said) = lua.decide(|seq| ToLua::Key {
            seq,
            key: key(name, 1),
            handlers: vec![1, 5, 3],
        });
        (stop, plain(said))
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
                    desc: None,
                    by: SetBy::DEFAULT,
                },
                FromLua::Map {
                    modes: vec![KeyMode::Normal, KeyMode::Insert],
                    keys: parse_keys("x").unwrap(),
                    binding: 2,
                    desc: None,
                    by: SetBy::DEFAULT,
                },
                FromLua::Unmap {
                    modes: vec![KeyMode::Normal, KeyMode::Insert, KeyMode::Visual],
                    keys: parse_keys("<C-b>x").unwrap(),
                    by: SetBy::DEFAULT,
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
    fn bindings_say_what_they_do_and_hear_the_count() {
        let (mut lua, said) = start(
            r#"
            client.keys.map("n", "gx", function(ev)
                client.cmd(`count {ev.count} {ev.count1}`)
            end, {desc = "open the link"})
            client.keys.map("nv", "gi", "gri", {desc = "go in"})
            client.keys.map("n", "gy", print, {})
            client.keys.group("n", "<Space>g", "git")
            client.keys.group("n", "<Space>g", nil)
            "#,
        );
        assert_eq!(
            said,
            [
                FromLua::Map {
                    modes: vec![KeyMode::Normal],
                    keys: parse_keys("gx").unwrap(),
                    binding: 1,
                    desc: Some("open the link".into()),
                    by: SetBy::DEFAULT,
                },
                FromLua::Remap {
                    modes: vec![KeyMode::Normal, KeyMode::Visual],
                    keys: parse_keys("gi").unwrap(),
                    to: parse_keys("gri").unwrap(),
                    desc: Some("go in".into()),
                    by: SetBy::DEFAULT,
                },
                FromLua::Map {
                    modes: vec![KeyMode::Normal],
                    keys: parse_keys("gy").unwrap(),
                    binding: 2,
                    desc: None,
                    by: SetBy::DEFAULT,
                },
                FromLua::Group {
                    modes: vec![KeyMode::Normal],
                    keys: parse_keys("<Space>g").unwrap(),
                    name: Some("git".into()),
                },
                FromLua::Group {
                    modes: vec![KeyMode::Normal],
                    keys: parse_keys("<Space>g").unwrap(),
                    name: None,
                },
            ]
        );
        let counted = |lua: &mut Lua, count| {
            plain(
                lua.decide(|seq| ToLua::Bound {
                    seq,
                    key: KeyInfo {
                        count,
                        ..key("gx", 1)
                    },
                    binding: 1,
                })
                .1,
            )
        };
        assert_eq!(
            counted(&mut lua, Some(3)),
            [FromLua::Command {
                line: "count 3 3".into(),
                by: SetBy::DEFAULT,
            }]
        );
        assert_eq!(
            counted(&mut lua, None),
            [FromLua::Command {
                line: "count 0 1".into(),
                by: SetBy::DEFAULT,
            }],
            "as Vim's v:count and v:count1"
        );
    }

    #[test]
    fn a_binding_written_for_neovim_loads_and_says_what_it_leaves_out() {
        // Neovim's options that change nothing here bind quietly, so a
        // binding written for Neovim loads as it is.
        let (_, said) = start(
            "client.keys.map('n', 'a', print, \
             {silent = true, noremap = true, nowait = true, unique = true, remap = false})",
        );
        assert!(
            matches!(&said[..], [FromLua::Map { desc: None, .. }]),
            "{said:?}"
        );
        // Any other is left out, saying so, and the binding's made all the
        // same, and the lines after it run.
        for (options, wanted, kept) in [
            (
                "{desc = 3}",
                "leaves out its `desc`, which is a string",
                None,
            ),
            (
                "{remap = true, desc = 'go'}",
                "leaves out `remap`",
                Some("go"),
            ),
            (
                "{expr = true, desc = 'go'}",
                "leaves out `expr`",
                Some("go"),
            ),
            ("{buffer = 0}", "leaves out `buffer`", None),
            ("{callback = print}", "leaves out `callback`", None),
            (
                "{replace_keycodes = false}",
                "leaves out `replace_keycodes`",
                None,
            ),
            (
                "{dsec = 'x'}",
                "leaves out `dsec`, which isn't one of its options",
                None,
            ),
            (
                "{'x', desc = 'go'}",
                "leaves out options that aren't named",
                Some("go"),
            ),
        ] {
            let (_, said) = start(&format!(
                "client.keys.map('n', '<C-b>a', 'gri', {options})\nclient.keys.unmap('n', 'b')"
            ));
            assert!(
                matches!(
                    &said[..],
                    [
                        FromLua::Remap { desc, .. },
                        FromLua::Error(err),
                        FromLua::Unmap { .. },
                    ] if desc.as_deref() == kept
                        && err.starts_with("the binding of `<C-b>a` ")
                        && err.contains(wanted)
                ),
                "{options}: {said:?}"
            );
        }
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
                    desc: None,
                    by: SetBy::DEFAULT,
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

    /// Every window runs its config in a process shared with the others,
    /// so nothing in it may reach the whole process, as ending it or
    /// changing its locale, environment or files would. Luau's libraries
    /// never had any of that, unlike Lua's: this only keeps it so, should
    /// a library be added.
    #[test]
    fn config_can_do_nothing_to_the_process_the_windows_share() {
        let (_lua, said) = start(
            r"
            assert(os.exit == nil and os.execute == nil and os.setlocale == nil)
            assert(os.remove == nil and os.rename == nil and os.getenv == nil)
            assert(io == nil and package == nil and loadfile == nil and dofile == nil)
            ",
        );
        assert!(
            !said.iter().any(|said| matches!(said, FromLua::Error(_))),
            "{said:?}"
        );
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
            client.opt.sbr = "↪ "
            client.opt.showbreak = client.opt.showbreak .. ">"
            client.keys.map("n", "k", function()
                client.opt.number = client.opt.numberwidth == 9
            end)
            "#,
        );
        assert!(matches!(said.pop(), Some(FromLua::Map { .. })));
        let set = |name, value| FromLua::Set {
            option: find(name).unwrap(),
            value,
            by: SetBy::DEFAULT,
        };
        assert_eq!(
            said,
            [
                set("number", Value::Bool(false)),
                set("numberwidth", Value::Number(5)),
                set("relativenumber", Value::Bool(false)),
                set("showbreak", Value::Text("↪ ".into())),
                set("showbreak", Value::Text("↪ >".into())),
            ]
        );
        // What the window sets, Lua reads.
        lua.send(ToLua::Option {
            option: find("numberwidth").unwrap(),
            value: Value::Number(9),
        });
        assert_eq!(bound(&mut lua, 1, 1).1, [set("number", Value::Bool(true))]);
        for (config, wanted) in [
            ("client.opt.spell = true", "init.lua:1: no option ‘spell’"),
            ("client.opt.nu = 1", "init.lua:1: number is true or false"),
            (
                "client.opt.sbr = 1",
                "init.lua:1: showbreak is text, without control characters",
            ),
            (
                "client.opt.nuw = 2.5",
                "init.lua:1: numberwidth is a whole number from 1 to 20",
            ),
            (
                "\nclient.opt.nuw = 0",
                "init.lua:2: numberwidth is a whole number from 1 to 20",
            ),
            ("print(client.opt.list)", "init.lua:1: no option ‘list’"),
        ] {
            let (_, said) = start(config);
            assert!(
                matches!(&said[..], [FromLua::Error(err)] if err.starts_with(wanted)),
                "{config}: {said:?}"
            );
        }
    }

    #[test]
    fn the_font_is_read_as_the_window_started_and_its_size_can_have_a_fraction() {
        use crate::options::{Decimal, Options, find};
        let options = Options::drawing_with(&crate::FontOptions {
            family: Some("Iosevka".into()),
            size: 13.5,
        });
        let lua = Lua::start(
            Source::Text(
                r#"
                if client.opt.font == "Iosevka" and client.opt.fsz == 13.5 then
                    client.opt.gfn = "JetBrains Mono"
                end
                client.opt.fontsize = 10.25
                client.opt.fontsize = client.opt.fontsize + 0.25
                client.opt.fontsize = 14
                client.opt.fsz = "10.5"
                client.opt.numberwidth = "6"
                "#
                .into(),
            ),
            options,
            Box::new(|| {}),
        )
        .expect("Lua started");
        let set = |name, value| FromLua::Set {
            option: find(name).unwrap(),
            value,
            by: SetBy::DEFAULT,
        };
        let size = |number: f64| Value::Decimal(Decimal::exact(number).unwrap());
        assert_eq!(
            plain(lua.ready()),
            [
                set("font", Value::Text("JetBrains Mono".into())),
                set("fontsize", size(10.25)),
                set("fontsize", size(10.5)),
                set("fontsize", size(14.0)),
                set("fontsize", size(10.5)),
                set("numberwidth", Value::Number(6)),
            ]
        );
        let wants = "fontsize is a number from 4 to 200, to two decimal places at most";
        for config in [
            "client.opt.fsz = 10.125",
            "client.opt.fsz = 3",
            "client.opt.fsz = -12",
            "client.opt.fsz = '12pt'",
            "client.opt.fsz = 0/0",
            "client.opt.fsz = 1/0",
            "client.opt.fsz = math.huge",
            "client.opt.fsz = -math.huge",
        ] {
            let (_, said) = start(config);
            assert!(
                matches!(&said[..], [FromLua::Error(err)] if err.ends_with(wants)),
                "{config}: {said:?}"
            );
        }
        let wants = "numberwidth is a whole number from 1 to 20";
        for config in [
            "client.opt.nuw = 1/0",
            "client.opt.nuw = math.huge",
            "client.opt.nuw = '6.5'",
        ] {
            let (_, said) = start(config);
            assert!(
                matches!(&said[..], [FromLua::Error(err)] if err.ends_with(wants)),
                "{config}: {said:?}"
            );
        }
    }

    #[test]
    fn a_list_is_set_with_a_string_a_table_or_a_number() {
        use crate::options::find;
        let (mut lua, mut said) = start(
            r#"
            client.opt.cc = "80,120"
            client.opt.colorcolumn = { client.opt.cc[2], 100 }
            client.opt.cc = 72
            client.opt.cc = ""
            client.keys.map("n", "k", function()
                client.opt.ts = #client.opt.cc
            end)
            "#,
        );
        assert!(matches!(said.pop(), Some(FromLua::Map { .. })));
        let set = |name, value| FromLua::Set {
            option: find(name).unwrap(),
            value,
            by: SetBy::DEFAULT,
        };
        assert_eq!(
            said,
            [
                set("colorcolumn", Value::List(vec![80, 120])),
                set("colorcolumn", Value::List(vec![120, 100])),
                set("colorcolumn", Value::List(vec![72])),
                set("colorcolumn", Value::List(Vec::new())),
            ]
        );
        lua.send(ToLua::Option {
            option: find("cc").unwrap(),
            value: Value::List(vec![1, 2, 3]),
        });
        assert_eq!(bound(&mut lua, 1, 1).1, [set("tabstop", Value::Number(3))]);
        let list = "colorcolumn is whole numbers from 1 to 9999, with commas between them";
        for (config, wanted) in [
            ("client.opt.cc = '80,x'", list),
            ("client.opt.cc = {80, 2.5}", list),
            ("client.opt.cc = 0", list),
            ("client.opt.cc = true", list),
            (
                "client.opt.ts = 'four'",
                "tabstop is a whole number from 1 to 32",
            ),
        ] {
            let (_, said) = start(config);
            assert!(
                matches!(&said[..], [FromLua::Error(err)] if err.starts_with(&format!("init.lua:1: {wanted}"))),
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

    /// A config directory holding `files`, by their paths in it.
    fn config_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one config, and what each line set")]
    fn settings_lua_runs_first_and_what_set_each_option_is_known_by_its_line() {
        use crate::options::find;
        use crate::options::provenance::{Place, Source as From};
        let dir = config_dir(&[
            (
                "client/settings.lua",
                "-- Written by clackatoo.\nclient.opt.number = false\n\
                 client.opt.nosuch = 1\nclient.opt.wrap = false\n",
            ),
            (
                "client/init.lua",
                "local keys = require('keys')\n\nclient.opt.nuw = 6\nkeys.setup()\n\
                 client.keys.map('n', 'k', function()\n  client.opt.ts = 8\nend)\n\
                 client.cmd('set nolbr')\n",
            ),
            (
                "lua/keys.lua",
                "client.opt.so = 3\nreturn {\n  setup = function()\n    client.opt.rnu = false\n  end,\n}\n",
            ),
        ]);
        let file = |name: &str| -> std::sync::Arc<str> {
            dir.path().join(name).display().to_string().into()
        };
        let place = |name, line| {
            Some(Place {
                file: file(name),
                line,
            })
        };
        let by = |from, loading, at, inner| SetBy {
            source: from,
            loading,
            at,
            inner,
        };
        let mut lua = Lua::start(
            Source::Dir(dir.path().into()),
            Options::default(),
            Box::new(|| {}),
        )
        .expect("Lua started");
        let said = lua.ready();
        let set = |name, value, by| FromLua::Set {
            option: find(name).unwrap(),
            value,
            by,
        };
        let settings = |line| {
            by(
                From::Settings,
                true,
                place("client/settings.lua", line),
                None,
            )
        };
        let init = |line, inner| by(From::Config, true, place("client/init.lua", line), inner);
        let [
            number,
            FromLua::Error(nosuch),
            wrap,
            scrolloff,
            numberwidth,
            relativenumber,
            FromLua::Map { by: map, .. },
            FromLua::Command { by: command, .. },
        ] = &said[..]
        else {
            panic!("{said:?}")
        };
        assert_eq!(*number, set("number", Value::Bool(false), settings(2)));
        // A line of settings.lua this build can't set is said, and the
        // rest are set all the same.
        assert_eq!(
            *nosuch,
            format!("{}:3: no option ‘nosuch’", file("client/settings.lua"))
        );
        assert_eq!(*wrap, set("wrap", Value::Bool(false), settings(4)));
        // A module's own line, and the user's that required it.
        assert_eq!(
            *scrolloff,
            set(
                "scrolloff",
                Value::Number(3),
                init(1, place("lua/keys.lua", 1))
            )
        );
        assert_eq!(
            *numberwidth,
            set("numberwidth", Value::Number(6), init(3, None))
        );
        assert_eq!(
            *relativenumber,
            set(
                "relativenumber",
                Value::Bool(false),
                init(4, place("lua/keys.lua", 4))
            )
        );
        assert_eq!(*map, init(5, None));
        assert_eq!(*command, init(8, None));
        // After it's loaded, a handler's line, as the config's.
        let (_, said) = lua.decide(|seq| ToLua::Bound {
            seq,
            key: key("k", 1),
            binding: 1,
        });
        assert_eq!(
            said,
            [set(
                "tabstop",
                Value::Number(8),
                by(From::Config, false, place("client/init.lua", 6), None)
            )]
        );
    }

    #[test]
    fn options_read_only_as_a_window_opens_are_read_but_not_set() {
        let (_, said) =
            start("assert(client.opt.renderer == 'vulkan')\nclient.opt.renderer = 'opengl'");
        assert!(
            matches!(
                &said[..],
                [FromLua::Error(err)] if err == "init.lua:2: renderer is read as a window opens: \
                    :setpersist saves it in startup.toml, for windows opened after"
            ),
            "{said:?}"
        );
    }

    #[test]
    fn a_config_file_that_cant_be_read_is_said_and_the_other_runs() {
        let dir = config_dir(&[("client/init.lua", "client.opt.nu = false\n")]);
        let settings = dir.path().join("client/settings.lua");
        std::fs::write(&settings, b"client.opt.wrap = \"\xff\"\n").unwrap();
        let start = || {
            Lua::start(
                Source::Dir(dir.path().into()),
                Options::default(),
                Box::new(|| {}),
            )
            .expect("Lua started")
            .ready()
        };
        let said = plain(start());
        let unreadable = format!("couldn't read {}: it isn't UTF-8 text", settings.display());
        assert!(
            matches!(
                &said[..],
                [FromLua::Error(err), FromLua::Set { .. }] if *err == unreadable
            ),
            "{said:?}"
        );
        // Alone, it's still said.
        std::fs::remove_file(dir.path().join("client/init.lua")).unwrap();
        let said = start();
        assert!(
            matches!(&said[..], [FromLua::Error(err)] if *err == unreadable),
            "{said:?}"
        );
    }
}
