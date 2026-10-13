//! Sample messages of the core flows, to pin down what each protocol
//! version's bytes are.
//!
//! Each release commits its protocol version's samples, encoded, as a
//! fixture: `fixtures/protocol-vP.bin`. A build that still talks to peers
//! of version P must encode [`corpus(P)`](corpus) at P to exactly those
//! bytes, or a field was added without being gated, something was
//! reordered, or a type changed, here or in the terminal's or the text's
//! types the messages carry.
//!
//! The samples fill in everything: every `Option` is `Some` and every `Vec`
//! has something in it, so a field added anywhere they reach changes the
//! bytes. A sample new in a version is tagged with it, and left out of
//! older versions' corpora. Samples are never changed or removed while a
//! fixture holds them.

use std::num::NonZeroU32;

use clackatoo_term::{
    Attributes, Color, CursorShape, InputModes, LinkId, ShellState, Size, Style, Underline,
};
use clackatoo_text::{AgentHunk, Diagnostic, Edit, Hunk, Kind, Severity, Span};

use crate::frame::{self, FrameDecoder, FrameError};
use crate::{
    AgentSession, AgentTools, Author, Axis, BoardId, BufferId, BufferSnapshot, BufferUpdate,
    Change, ChatAction, ChatName, ChatNode, ChatPatch, ChatState, Choice, ChoiceKind,
    ClientMessage, Content, CursorState, Diff, Error, Event, FilesId, GridNode, GridState,
    HistoryLine, HistoryLines, Item, Layout, Line, LinkTarget, Location, NewAgent, NewTerminal,
    Node, NodeId, Outcome, Pane, PaneId, PaneTerminal, Patch, Provider, ProviderModels, Question,
    Request, Response, Resume, ResumeSession, ReviewId, Run, Sender, ServerMessage, Setting,
    SettingChoice, SettingKind, Side, Subagent, SubagentStatus, Switch, Tab, TabId, TerminalId,
    TerminalInfo, TerminalStatus, Tool, ToolContent, ToolKind, ToolStatus, Turn, TurnEnd,
    TurnFiles, TurnFilesState, Usage, View, ViewUpdate, Workspace, WorkspaceId, Workspaces,
};

/// One sample: a message either side sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sample {
    /// A client's.
    Client(ClientMessage),
    /// The daemon's.
    Server(ServerMessage),
}

/// The protocol version the corpus began at: no sample is older.
pub const FIRST: u32 = 72;

/// The samples a peer speaking protocol `wire` sends and reads, in order.
#[must_use]
pub fn corpus(wire: u32) -> Vec<Sample> {
    tagged(wire)
        .into_iter()
        .filter(|&(since, _)| since <= wire)
        .map(|(_, sample)| sample)
        .collect()
}

/// The fixture of protocol `wire`: [`corpus(wire)`](corpus), each sample
/// a frame encoded at `wire`, one after the other.
///
/// # Panics
///
/// If a sample can't be encoded, which they all can.
#[must_use]
pub fn fixture(wire: u32) -> Vec<u8> {
    corpus(wire)
        .iter()
        .flat_map(|sample| {
            match sample {
                Sample::Client(message) => frame::encode(message),
                Sample::Server(message) => frame::encode(message),
            }
            .expect("samples encode")
        })
        .collect()
}

/// What the fixture of protocol `wire` is called.
#[must_use]
pub fn fixture_name(wire: u32) -> String {
    format!("protocol-v{wire}.bin")
}

/// Reads the fixture of protocol `wire` back, each frame as the sample in
/// its place in [`corpus(wire)`](corpus) is: a client's message or the
/// daemon's.
///
/// # Errors
///
/// If a frame can't be decoded as one, or there are more or fewer frames
/// than samples.
pub fn read_fixture(wire: u32, bytes: &[u8]) -> Result<Vec<Sample>, FrameError> {
    let mut decoder = FrameDecoder::new();
    decoder.push(bytes);
    let mut read = Vec::new();
    for sample in corpus(wire) {
        let next = match sample {
            Sample::Client(_) => decoder.decode()?.map(Sample::Client),
            Sample::Server(_) => decoder.decode()?.map(Sample::Server),
        };
        read.push(next.ok_or(FrameError::TrailingBytes(0))?);
    }
    if decoder.has_partial_frame() {
        return Err(FrameError::TrailingBytes(bytes.len()));
    }
    Ok(read)
}

/// Every sample, with the version it's in from.
#[allow(clippy::too_many_lines, reason = "a list of samples")]
fn tagged(wire: u32) -> Vec<(u32, Sample)> {
    use Sample::{Client, Server};
    let terminal = TerminalId(7);
    let buffer = BufferId(3);
    vec![
        // Opening a connection.
        (
            FIRST,
            Client(ClientMessage::Hello {
                protocol_version: wire,
            }),
        ),
        (
            FIRST,
            Server(ServerMessage::Hello {
                protocol_version: wire,
            }),
        ),
        // Workspaces.
        (
            FIRST,
            Client(ClientMessage::Request(Request::WatchWorkspaces)),
        ),
        (
            FIRST,
            Server(ServerMessage::Response(Response::Workspaces(workspaces()))),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::Workspaces(workspaces()))),
        ),
        (
            FIRST,
            Client(ClientMessage::Request(Request::Split {
                pane: PaneId(1),
                side: Side::Right,
                terminal: PaneTerminal::New(new_terminal()),
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Response(Response::PaneCreated(PaneId(9)))),
        ),
        (
            FIRST,
            Client(ClientMessage::Request(Request::ListTerminals)),
        ),
        (
            FIRST,
            Server(ServerMessage::Response(Response::Terminals(vec![
                TerminalInfo {
                    id: terminal,
                    pid: Some(4242),
                    size: size(),
                    title: "vim".into(),
                    status: TerminalStatus::Running,
                },
            ]))),
        ),
        // A terminal: following its view, typing, resizing.
        (
            FIRST,
            Client(ClientMessage::Request(Request::Attach { terminal })),
        ),
        (FIRST, Server(ServerMessage::Response(Response::Done))),
        (
            FIRST,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Snapshot(View {
                    title: "vim".into(),
                    cwd: Some("/home/jack".into()),
                    root: Node::Grid(GridNode {
                        size: size(),
                        state: grid_state(),
                        lines: vec![line("$ ls"), line("Cargo.toml")],
                        links: links(),
                    }),
                }),
            })),
        ),
        (FIRST, Client(ClientMessage::FrameDone { terminal })),
        (
            FIRST,
            Client(ClientMessage::Request(Request::SendInput {
                terminal,
                bytes: b"ls\r".to_vec(),
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Patch(vec![
                    Patch::Title("ls".into()),
                    Patch::Cwd(Some("/tmp".into())),
                    Patch::Grid {
                        node: NodeId::ROOT,
                        state: grid_state(),
                        lines: vec![(1, line("src"))],
                        links: links(),
                    },
                ]),
            })),
        ),
        (
            FIRST,
            Client(ClientMessage::Request(Request::Resize {
                terminal,
                size: size(),
            })),
        ),
        (
            FIRST,
            Client(ClientMessage::Request(Request::History {
                terminal,
                end: 100,
                count: 2,
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Response(Response::History(HistoryLines {
                first: 98,
                lines: vec![HistoryLine {
                    width: 80,
                    line: line("old"),
                }],
                links: links(),
            }))),
        ),
        (
            FIRST,
            Server(ServerMessage::Response(Response::Error(
                Error::NoSuchTerminal(terminal),
            ))),
        ),
        // A buffer: attaching, editing, and what the daemon says of it.
        (
            FIRST,
            Client(ClientMessage::Request(Request::AttachBuffer {
                buffer,
                window: 0x5eed,
                resume: Some(Resume {
                    daemon: 11,
                    version: 4,
                }),
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Snapshot(BufferSnapshot {
                    path: "/src/main.rs".into(),
                    text: "fn main() {}\n".into(),
                    version: 4,
                    modified: true,
                    applied: Some(8),
                    landed: Some(3),
                    daemon: 11,
                }),
            })),
        ),
        (
            FIRST,
            Client(ClientMessage::Edit {
                buffer,
                seq: 9,
                change: Change::Edit {
                    edit: Edit::replace(13, 3, "main", "start"),
                    base: 4,
                    step: true,
                },
            }),
        ),
        (
            FIRST,
            Client(ClientMessage::Edit {
                buffer,
                seq: 10,
                change: Change::Undo,
            }),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Acked { version: 5, seq: 9 },
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Edited {
                    version: 6,
                    edit: Edit::replace(14, 0, "", "// "),
                    author: Author::Agent(terminal),
                    answers: Some(10),
                },
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Resumed {
                    daemon: 11,
                    path: "/src/main.rs".into(),
                    edits: vec![Edit::replace(13, 0, "", "x")],
                    applied: Some(8),
                    landed: Some(5),
                    modified: true,
                },
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Highlights {
                    version: 6,
                    start: 0,
                    end: 13,
                    spans: vec![Span {
                        start: 0,
                        end: 2,
                        kind: Kind::Keyword,
                    }],
                },
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Diagnostics {
                    version: 6,
                    diagnostics: vec![Diagnostic {
                        start: 3,
                        end: 7,
                        severity: Severity::Warning,
                        message: "unused".into(),
                        source: Some("rustc".into()),
                        code: Some("W1".into()),
                    }],
                },
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Hunks {
                    version: 6,
                    hunks: vec![Hunk {
                        start: 0,
                        end: 13,
                        base: "fn old() {}\n".into(),
                    }],
                },
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Agents {
                    version: 6,
                    hunks: vec![AgentHunk {
                        id: 1,
                        start: 0,
                        end: 13,
                        base: "fn old() {}\n".into(),
                        chat: terminal.0,
                        turn: 2,
                        reviewed: true,
                    }],
                    chats: vec![ChatName {
                        chat: terminal,
                        name: "Claude".into(),
                    }],
                },
            })),
        ),
        // A chat: its view, how it changes, and answering its question.
        (
            FIRST,
            Client(ClientMessage::Request(Request::Split {
                pane: PaneId(1),
                side: Side::Below,
                terminal: PaneTerminal::Agent(NewAgent {
                    size: size(),
                    cwd: Some("/src".into()),
                    agent: Some("claude".into()),
                    tools: AgentTools::Allow,
                    task: true,
                    browser: true,
                    model: Some("haiku".into()),
                    resume: Some(ResumeSession {
                        session: "s-1".into(),
                        title: "Earlier".into(),
                    }),
                }),
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Snapshot(View {
                    title: "Claude".into(),
                    cwd: Some("/src".into()),
                    root: Node::Chat(Box::new(chat())),
                }),
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Patch(
                    [
                        ChatPatch::Turn {
                            index: 1,
                            turn: turn(),
                        },
                        ChatPatch::Item {
                            turn: 1,
                            item: 0,
                            value: Item::Tool(tool()),
                        },
                        ChatPatch::Append {
                            turn: 1,
                            item: 1,
                            text: "more".into(),
                        },
                        ChatPatch::Output {
                            turn: 1,
                            item: 0,
                            text: "ran\n".into(),
                        },
                        ChatPatch::State(ChatState::Failed("gone".into())),
                        ChatPatch::Question(Some(question())),
                        ChatPatch::Notice(Some("slow".into())),
                        ChatPatch::Settings(vec![setting()]),
                        ChatPatch::Usage(Some(usage())),
                        ChatPatch::Files {
                            turn: 1,
                            files: Some(files()),
                        },
                        ChatPatch::Agent("codex".into()),
                        ChatPatch::Providers(vec![provider()]),
                        ChatPatch::Subagents(vec![subagent()]),
                    ]
                    .into_iter()
                    .map(Patch::Chat)
                    .collect(),
                ),
            })),
        ),
        (
            FIRST,
            Client(ClientMessage::Request(Request::Chat {
                terminal,
                action: ChatAction::Send("hello".into()),
            })),
        ),
        (
            FIRST,
            Client(ClientMessage::Request(Request::Chat {
                terminal,
                action: ChatAction::Answer {
                    question: 5,
                    option: Some("allow".into()),
                },
            })),
        ),
        (
            FIRST,
            Server(ServerMessage::Event(Event::Sessions {
                id: 2,
                sessions: Ok(vec![AgentSession {
                    session: "s-1".into(),
                    title: "Earlier".into(),
                    cwd: "/src".into(),
                    updated: Some("2026-10-13".into()),
                    chat: Some(terminal),
                }]),
            })),
        ),
    ]
}

fn size() -> Size {
    Size::new(80, 24).expect("80x24 is a size")
}

fn new_terminal() -> NewTerminal {
    NewTerminal {
        size: size(),
        command: Some(vec!["bash".into(), "-l".into()]),
        cwd: Some("/src".into()),
        record_to: Some("/tmp/out".into()),
    }
}

/// Workspaces with a pane of every kind.
fn workspaces() -> Workspaces {
    let pane = |id, content| {
        Layout::Pane(Pane {
            id: PaneId(id),
            content,
        })
    };
    Workspaces {
        list: vec![Workspace {
            id: WorkspaceId(1),
            name: "clackatoo".into(),
            tabs: vec![Tab {
                id: TabId(2),
                layout: Layout::Split {
                    axis: Axis::LeftToRight,
                    parts: vec![
                        pane(1, Content::Terminal(TerminalId(7))),
                        Layout::Split {
                            axis: Axis::TopToBottom,
                            parts: vec![
                                pane(2, Content::Buffer(BufferId(3))),
                                pane(3, Content::Files(FilesId(4))),
                                pane(4, Content::Board(BoardId(5))),
                                pane(5, Content::Review(ReviewId(6))),
                                pane(6, Content::Browser(TerminalId(7))),
                                pane(7, Content::Folder(FilesId(8))),
                            ],
                            shares: vec![1, 1, 1, 1, 1, 1],
                        },
                    ],
                    shares: vec![2, 3],
                },
                focused: PaneId(1),
                name: Some("main".into()),
            }],
            active_tab: TabId(2),
        }],
        active: Some(WorkspaceId(1)),
    }
}

fn grid_state() -> GridState {
    GridState {
        cursor: CursorState {
            line: 1,
            col: 4,
            visible: true,
            shape: CursorShape::Bar,
        },
        modes: InputModes {
            bracketed_paste: true,
            ..InputModes::default()
        },
        alternate: true,
        history_end: 98,
        shell: ShellState::Prompt,
        block: Some(12),
    }
}

/// A line of `text` in three runs, one of each kind of colour, the last a
/// link.
fn line(text: &str) -> Line {
    let style = Style {
        fg: Color::Indexed(1),
        bg: Color::Rgb(1, 2, 3),
        underline_color: Color::Default,
        attributes: Attributes::BOLD | Attributes::ITALIC,
        underline: Underline::Curly,
        link: Some(link()),
    };
    Line {
        runs: vec![
            Run {
                style: Style::DEFAULT,
                text: "> ".into(),
                separate: false,
            },
            Run {
                style,
                text: text.into(),
                separate: true,
            },
        ],
        wrapped: true,
    }
}

fn link() -> LinkId {
    LinkId(NonZeroU32::new(1).expect("1 isn't 0"))
}

fn links() -> Vec<(LinkId, LinkTarget)> {
    vec![(
        link(),
        LinkTarget {
            uri: "https://clackatoo.dev".into(),
            named: true,
        },
    )]
}

fn chat() -> ChatNode {
    ChatNode {
        agent: "Claude Code".into(),
        turns: vec![turn()],
        state: ChatState::Answering,
        question: Some(question()),
        notice: Some("slow".into()),
        settings: vec![setting()],
        usage: Some(usage()),
        providers: vec![provider()],
        subagents: vec![subagent()],
    }
}

fn turn() -> Turn {
    Turn {
        prompt: "fix it".into(),
        sent: 1_760_000_000,
        items: vec![
            Item::Words("Done.".into()),
            Item::Thought("Hmm.".into()),
            Item::Tool(tool()),
        ],
        end: Some(TurnEnd {
            at: 1_760_000_060,
            outcome: Outcome::Failed("stopped".into()),
        }),
        files: Some(files()),
        from: Some(Sender {
            chat: TerminalId(8),
            agent: "codex".into(),
            title: "Helper".into(),
        }),
        switch: Some(Switch {
            from: "Claude Code".into(),
            to: "Codex".into(),
            model: Some("gpt".into()),
            resumed: true,
            notes: vec!["mode".into()],
        }),
    }
}

fn tool() -> Tool {
    Tool {
        id: "t1".into(),
        title: "Edit main.rs".into(),
        status: ToolStatus::Done,
        kind: ToolKind::Edit,
        content: vec![
            ToolContent::Text("why".into()),
            ToolContent::Diff(Diff {
                path: "/src/main.rs".into(),
                old: Some("a".into()),
                new: "b".into(),
            }),
            ToolContent::Output("ok\n".into()),
        ],
        locations: vec![Location {
            path: "/src/main.rs".into(),
            line: Some(1),
        }],
    }
}

fn question() -> Question {
    Question {
        id: 5,
        title: "Allow the agent to run ls?".into(),
        choices: vec![Choice {
            id: "allow".into(),
            name: "Allow".into(),
            kind: ChoiceKind::AllowOnce,
        }],
        own: true,
    }
}

fn setting() -> Setting {
    Setting {
        id: "model".into(),
        name: "Model".into(),
        kind: SettingKind::Model,
        value: "haiku".into(),
        choices: vec![choice()],
    }
}

fn choice() -> SettingChoice {
    SettingChoice {
        value: "haiku".into(),
        name: "Haiku".into(),
        description: Some("fast".into()),
    }
}

fn usage() -> Usage {
    Usage {
        used: 1000,
        size: 200_000,
    }
}

fn files() -> TurnFiles {
    TurnFiles {
        count: 2,
        state: TurnFilesState::Partly,
    }
}

fn provider() -> Provider {
    Provider {
        agent: "claude".into(),
        title: "Claude Code".into(),
        models: ProviderModels::Known(vec![choice()]),
    }
}

fn subagent() -> Subagent {
    Subagent {
        chat: TerminalId(8),
        agent: "claude".into(),
        profile: Some("explore".into()),
        model: Some("haiku".into()),
        task: "look".into(),
        status: SubagentStatus::Failed("no".into()),
        started: 1_760_000_000,
        ended: Some(1_760_000_030),
        turn: 0,
        shown: true,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::PROTOCOL_VERSION;

    /// The committed fixtures, by protocol version.
    fn fixtures() -> Vec<(u32, Vec<u8>)> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut fixtures: Vec<(u32, Vec<u8>)> = std::fs::read_dir(&dir)
            .expect("the fixtures are committed")
            .map(|entry| entry.expect("the fixtures can be listed").path())
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?;
                let wire = name.strip_prefix("protocol-v")?.strip_suffix(".bin")?;
                Some((wire.parse().ok()?, std::fs::read(&path).ok()?))
            })
            .collect();
        fixtures.sort_unstable();
        fixtures
    }

    #[test]
    fn each_fixture_is_still_what_its_version_encodes_to() {
        let fixtures = fixtures();
        assert!(!fixtures.is_empty());
        for (wire, bytes) in fixtures {
            assert!(
                wire <= PROTOCOL_VERSION,
                "a fixture from the future: {wire}"
            );
            assert!(
                fixture(wire) == bytes,
                "protocol {wire}'s samples no longer encode to {}: a field was added \
                 without a gate, or something was reordered or changed",
                fixture_name(wire)
            );
        }
    }

    #[test]
    fn each_fixture_reads_back_as_its_samples() {
        for (wire, bytes) in fixtures() {
            let read = read_fixture(wire, &bytes).unwrap();
            assert_eq!(read, corpus(wire), "protocol {wire}");
        }
    }

    #[test]
    fn samples_are_never_newer_than_the_protocol() {
        assert!(
            tagged(PROTOCOL_VERSION)
                .iter()
                .all(|&(since, _)| (FIRST..=PROTOCOL_VERSION).contains(&since))
        );
        assert_eq!(corpus(PROTOCOL_VERSION).len(), tagged(0).len());
    }
}
