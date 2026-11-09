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

use crate::agents::{AgentSpec, DEEPEST_SUBAGENTS, Launch};
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
    let Ok((mut state, _)) = State::from_bytes(bytes) else {
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
        let images = state.images.as_ref().map(|images| images.chats.as_slice());
        chat.resume_originals(&mut state.image_originals, images);
        chat.resume_queue(&mut state.chat_queues);
        chat.resume_draft(&mut state.drafts);
        if let Some(talk) = talk.as_mut() {
            chat.resume_standing(talk, false);
        }
        let id = chat.info().id.0;
        let sessions = state.mcp.iter().flat_map(|mcp| &mcp.sessions);
        for (_, session, token) in sessions.filter(|(chat, _, _)| *chat == id) {
            chat.resume_mcp_token(session.clone(), token.clone());
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

/// Moves `text`, as an old `agents.toml`, into the daemon's Luau config,
/// as the first build with it does (see `settings/migrate.rs`), and runs
/// what it comes to: its agents, profiles and limits on subagents.
///
/// # Panics
///
/// If what it comes to doesn't run, or a limit it sets is out of its
/// range.
pub fn agents_toml(text: &str) {
    let lua = crate::settings::migrate_agents_toml(text);
    let settings = crate::settings::Settings::from_lua(&lua)
        .unwrap_or_else(|failure| panic!("{}\n{lua}", failure.said));
    let limits = settings.agents().subagents;
    assert!((1..=64).contains(&limits.working));
    assert!(limits.default <= limits.working);
    assert!((1..=DEEPEST_SUBAGENTS).contains(&limits.depth));
    let mut profiles = crate::profiles::built_in();
    crate::profiles::apply(&settings.agents().profiles, &mut profiles);
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

/// Reads `bytes` as an image the daemon is asked to keep, sent by a
/// program, an agent or a file: what it is, by its header, must be read or
/// refused, never panic, and what's kept must be of a size that may be;
/// and, of a small one, a copy at a smaller size made, as the web app asks.
///
/// # Panics
///
/// If what's read of an image is of a size it may not be.
pub fn image(bytes: &[u8]) {
    let Ok((format, width, height)) = crate::images::look_at(bytes) else {
        return;
    };
    assert!(clackatoo_protocol::ImageInfo::sized(width, height));
    if u64::from(width) * u64::from(height) <= 1 << 16 {
        let info = clackatoo_protocol::ImageInfo {
            id: crate::images::id_of(bytes),
            width,
            height,
            format,
            bytes: bytes.len() as u64,
        };
        let _ = crate::images::decode(info, bytes);
    }
}
