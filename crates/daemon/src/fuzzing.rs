//! Entry points for the fuzzer (`fuzz/` at the repository's top), which
//! reach what the crate keeps to itself. Not part of its API: only the
//! `fuzzing` feature builds it.
//!
//! Each takes what another program, another build or the user wrote, as
//! bytes, and reads it as the daemon does. Reading must never panic, and
//! what's read must keep within the caps its reader promises.

use std::ffi::OsString;
use std::path::Path;

use clackatoo_term::{Config, Terminal};
use clackatoo_text::Rope;

use crate::agents::{AgentSpec, DEEPEST_SUBAGENTS, Launch, SubagentLimits};
use crate::buffer::Buffer;
use crate::chat::HostedChat;
use crate::handover::State;
use crate::rpc::Framing;
use crate::workspaces::WorkspaceState;

/// Reads `bytes` as a handover, a state the build before wrote for this
/// one, of any version this build reads; and if it can be read, makes of
/// it everything the daemon makes of a handover that needs no fds: the
/// workspaces, terminals' screens, buffers and chats.
pub fn handover(bytes: &[u8]) {
    let Ok(mut state) = State::from_bytes(bytes) else {
        return;
    };
    if let Ok(mut workspaces) = WorkspaceState::resume(&state.workspaces, &state.lent) {
        workspaces.set_home(Path::new("/home/fuzz"));
        workspaces.resume_background(&state.background);
        workspaces.resume_lent_background(&state.lent_background);
        workspaces.resume_kept(&state.kept);
        if let Some(restored) = &state.restored {
            workspaces.resume_restored(restored);
        }
    }
    for terminal in &state.terminals {
        let config = Config::default();
        if let Ok(mut screen) =
            Terminal::restore_with_links(&terminal.screen, &terminal.links, &config)
        {
            screen.restore_blocks(&terminal.blocks);
            let _ = screen.snapshot_with_links();
        }
    }
    let mut undo_states = std::mem::take(&mut state.undo_states);
    for buffer in std::mem::take(&mut state.buffers) {
        let undo = (undo_states.iter())
            .position(|undo| undo.buffer == buffer.id)
            .map(|at| undo_states.swap_remove(at));
        let _ = Buffer::resume(buffer, undo);
    }
    let agent = AgentSpec {
        name: "fuzz".to_owned(),
        title: "Fuzz".to_owned(),
        launch: Launch::Command(vec![OsString::from("fuzz")]),
        starting_mode: None,
        tasks: true,
        login: None,
    };
    let mut talk = state.talk.take();
    for chat in std::mem::take(&mut state.chats) {
        let mut chat = HostedChat::resume(chat, &agent);
        if chat.resume_uids(&mut state.uids) {
            chat.resume_branches(&mut state.branches);
        }
        chat.resume_forks(&mut state.forks);
        chat.resume_nesting(&state.nested);
        chat.resume_tool_previews(&mut state.tool_previews);
        chat.resume_originals(&mut state.image_originals);
        chat.resume_queue(&mut state.chat_queues);
        chat.resume_draft(&mut state.drafts);
        if let Some(talk) = talk.as_mut() {
            chat.resume_standing(talk, false);
        }
    }
}

/// Reads `bytes` as the saved workspaces file. What's read must be
/// written back as the same.
///
/// # Panics
///
/// If it isn't.
pub fn saved_workspaces(bytes: &[u8]) {
    let Ok(state) = crate::saved::State::decode(bytes) else {
        return;
    };
    let again = crate::saved::State::decode(&state.encode()).expect("what it wrote reads back");
    assert_eq!(again, state);
}

/// Reads `text` as `agents.toml`: its agents, profiles and limits on
/// subagents.
///
/// # Panics
///
/// If a limit it reads is out of its range.
pub fn agents_toml(text: &str) {
    let Ok(document) = text.parse::<toml_edit::DocumentMut>() else {
        return;
    };
    let _ = crate::agents::parse(&document);
    let mut profiles = crate::profiles::built_in();
    let _ = crate::profiles::parse(&document, &mut profiles);
    let (limits, _) = SubagentLimits::from_document(&document);
    assert!((1..=64).contains(&limits.working));
    assert!(limits.default <= limits.working);
    assert!((1..=DEEPEST_SUBAGENTS).contains(&limits.depth));
}

/// Reads `pieces`, one after another, as what an agent (one message a
/// line) or a language server (each message after a header) wrote, as
/// `headers` says, and each message read through every walker that reads
/// what either sends, places in its answers found in `text`.
///
/// # Panics
///
/// If reading it in pieces finds other messages than reading it whole, or
/// a walker keeps more than its caps allow.
pub fn json_rpc(headers: bool, pieces: &[&[u8]], text: &str) {
    let framing = if headers {
        Framing::Headers
    } else {
        Framing::Lines
    };
    let text = Rope::from_str(text);
    for message in crate::rpc::fuzz_read(framing, pieces) {
        crate::agent::fuzz_receive(&message);
        crate::lsp::fuzz_receive(&message, &text);
    }
}
