//! Chats for tests: a long one, as a day's work with an agent makes, and
//! short ones; and what tests do with them.

use clackatoo_protocol::{
    ChatNode, ChatState, Diff, Item, Location, Outcome, Tool, ToolContent, ToolKind, ToolStatus,
    Turn, TurnEnd,
};
use std::fmt::Write;

use winit::keyboard::{Key, KeyLocation, NamedKey};

use super::{ChatPaint, ChatView};
use crate::fonts::{FontOptions, Fonts};
use crate::input::{KeyPress, Modifiers};

/// The fonts a test lays chats out with.
pub(crate) fn fonts() -> Fonts {
    Fonts::load(&FontOptions::default(), 1.0).expect("a font")
}

/// Draws `chat` in a pane 900 by 1000 pixels at the window's top left,
/// with the keyboard.
pub(crate) fn paint(chat: &mut ChatView, fonts: &mut Fonts) -> ChatPaint {
    chat.paint(fonts, [0.0; 2], [900.0, 1000.0], true)
}

/// A chat whose turns are done, each a message and an answer of words
/// alone, as `pairs` gives them.
pub(crate) fn talk(pairs: &[(&str, &str)]) -> ChatNode {
    ChatNode {
        state: ChatState::Ready,
        turns: (pairs.iter().enumerate())
            .map(|(index, (prompt, answer))| Turn {
                from: None,
                prompt: (*prompt).to_owned(),
                sent: 1000 * index as u64,
                items: vec![Item::Words((*answer).to_owned())],
                end: Some(TurnEnd {
                    at: 1000 * index as u64 + 500,
                    outcome: Outcome::Done,
                }),
                files: None,
            })
            .collect(),
        ..ChatNode::default()
    }
}

/// Types `keys` in normal mode, a character at a time. Returns what the
/// last came to: whether to redraw, and whether it went to insert mode.
pub(crate) fn read(chat: &mut ChatView, keys: &str) -> (bool, bool) {
    let mut last = (false, false);
    for ch in keys.chars() {
        let text = ch.to_string();
        last = chat.read_key(&KeyPress {
            key: &Key::Character(text.as_str().into()),
            text: Some(&text),
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers::default(),
        });
    }
    last
}

/// Presses `named` in normal mode.
pub(crate) fn read_named(chat: &mut ChatView, named: NamedKey) -> (bool, bool) {
    chat.read_key(&KeyPress {
        key: &Key::Named(named),
        text: None,
        location: KeyLocation::Standard,
        code: None,
        modifiers: Modifiers::default(),
    })
}

/// A chat of `turns` turns, each as an agent's turn at real work goes:
/// a message, some thinking, files read, a command with a long output, a
/// change to a file, and an answer with code in it. All have ended but
/// the last, which is still going.
pub(crate) fn long_chat(turns: usize) -> ChatNode {
    let mut node = ChatNode {
        agent: "Claude Code".into(),
        state: ChatState::Answering,
        ..ChatNode::default()
    };
    for index in 0..turns {
        let sent = 1_000_000 + 600_000 * index as u64;
        let last = index + 1 == turns;
        node.turns.push(Turn {
            from: None,
            prompt: format!(
                "Turn {index}: why does the build fail on CI but not here? It started \
                 after the last dependency bump."
            ),
            sent,
            items: work(index),
            end: (!last).then(|| TurnEnd {
                at: sent + 42_000,
                outcome: Outcome::Done,
            }),
            files: None,
        });
    }
    node
}

/// What the agent does in turn `index`.
fn work(index: usize) -> Vec<Item> {
    let read = |name: &str| {
        Item::Tool(Tool {
            id: format!("{index}-read-{name}"),
            title: format!("Read {name}"),
            status: ToolStatus::Done,
            kind: ToolKind::Read,
            content: Vec::new(),
            locations: Vec::new(),
        })
    };
    let (mut output, mut old) = (String::new(), String::new());
    for line in 0..200 {
        let _ = writeln!(
            output,
            "   Compiling crate-{line} v0.{line}.0 (/src/crates/crate-{line})"
        );
    }
    for line in 0..30 {
        let _ = writeln!(old, "line {line}");
    }
    let new = old
        .replace("line 7\n", "line seven\n")
        .replace("line 20\n", "");
    vec![
        Item::Thought(
            "The failure only happens on CI, so the difference is probably in the \
             environment: the toolchain, the features turned on, or a cached build \
             directory. Let me look at the workflow and the manifest first."
                .into(),
        ),
        read("Cargo.toml"),
        read(".github/workflows/ci.yml"),
        read("build.rs"),
        Item::Words("Let me build it the way CI does.".into()),
        Item::Tool(Tool {
            id: format!("{index}-build"),
            title: "cargo build --locked --all-features".into(),
            status: ToolStatus::Done,
            kind: ToolKind::Execute,
            content: vec![ToolContent::Text(format!("```console\n{output}```"))],
            locations: Vec::new(),
        }),
        Item::Tool(Tool {
            id: format!("{index}-edit"),
            title: "Edit src/lib.rs".into(),
            status: ToolStatus::Done,
            kind: ToolKind::Edit,
            content: vec![ToolContent::Diff(Diff {
                path: "/src/lib.rs".into(),
                old: Some(old),
                new,
            })],
            locations: vec![Location {
                path: "/src/lib.rs".into(),
                line: Some(10),
            }],
        }),
        Item::Words(
            "The build fails on CI because `--all-features` turns on `vendored`, \
             which needs **perl** to build OpenSSL, and the CI image has none.\n\n\
             Either install it in the workflow:\n\n\
             ```yaml\n- run: sudo apt-get install -y perl\n```\n\n\
             or leave `vendored` out of CI's features. I've changed `src/lib.rs` \
             so the error says which it is."
                .into(),
        ),
    ]
}
