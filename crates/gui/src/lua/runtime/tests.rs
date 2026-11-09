use std::sync::mpsc::{self, Receiver};

use super::*;

/// The one window these tests' Lua has.
const WINDOW: u32 = 1;

/// A runtime with one window, and where what it says comes.
fn runtime() -> (Runtime, Receiver<(To, Origin, FromLua)>) {
    let (to_windows, said) = mpsc::channel();
    let state = Rc::new(State::new(Options::default(), to_windows, Box::new(|| {})));
    state.worlds.borrow_mut().insert(WINDOW, World::default());
    let slice = Arc::new(AtomicBool::new(false));
    let runtime = Runtime::new(&Source::Text(String::new()), &state, slice).unwrap();
    (runtime, said)
}

/// Loads `text` as `init.lua`.
fn load(runtime: &mut Runtime, text: &str) {
    runtime.load(&[Chunk {
        path: "init.lua".into(),
        text: Ok(text.into()),
        from: SetFrom::Config,
    }]);
}

/// Runs the background work to its end, as the Lua thread does when
/// nothing else comes, doing `between` before each slice. Returns how many
/// slices it took.
fn drain(runtime: &mut Runtime, between: impl Fn(&Runtime)) -> usize {
    let mut slices = 0;
    while let Some(job) = runtime.background.pop_front() {
        between(runtime);
        runtime.run_slice(job);
        slices += 1;
    }
    slices
}

/// A chat's turn ending, for the window.
fn turn_ended() -> ToLua {
    ToLua::TurnEnded(TurnEnd {
        chat: clackatoo_protocol::TerminalId(5),
        title: "Claude".into(),
        agent: "Claude Code".into(),
        failed: Some(false),
        window: WINDOW,
        pane: None,
        guarded: false,
    })
}

/// The commands the config ran, and its mistakes, in order.
fn commands(said: &Receiver<(To, Origin, FromLua)>) -> Vec<String> {
    (said.try_iter())
        .filter_map(|(_, _, said)| match said {
            FromLua::Command { line, .. } => Some(line),
            FromLua::Error(err) => Some(format!("error: {err}")),
            _ => None,
        })
        .collect()
}

/// `init.lua` that waits on a request as it loads, as `pane:split`
/// would, sets what it sets after as it loads, not later.
#[test]
fn a_config_that_waits_as_it_loads_is_loading_until_it_ends() {
    let (mut runtime, said) = runtime();
    // What `pane:split` yields, to wait on its answer.
    let request: Table = runtime.prelude.get("request").unwrap();
    runtime.lua.globals().set("REQUEST", request).unwrap();
    load(
        &mut runtime,
        "client.opt.nu = false\n\
         coroutine.yield(REQUEST, 7)\n\
         client.opt.wrap = false\n\
         client.keys.map('n', 'k', function() client.opt.ts = 8 end)\n",
    );
    let loading = |said: &FromLua| match said {
        FromLua::Set { by, .. } | FromLua::Map { by, .. } => {
            Some((by.loading, by.at.as_ref().map(|at| at.line)))
        }
        _ => None,
    };
    let first: Vec<_> = said.try_iter().map(|(_, _, said)| said).collect();
    assert_eq!(
        first.iter().map(loading).collect::<Vec<_>>(),
        [Some((true, Some(1)))]
    );
    // Meanwhile, a key's handler isn't the config loading.
    runtime.handle(ToLua::Answer {
        request: 7,
        result: Ok(PaneId(2)),
    });
    drain(&mut runtime, |_| {});
    let after: Vec<_> = said.try_iter().map(|(_, _, said)| said).collect();
    assert_eq!(
        after.iter().map(loading).collect::<Vec<_>>(),
        [Some((true, Some(3))), Some((true, Some(4)))]
    );
    assert!(runtime.waiting.is_empty(), "it's loaded");
    runtime.handle(ToLua::Bound {
        seq: 1,
        key: KeyInfo {
            key: "k".into(),
            keys: Some("k".into()),
            text: None,
            mode: KeyMode::Normal,
            window: WINDOW,
            guarded: false,
            pane: None,
            count: None,
        },
        binding: 1,
    });
    let later: Vec<_> = (said.try_iter())
        .filter_map(|(_, _, said)| loading(&said))
        .collect();
    assert_eq!(later, [(false, Some(4))]);
}

/// The interrupt yields only the handler it runs a slice at a time: a
/// coroutine of the handler's own, yielded by it, would hand the handler
/// nothing in place of what it yields, and the handler would go wrong.
#[test]
fn a_coroutine_of_the_handlers_own_is_never_ended_for_a_slice() {
    let (mut runtime, said) = runtime();
    load(
        &mut runtime,
        r#"
        client.on("turnend", function()
            local co = coroutine.wrap(function()
                for i = 1, 3 do
                    -- Longer than a slice, inside its own coroutine.
                    local t = os.clock()
                    while os.clock() - t < 0.01 do end
                    coroutine.yield(i)
                end
                return "end"
            end)
            local got = {}
            for i = 1, 4 do
                got[i] = tostring(co())
                local t = os.clock()
                while os.clock() - t < 0.005 do end
            end
            client.cmd(table.concat(got, ","))
        end)
        "#,
    );
    runtime.handle(turn_ended());
    let slices = drain(&mut runtime, |_| {});
    assert_eq!(commands(&said), ["1,2,3,end"]);
    assert!(slices > 1, "the handler itself was run a slice at a time");
}

/// The flag that ends a slice can't keep a handler from going on: set
/// before a slice starts, as a key coming between two would, it's cleared
/// as the slice starts, so each slice runs its length.
#[test]
fn the_flag_that_ends_a_slice_cant_stall_a_handler() {
    let (mut runtime, said) = runtime();
    load(
        &mut runtime,
        r#"
        client.on("turnend", function()
            local t = os.clock()
            while os.clock() - t < 0.05 do end
            client.cmd("done")
        end)
        "#,
    );
    runtime.handle(turn_ended());
    let slices = drain(&mut runtime, |runtime| {
        runtime.slice.store(true, Ordering::Relaxed);
    });
    assert_eq!(commands(&said), ["done"]);
    // 50 ms in slices of 2: a slice that ended at once would make
    // hundreds.
    assert!((2..=40).contains(&slices), "{slices} slices");
}

/// The watchdog's limit is on a handler's running, slice by slice, so one
/// run a slice at a time forever is still stopped; so is one that calls
/// `coroutine.yield()` forever. Each slice adds to what it's run, so
/// either comes to its limit, however quickly slices run.
#[test]
fn a_handler_run_a_slice_at_a_time_forever_is_still_stopped() {
    for body in ["while true do end", "while true do coroutine.yield() end"] {
        let (mut runtime, said) = runtime();
        runtime.budget = Duration::from_millis(20);
        load(
            &mut runtime,
            &format!("client.on('turnend', function() {body} end)"),
        );
        runtime.handle(turn_ended());
        drain(&mut runtime, |_| {});
        let said = commands(&said);
        assert!(
            matches!(&said[..], [err] if err.starts_with("error: ") && err.contains("ran too long")),
            "{body}: {said:?}"
        );
    }
}

/// A handler that's had its time isn't resumed at all.
#[test]
fn a_handler_that_has_had_its_time_isnt_resumed() {
    let (mut runtime, said) = runtime();
    load(
        &mut runtime,
        "client.on('turnend', function() client.cmd('ran') end)",
    );
    runtime.handle(turn_ended());
    let mut job = runtime.background.pop_front().unwrap();
    job.ran = runtime.budget;
    runtime.run_slice(job);
    assert_eq!(commands(&said), ["error: ran too long, so it was stopped"]);
}

/// The watchdog asking to stop a run as it ends is taken back, so the next
/// run isn't stopped for it.
#[test]
fn a_stop_asked_for_as_a_run_ends_stops_nothing_after() {
    let (mut runtime, said) = runtime();
    load(
        &mut runtime,
        "client.on('turnend', function() client.cmd('ran') end)",
    );
    runtime.watchdog.watch(Duration::from_secs(1), false);
    // As if its deadline came just as it ended.
    runtime.watchdog.stop.store(true, Ordering::Relaxed);
    runtime.watchdog.done();
    assert!(!runtime.watchdog.stop.load(Ordering::Relaxed));
    runtime.handle(turn_ended());
    drain(&mut runtime, |_| {});
    assert_eq!(commands(&said), ["ran"]);
}

/// Requests are numbered across every Lua of the process, so one run again
/// by `:reload` never takes an answer to the old one's for its own.
#[test]
fn requests_are_numbered_afresh_across_lua_run_again() {
    let split = || {
        let (mut runtime, said) = runtime();
        load(
            &mut runtime,
            "client.keys.map('n', 's', function(ev) ev.pane:split('right', {kind = 'terminal'}) end)",
        );
        runtime.handle(ToLua::Bound {
            seq: 1,
            key: KeyInfo {
                key: "s".into(),
                keys: Some("s".into()),
                text: None,
                mode: KeyMode::Normal,
                window: WINDOW,
                guarded: false,
                pane: Some(PaneId(1)),
                count: None,
            },
            binding: 1,
        });
        (said.try_iter())
            .find_map(|(_, _, said)| match said {
                FromLua::Split { request, .. } => Some(request),
                _ => None,
            })
            .expect("it split")
    };
    let (first, again) = (split(), split());
    assert!(again > first, "{first} then {again}");
}
