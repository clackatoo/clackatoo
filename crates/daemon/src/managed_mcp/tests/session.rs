//! The proxy's rules, each driven with the clock in hand: no server, no
//! daemon and no waiting.
use std::time::{Duration, Instant};

use clackatoo_protocol::{McpScope, McpState};
use serde_json::{Value, json};

use super::super::config::{Bound, Definition};
use super::super::proxy::Grant;
use super::super::proxy::session::{Effect, Ending, LONGEST_ASKING, Session, Verdict};
use super::super::transport::Lost;

/// A grant for a server called `fixture`, whose requests may take
/// `timeout` seconds and whose calls `call_timeout`.
fn grant(timeout: u32, call_timeout: u32, generation: u64) -> Grant {
    let definition = Definition {
        command: Some("server".into()),
        timeout_seconds: timeout,
        call_timeout_seconds: call_timeout,
        ..Definition::default()
    };
    Grant {
        bound: Bound {
            name: "fixture".into(),
            definition: Some(definition),
            source: "Personal".into(),
            path: "/nowhere/mcp.json".into(),
            scope: McpScope::Personal,
            editable: true,
            cwd: "/nowhere".into(),
            private: "/nowhere/private".into(),
            revision: "revision".into(),
            approved: false,
            available_by_default: true,
            problem: String::new(),
        },
        generation,
    }
}

fn initialize(id: u32) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"agent","version":"1"}}})
}

fn initialized(id: u32) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":{
        "protocolVersion":"2025-06-18","capabilities":{"tools":{}},
        "serverInfo":{"name":"real","version":"1"}}})
}

fn call(id: u32) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":"write","arguments":{},"_meta":{"progressToken":format!("p{id}")}}})
}

/// A session whose server is connected and has answered `initialize`,
/// and whose agent has said so, at `now`.
fn connected(timeout: u32, call_timeout: u32, now: Instant) -> Session {
    let mut session = Session::new(grant(timeout, call_timeout, 0));
    session.agent_said(initialize(0), now);
    session.connected(Ok(()), now);
    session.server_said(initialized(0), now);
    session.agent_said(
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        now,
    );
    let effects = session.effects();
    assert!(
        effects.contains(&Effect::Agent(initialized(0))),
        "{effects:?}"
    );
    session
}

/// Has the daemon allow the call `id`, which the session asked about.
fn allow(session: &mut Session, id: u32, now: Instant) {
    session.agent_said(call(id), now);
    let effects = session.effects();
    assert!(
        matches!(
            &effects[..],
            [Effect::Ask { key, method, .. }] if *key == id.to_string() && method == "tools/call"
        ),
        "{effects:?}"
    );
    session.answered(&id.to_string(), Verdict::Allowed, now);
    assert_eq!(session.effects(), [Effect::Server(call(id))]);
}

/// The error the agent was told for the request `id`, among `effects`.
fn error_for(effects: &[Effect], id: u32) -> String {
    effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Agent(message) if message["id"] == id => {
                Some(message["error"]["message"].as_str()?.to_owned())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no error for {id} in {effects:?}"))
}

#[test]
fn a_call_waits_for_leave_as_long_as_a_person_takes_and_is_given_up_only_after_a_day() {
    let start = Instant::now();
    let mut session = connected(30, 600, start);
    session.agent_said(call(1), start);
    assert!(matches!(&session.effects()[..], [Effect::Ask { .. }]));
    // Five minutes was once the end of it. Nothing happens now, however
    // long the user takes.
    for minutes in [5, 6, 60, 23 * 60] {
        session.tick(start + Duration::from_mins(minutes));
        assert_eq!(session.effects(), [], "after {minutes} minutes");
    }
    assert_eq!(session.next_due(), Some(start + LONGEST_ASKING));
    session.tick(start + LONGEST_ASKING);
    let effects = session.effects();
    assert_eq!(effects[0], Effect::Unask("1".into()));
    assert!(
        error_for(&effects, 1).contains("within a day"),
        "{effects:?}"
    );
    // An answer that comes after all is no longer wanted.
    session.answered("1", Verdict::Allowed, start + LONGEST_ASKING);
    assert_eq!(session.effects(), []);
}

#[test]
fn a_tool_call_outlasts_timeout_seconds_and_progress_restarts_its_clock() {
    let start = Instant::now();
    let at = |seconds| start + Duration::from_secs(seconds);
    let mut session = connected(5, 60, start);
    allow(&mut session, 1, start);
    // Past `timeout_seconds`, within `call_timeout_seconds`.
    session.tick(at(30));
    assert_eq!(session.effects(), []);
    let progress = json!({"jsonrpc":"2.0","method":"notifications/progress","params":{
        "progressToken":"p1","progress":1}});
    session.server_said(progress.clone(), at(50));
    assert_eq!(session.effects(), [Effect::Agent(progress)]);
    // A minute after it was sent, but not after it last made progress.
    session.tick(at(70));
    assert_eq!(session.effects(), []);
    assert_eq!(session.next_due(), Some(at(110)));
    session.tick(at(110));
    let effects = session.effects();
    assert!(
        matches!(
            &effects[0],
            Effect::Server(told)
                if told["method"] == "notifications/cancelled" && told["params"]["requestId"] == 1
        ),
        "{effects:?}"
    );
    let why = error_for(&effects, 1);
    assert!(
        why.contains("‘write’")
            && why.contains("60 seconds")
            && why.contains("call_timeout_seconds"),
        "{why}"
    );
    // Its answer, coming late, isn't passed on twice.
    session.server_said(json!({"jsonrpc":"2.0","id":1,"result":{}}), at(111));
    assert_eq!(session.effects(), []);

    // A list has `timeout_seconds`, and says so.
    session.agent_said(
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        at(200),
    );
    session.effects();
    session.tick(at(204));
    assert_eq!(session.effects(), []);
    session.tick(at(205));
    let why = error_for(&session.effects(), 2);
    assert!(
        why.contains("tools/list") && why.contains("(its timeout_seconds)"),
        "{why}"
    );
}

#[test]
fn initialize_is_never_cancelled_a_server_that_doesnt_answer_it_is_connected_to_again() {
    let start = Instant::now();
    let mut session = Session::new(grant(5, 60, 0));
    assert_eq!(
        session.effects(),
        [Effect::Connect {
            after: Duration::ZERO
        }]
    );
    session.agent_said(initialize(0), start);
    session.connected(Ok(()), start);
    assert_eq!(session.effects(), [Effect::Server(initialize(0))]);
    session.tick(start + Duration::from_secs(5));
    let effects = session.effects();
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Server(_))),
        "nothing is sent the server, least of all a cancellation: {effects:?}"
    );
    // The agent is answered for the server, so it keeps it.
    let greeting = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Agent(message) if message["id"] == 0 => Some(message.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(greeting["result"]["serverInfo"]["name"], "fixture");
    assert!(matches!(effects.last(), Some(Effect::Connect { after }) if !after.is_zero()));
}

#[test]
fn a_server_that_goes_fails_what_it_was_doing_and_is_initialized_again_once_back() {
    let start = Instant::now();
    let mut session = connected(30, 600, start);
    allow(&mut session, 1, start);
    session.server_said(
        json!({"jsonrpc":"2.0","id":"ask","method":"roots/list"}),
        start,
    );
    session.agent_said(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), start);
    session.effects();

    session.lost(
        Lost::Failed("its process exited (exit status: 1)".into()),
        start,
    );
    let effects = session.effects();
    let why = error_for(&effects, 1);
    assert!(
        why.contains("isn't known") && why.contains("exit status: 1"),
        "{why}"
    );
    assert!(error_for(&effects, 2).contains("try again shortly"));
    // What the server asked the agent is called off.
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Agent(told)
        if told["method"] == "notifications/cancelled" && told["params"]["requestId"] == "ask"))
    );
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Report {
            state: McpState::Error,
            ..
        }
    )));
    assert_eq!(
        effects.last(),
        Some(&Effect::Connect {
            after: Duration::from_millis(500)
        })
    );
    assert!(!session.initialized());

    // Meanwhile the agent is told why, and lists are empty, not errors.
    session.agent_said(call(3), start);
    session.agent_said(json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}), start);
    let effects = session.effects();
    assert!(
        error_for(&effects, 3).contains("isn't connected"),
        "{effects:?}"
    );
    assert_eq!(
        effects[1],
        Effect::Agent(json!({"jsonrpc":"2.0","id":4,"result":{"tools":[]}}))
    );

    // Back: it's sent what the agent sent the last one, and the agent
    // hears only that its tools may have changed.
    session.connected(Ok(()), start);
    assert_eq!(session.effects(), [Effect::Server(initialize(0))]);
    session.server_said(initialized(0), start);
    let effects = session.effects();
    assert_eq!(
        effects,
        [
            Effect::Report {
                state: McpState::Connected,
                detail: String::new(),
                tools: vec![]
            },
            Effect::Server(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
            Effect::Agent(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})),
        ]
    );
    allow(&mut session, 5, start);
}

#[test]
fn a_server_that_keeps_failing_is_tried_less_and_less_often() {
    let start = Instant::now();
    let mut session = Session::new(grant(30, 600, 0));
    session.effects();
    let mut waits = Vec::new();
    for _ in 0..8 {
        session.connected(Err(Lost::Failed("no such command".into())), start);
        let effects = session.effects();
        let Some(Effect::Connect { after }) = effects.last() else {
            panic!("{effects:?}");
        };
        waits.push(after.as_millis());
    }
    assert_eq!(waits, [500, 1000, 2000, 4000, 8000, 16000, 30000, 30000]);
}

#[test]
fn a_server_wanting_a_sign_in_is_answered_for_until_the_user_signs_in() {
    let start = Instant::now();
    let mut session = Session::new(grant(30, 600, 0));
    session.effects();
    // The agent's `initialize` waits for the connection to be made.
    session.agent_said(initialize(0), start);
    assert_eq!(session.effects(), []);
    session.connected(
        Err(Lost::NeedsLogin("Authentication required.".into())),
        start,
    );
    let effects = session.effects();
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Report {
            state: McpState::NeedsLogin,
            ..
        }
    )));
    assert!(effects.iter().any(|effect| matches!(effect, Effect::Agent(greeting)
        if greeting["id"] == 0 && greeting["result"]["capabilities"]["tools"]["listChanged"] == true)));
    // No retrying: it waits for the daemon to say the user signed in.
    assert_eq!(effects.last(), Some(&Effect::Disconnect));
    assert_eq!(session.next_due(), None);

    session.agent_said(call(1), start);
    let why = error_for(&session.effects(), 1);
    assert!(
        why.contains("sign in") && why.contains("was not sent"),
        "{why}"
    );

    // Signing in has the daemon grant the next generation.
    session.granted(grant(30, 600, 1));
    assert_eq!(
        session.effects(),
        [Effect::Connect {
            after: Duration::ZERO
        }]
    );
    session.connected(Ok(()), start);
    assert_eq!(session.effects(), [Effect::Server(initialize(0))]);
    session.server_said(initialized(0), start);
    let effects = session.effects();
    assert!(effects.contains(&Effect::Agent(
        json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})
    )));
    // The stand-in promised resources; this server has none, so that
    // list is empty rather than an error from a server never asked.
    session.agent_said(
        json!({"jsonrpc":"2.0","id":2,"method":"resources/list"}),
        start,
    );
    assert_eq!(
        session.effects(),
        [Effect::Agent(
            json!({"jsonrpc":"2.0","id":2,"result":{"resources":[]}})
        )]
    );
}

#[test]
fn while_the_daemon_is_away_calls_are_refused_and_everything_else_goes_on() {
    let start = Instant::now();
    let mut session = connected(30, 600, start);
    allow(&mut session, 1, start);
    session.agent_said(call(2), start);
    session.effects();

    session.daemon_went();
    // The one being asked about is told as its connection closes.
    session.answered("2", Verdict::Away, start);
    assert!(error_for(&session.effects(), 2).contains("daemon isn't answering"));
    // Thirty seconds on it's still away: a new call is refused at once,
    // and can be tried again.
    let later = start + Duration::from_secs(30);
    session.tick(later);
    session.agent_said(call(3), later);
    let why = error_for(&session.effects(), 3);
    assert!(
        why.contains("was not sent") && why.contains("Try again"),
        "{why}"
    );
    // The server is still there: what needs no leave goes to it, and
    // the call already allowed is answered.
    let list = json!({"jsonrpc":"2.0","id":4,"method":"tools/list"});
    session.agent_said(list.clone(), later);
    assert_eq!(session.effects(), [Effect::Server(list)]);
    let answer = json!({"jsonrpc":"2.0","id":1,"result":{"content":[]}});
    session.server_said(answer.clone(), later);
    assert_eq!(session.effects(), [Effect::Agent(answer)]);
    assert!(session.ending().is_none() && session.initialized());

    // Back, with the grant it had: nothing is connected again.
    session.granted(grant(30, 600, 0));
    assert_eq!(session.effects(), []);
    allow(&mut session, 5, later);
}

#[test]
fn a_new_grant_connects_again_in_place_and_answers_what_was_waiting() {
    let start = Instant::now();
    let mut session = connected(30, 600, start);
    allow(&mut session, 1, start);
    session.agent_said(call(2), start);
    session.effects();
    let mut edited = grant(30, 900, 0);
    edited.bound.revision = "edited".into();
    session.granted(edited.clone());
    let effects = session.effects();
    assert_eq!(effects[0], Effect::Unask("2".into()));
    assert!(error_for(&effects, 2).contains("was not sent"));
    assert!(error_for(&effects, 1).contains("isn't known"));
    assert_eq!(
        effects.last(),
        Some(&Effect::Connect {
            after: Duration::ZERO
        })
    );
    assert_eq!(session.grant(), &edited);
    assert!(session.ending().is_none());
}

#[test]
fn cancelling_a_call_that_waits_for_leave_takes_the_question_back() {
    let start = Instant::now();
    let mut session = connected(30, 600, start);
    session.agent_said(call(1), start);
    session.effects();
    let cancel = |id: u32| json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":id}});
    session.agent_said(cancel(1), start);
    // Nothing was sent, so the server isn't told, and a cancelled
    // request is never answered.
    assert_eq!(session.effects(), [Effect::Unask("1".into())]);
    session.answered("1", Verdict::Allowed, start);
    assert_eq!(session.effects(), []);

    // One the server has is cancelled there.
    allow(&mut session, 2, start);
    session.agent_said(cancel(2), start);
    assert_eq!(session.effects(), [Effect::Server(cancel(2))]);
}

#[test]
fn a_refusal_and_a_verdict_are_told_the_agent_in_the_daemons_words() {
    let start = Instant::now();
    let mut session = connected(30, 600, start);
    session.agent_said(call(1), start);
    session.effects();
    session.answered(
        "1",
        Verdict::Refused("The user refused this call.".into()),
        start,
    );
    assert_eq!(
        error_for(&session.effects(), 1),
        "The user refused this call."
    );
}

#[test]
fn every_ending_answers_every_request_still_waiting() {
    let start = Instant::now();
    let mut session = connected(30, 600, start);
    allow(&mut session, 1, start);
    session.agent_said(call(2), start);
    session.agent_said(json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}), start);
    session.effects();
    session.refused("this chat's tools are off".into());
    let effects = session.effects();
    for id in [1, 2, 3] {
        assert!(
            error_for(&effects, id).contains("this chat's tools are off"),
            "{id}"
        );
    }
    assert!(effects.contains(&Effect::Unask("2".into())));
    assert_eq!(effects.last(), Some(&Effect::Disconnect));
    assert_eq!(
        session.ending(),
        Some(&Ending::Refused("this chat's tools are off".into()))
    );
}

#[test]
fn what_either_side_writes_is_passed_on_as_written_unknown_fields_and_all() {
    let start = Instant::now();
    let mut session = connected(30, 600, start);
    let list = json!({"jsonrpc":"2.0","id":"a","method":"tools/list","extra":true,"params":{
        "vendor":{"x":[1,2]}}});
    session.agent_said(list.clone(), start);
    assert_eq!(session.effects(), [Effect::Server(list)]);
    let answer = json!({"jsonrpc":"2.0","id":"a","result":{"tools":[{
        "name":"t","description":"d","inputSchema":{"type":"object"},
        "vendorField":{"k":1},"annotations":{"readOnlyHint":true,"unknownHint":7}}],
        "unknownTop":[null]}});
    session.server_said(answer.clone(), start);
    let effects = session.effects();
    assert_eq!(effects.last(), Some(&Effect::Agent(answer)));
    // What the server asks the agent, and the agent's answer.
    let asks = json!({"jsonrpc":"2.0","id":9.5,"method":"vendor/ask","params":{"q":1}});
    session.server_said(asks.clone(), start);
    assert_eq!(session.effects(), [Effect::Agent(asks)]);
    let said = json!({"jsonrpc":"2.0","id":9.5,"result":{"anything":{"deep":[1]}},"extra":1});
    session.agent_said(said.clone(), start);
    assert_eq!(session.effects(), [Effect::Server(said)]);
}

#[test]
fn too_many_calls_waiting_and_too_long_a_request_have_their_own_words() {
    let start = Instant::now();
    let mut session = connected(30, 600, start);
    for id in 0..16 {
        session.agent_said(call(100 + id), start);
    }
    session.effects();
    session.agent_said(call(200), start);
    let why = error_for(&session.effects(), 200);
    assert!(why.contains("already waiting to be allowed"), "{why}");
    // What needs no leave isn't counted with them.
    let list = json!({"jsonrpc":"2.0","id":300,"method":"tools/list"});
    session.agent_said(list.clone(), start);
    assert_eq!(session.effects(), [Effect::Server(list)]);

    let mut session = connected(30, 600, start);
    let long = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
        "name":"write","arguments":{"text":"x".repeat(300 * 1024)}}});
    session.agent_said(long, start);
    let why = error_for(&session.effects(), 1);
    assert!(why.contains("KiB") && !why.contains("waiting"), "{why}");
}
