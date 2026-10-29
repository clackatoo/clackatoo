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

use clackatoo_term::mouse::{MouseEncoding, MouseTracking};
use clackatoo_term::{
    Attributes, Block, Color, CursorShape, InputModes, LinkId, Match, Position, Selection,
    ShellState, Size, Style, Underline, Which,
};
use clackatoo_text::{
    AgentHunk, Construct, Diagnostic, Edit, Hunk, Kind, LineIndent, Piece, Severity, Span,
};

use crate::frame::{self, FrameDecoder, FrameError};
use crate::wire::{
    BACKGROUND, BLANK, BLOCK_MARKS, BLOCKS, CHAIN, COLOURS, EXITED, FALLBACKS, FEDERATION,
    FORWARD_PACE, FORWARDS, HOVER, INLAY_HINTS, LEND, LIMITS, MIXED, PHONE_TERMINALS,
    PREVIEW_COLOURS, RECOVER, RELAY, RESTORED, SYMBOLS, USES, WORDS,
};
use crate::{
    AgentSession, AgentTools, Author, Axis, BlockMarks, BoardId, BufferId, BufferSnapshot,
    BufferUpdate, Change, ChatAction, ChatName, ChatNode, ChatPatch, ChatState, Choice, ChoiceKind,
    ClientMessage, Content, CursorState, Diff, Error, Event, FilesId, GridNode, GridState,
    HistoryLine, HistoryLines, Item, Layout, Lent, Limit, Line, LinkTarget, Location, NewAgent,
    NewReview, NewTerminal, Node, NodeId, Outcome, Pane, PaneId, PaneKind, PaneTerminal, Patch,
    Provider, ProviderModels, Question, Request, Response, Resume, ResumeSession, ReviewId, Run,
    Sender, ServerMessage, Setting, SettingChoice, SettingKind, Side, Subagent, SubagentStatus,
    Switch, Tab, TabId, TerminalId, TerminalInfo, TerminalStatus, Tool, ToolContent, ToolKind,
    ToolStatus, Turn, TurnEnd, TurnFiles, TurnFilesState, Usage, View, ViewUpdate, Workspace,
    WorkspaceId, Workspaces,
};
use crate::{
    Answer, BoardAction, BoardAuthor, BoardEntry, BoardLink, BoardScope, BoardSummary, BoardUpdate,
    BrowserFrame, BrowserStatus, BrowserUpdate, ChatChoices, ChatSummary, CodeAction, CommentPlace,
    CompletionItem, Connection, ConnectionState, Device, Done, Entry, EntryKind, Expect,
    FileLocation, FileOp, FileOpError, FileStamp, FilesAction, FilesUpdate, Forward,
    ForwardFailure, Forwarding, Found, FoundLine, FoundSymbol, GitMark, Hunks, InlayHint,
    InlayHintKind, Machine, MachineId, Made, OpenBlank, OpenBoard, OpenFile, OpenFiles, PairingAsk,
    PairingState, Place, PostKind, Query, Restored, ReviewAction, ReviewChat, ReviewComment,
    ReviewFile, ReviewHunk, ReviewList, ReviewScope, Search, Secret, ServerFeatures, Symbol,
    TextChange, Trigger, Udp, Use, UseKind,
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
                Sample::Client(message) => frame::encode(wire, message),
                Sample::Server(message) => frame::encode(wire, message),
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
/// than samples, saying which.
pub fn read_fixture(wire: u32, bytes: &[u8]) -> Result<Vec<Sample>, String> {
    let mut decoder = FrameDecoder::new();
    decoder.push(bytes);
    let mut read = Vec::new();
    for (index, sample) in corpus(wire).into_iter().enumerate() {
        let next: Result<Option<Sample>, FrameError> = match sample {
            Sample::Client(_) => decoder.decode(wire).map(|next| next.map(Sample::Client)),
            Sample::Server(_) => decoder.decode(wire).map(|next| next.map(Sample::Server)),
        };
        match next {
            Ok(Some(sample)) => read.push(sample),
            Ok(None) => return Err(format!("the fixture ends before sample {index}")),
            Err(err) => return Err(format!("sample {index}: {err}")),
        }
    }
    if decoder.has_partial_frame() {
        return Err("the fixture has more than its samples".to_owned());
    }
    Ok(read)
}

/// Every sample, with the version it's in from.
///
/// A sample new in a version is tagged with it, and left out of older
/// versions' corpora. A field gated at a version is filled in only when
/// `wire` is that version or later: an older peer neither sends nor reads
/// it, so the sample at its version is as that peer has it.
#[allow(clippy::too_many_lines, reason = "a list of samples")]
pub(crate) fn tagged(wire: u32) -> Vec<(u32, Sample)> {
    use Sample::{Client, Server};
    let terminal = TerminalId(7);
    let buffer = BufferId(3);
    let mut samples = vec![
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
                on: None,
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
                    cwd: None,
                    command: None,
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
                        marks: BlockMarks::default(),
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
                        marks: BlockMarks::default(),
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
                marks: BlockMarks::default(),
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
                on: None,
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
                            turn: Box::new(turn()),
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
    ];
    samples.extend(more().into_iter().map(|sample| (FIRST, sample)));
    // The fallbacks a newer daemon sends in place of what's newer.
    samples.extend([
        (
            FALLBACKS,
            Server(ServerMessage::Event(Event::Workspaces(Workspaces {
                list: vec![Workspace {
                    id: WorkspaceId(1),
                    name: "newer".into(),
                    tabs: vec![Tab {
                        id: TabId(2),
                        layout: Layout::Pane(Pane {
                            id: PaneId(1),
                            content: Content::Other {
                                kind: PaneKind::new("notebook"),
                            },
                            home: None,
                            key: None,
                        }),
                        focused: PaneId(1),
                        name: None,
                    }],
                    active_tab: TabId(2),
                    home: None,
                    lent: None,
                    background: Vec::new(),
                }],
                active: None,
                home: None,
                restored: Vec::new(),
                machine: None,
                others: Vec::new(),
                passed: Vec::new(),
                kept: Vec::new(),
            }))),
        ),
        (
            FALLBACKS,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Snapshot(View {
                    title: "newer".into(),
                    cwd: None,
                    root: Node::Other,
                }),
            })),
        ),
        (
            FALLBACKS,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Patch(vec![
                    Patch::Chat(ChatPatch::Item {
                        turn: 0,
                        item: 0,
                        value: Item::Other("a picture".into()),
                    }),
                    Patch::Chat(ChatPatch::Item {
                        turn: 0,
                        item: 1,
                        value: Item::Tool(Tool {
                            content: vec![ToolContent::Other("a chart".into())],
                            ..tool()
                        }),
                    }),
                ]),
            })),
        ),
        (
            FALLBACKS,
            Server(ServerMessage::Response(Response::Error(Error::Unsupported))),
        ),
        (
            FALLBACKS,
            Server(ServerMessage::Response(Response::Error(Error::Other(
                "no such notebook".into(),
            )))),
        ),
    ]);
    // A terminal's program ended, said before its pane closes.
    samples.push((
        EXITED,
        Server(ServerMessage::Event(Event::Exited { terminal })),
    ));
    // Terminals from a phone: the list says where each is and what it
    // runs, and a phone fits one to its screen for a while, then puts it
    // back.
    samples.extend([
        (
            PHONE_TERMINALS,
            Server(ServerMessage::Response(Response::Terminals(vec![
                TerminalInfo {
                    id: terminal,
                    pid: Some(4242),
                    size: size(),
                    title: "make".into(),
                    status: TerminalStatus::Running,
                    cwd: Some("/home/jack/clackatoo".into()),
                    command: Some("cargo test".into()),
                },
            ]))),
        ),
        (
            PHONE_TERMINALS,
            Client(ClientMessage::Request(Request::Fit {
                terminal,
                size: Some(size()),
                renew: true,
            })),
        ),
        (
            PHONE_TERMINALS,
            Client(ClientMessage::Request(Request::Fit {
                terminal,
                size: None,
                renew: false,
            })),
        ),
        (
            PHONE_TERMINALS,
            Server(ServerMessage::Response(Response::Error(
                Error::NotFollowing(terminal),
            ))),
        ),
        (
            PHONE_TERMINALS,
            Server(ServerMessage::Response(Response::Error(Error::NotFitted(
                terminal,
            )))),
        ),
    ]);
    // What's at a place, with the text it's about, then its code coloured.
    samples.extend([
        (
            HOVER,
            Server(ServerMessage::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Answer {
                    id: 4,
                    version: 6,
                    answer: Answer::Hover {
                        text: "```rust\nfn a()\n```".into(),
                        markdown: true,
                        range: Some((3, 4)),
                    },
                },
            })),
        ),
        (
            HOVER,
            Server(ServerMessage::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Coloured {
                    id: 4,
                    spans: vec![Span {
                        start: 8,
                        end: 10,
                        kind: Kind::Keyword,
                    }],
                },
            })),
        ),
    ]);
    samples.extend(symbols(buffer).into_iter().map(|sample| (SYMBOLS, sample)));
    // What a buffer's words are made of, in a language with names such as
    // `margin-top`.
    samples.push((
        WORDS,
        Server(ServerMessage::Event(Event::Buffer {
            buffer,
            update: BufferUpdate::Language { word: "-?".into() },
        })),
    ));
    samples.extend(uses(buffer).into_iter().map(|sample| (USES, sample)));
    samples.extend(
        inlay_hints(buffer)
            .into_iter()
            .map(|sample| (INLAY_HINTS, sample)),
    );
    // An account's limits, beside the context: one reached.
    samples.push((
        LIMITS,
        Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Usage(Some(Usage {
                used: 84_000,
                size: 200_000,
                five_hour: Some(Limit {
                    used: 42,
                    resets: Some(1_760_018_400_000),
                }),
                weekly: Some(Limit {
                    used: 100,
                    resets: Some(1_760_400_000_000),
                }),
                cost: None,
            })))]),
        })),
    ));
    samples.extend(forwards().into_iter().map(|sample| (FORWARDS, sample)));
    // A forwarded connection asked to go as fast as it can.
    samples.push((
        FORWARD_PACE,
        Client(ClientMessage::Request(Request::Forward(Forward {
            host: "::1".into(),
            port: 5173,
            unpaced: true,
        }))),
    ));
    // The code in an agent's words, asked about and coloured.
    samples.extend([
        (
            COLOURS,
            Client(ClientMessage::Colour {
                id: 9,
                code: vec![crate::Code {
                    language: "rust".into(),
                    text: "fn main() {}".into(),
                }],
            }),
        ),
        (
            COLOURS,
            Server(ServerMessage::Event(Event::Coloured {
                id: 9,
                spans: vec![vec![Span {
                    start: 0,
                    end: 2,
                    kind: Kind::Keyword,
                }]],
            })),
        ),
    ]);
    // A preview of an open file, coloured as its buffer is.
    samples.push((
        PREVIEW_COLOURS,
        Server(ServerMessage::Event(Event::Found {
            id: 9,
            found: Found::Preview {
                first: 2,
                lines: vec!["fn a() {".into(), "}".into()],
                spans: vec![Span {
                    start: 0,
                    end: 2,
                    kind: Kind::Keyword,
                }],
            },
        })),
    ));
    // A shell's blocks: where their prompts start on the screen, in a
    // patch and in history, and how they ended; and the daemon's home.
    let marks = |prompts: Vec<(u16, u32)>| BlockMarks {
        prompts,
        exits: vec![(4, 2), (5, 0)],
    };
    samples.extend([
        (
            BLOCK_MARKS,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Snapshot(View {
                    title: String::new(),
                    cwd: Some("/home/jack".into()),
                    root: Node::Grid(GridNode {
                        size: size(),
                        state: grid_state(),
                        lines: vec![line("$ false"), line("$ true")],
                        links: links(),
                        marks: marks(vec![(0, 4), (1, 5)]),
                    }),
                }),
            })),
        ),
        (
            BLOCK_MARKS,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Patch(vec![Patch::Grid {
                    node: NodeId::ROOT,
                    state: grid_state(),
                    lines: vec![(2, line("$ ls"))],
                    links: links(),
                    marks: marks(vec![(2, 6)]),
                }]),
            })),
        ),
        (
            BLOCK_MARKS,
            Server(ServerMessage::Response(Response::History(HistoryLines {
                first: 98,
                lines: vec![HistoryLine {
                    width: 80,
                    line: line("$ false"),
                }],
                links: links(),
                marks: marks(vec![(0, 4)]),
            }))),
        ),
        (
            BLOCK_MARKS,
            Server(ServerMessage::Response(Response::Workspaces(Workspaces {
                home: Some("/home/jack".into()),
                ..workspaces()
            }))),
        ),
    ]);
    // A block of a terminal's columns copied, to each line's end.
    samples.push((
        BLOCKS,
        Client(ClientMessage::Request(Request::BlockText {
            terminal,
            from: Position { line: 3, col: 2 },
            to: Position {
                line: 7,
                col: usize::MAX,
            },
        })),
    ));
    // A buffer with no file yet, in a new tab and in place of a file, and
    // named once it's written.
    samples.extend([
        (
            BLANK,
            Client(ClientMessage::Request(Request::NewTab {
                workspace: WorkspaceId(1),
                terminal: PaneTerminal::Blank(OpenBlank {
                    cwd: Some("/home/jack/src".into()),
                }),
                on: None,
            })),
        ),
        (
            BLANK,
            Client(ClientMessage::Request(Request::BlankHere {
                pane: PaneId(3),
                blank: OpenBlank { cwd: None },
            })),
        ),
        (
            BLANK,
            Client(ClientMessage::Request(Request::SaveAs {
                buffer: BufferId(4),
                path: "notes.md".into(),
                force: true,
            })),
        ),
    ]);
    // Changes not saved that came back from recovery files, taken or
    // shown, and dropped.
    samples.extend([
        (
            RECOVER,
            Client(ClientMessage::Request(Request::Recover {
                pane: PaneId(3),
                drop: false,
            })),
        ),
        (
            RECOVER,
            Client(ClientMessage::Request(Request::Recover {
                pane: PaneId(5),
                drop: true,
            })),
        ),
    ]);
    // Workspaces made again as the daemon started: a terminal's command to
    // offer to run again, and an editor's place, which windows say and
    // dismiss.
    let place = Place {
        line: 120,
        column: 8,
        top: 101,
    };
    samples.extend([
        (
            RESTORED,
            Server(ServerMessage::Event(Event::Workspaces(Workspaces {
                restored: vec![
                    Restored {
                        pane: PaneId(1),
                        ran: Some("cargo watch -x test".into()),
                        place: None,
                    },
                    Restored {
                        pane: PaneId(2),
                        ran: None,
                        place: Some(place),
                    },
                ],
                ..workspaces()
            }))),
        ),
        (
            RESTORED,
            Client(ClientMessage::Place {
                pane: PaneId(2),
                place,
            }),
        ),
        (RESTORED, Client(ClientMessage::Dismiss { pane: PaneId(1) })),
    ]);
    samples.extend(federation().into_iter().map(|sample| (FEDERATION, sample)));
    samples.extend(relay().into_iter().map(|sample| (RELAY, sample)));
    samples.extend(lend().into_iter().map(|sample| (LEND, sample)));
    samples.extend(mixed().into_iter().map(|sample| (MIXED, sample)));
    samples.extend(chain().into_iter().map(|sample| (CHAIN, sample)));
    samples.extend(background().into_iter().map(|sample| (BACKGROUND, sample)));
    samples.extend(kept().into_iter().map(|sample| (BACKGROUND, sample)));
    samples.push((
        crate::wire::CHAT_IMAGES,
        Client(ClientMessage::Request(Request::Chat {
            terminal,
            action: ChatAction::SendImages {
                text: "Look at this".into(),
                images: vec![crate::ChatImage {
                    preview: None,
                    data: "cG5n".into(),
                    width: 2,
                    height: 3,
                }],
            },
        })),
    ));
    samples.push((
        crate::wire::CHAT_PREVIEWS,
        Client(ClientMessage::Request(Request::Chat {
            terminal,
            action: ChatAction::SendImages {
                text: "Thumbnail".into(),
                images: vec![crate::ChatImage {
                    data: "cG5n".into(),
                    width: 1,
                    height: 1,
                    preview: Some(crate::ChatPreview {
                        original: 0,
                        width: 1,
                        height: 1,
                        rgba: vec![25, 50, 75, 255].into(),
                    }),
                }],
            },
        })),
    ));
    samples.push((
        crate::wire::CHAT_PREVIEWS,
        Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Turn {
                index: 0,
                turn: Box::new(Turn {
                    woke: None,
                    prompt: "[Image]".into(),
                    sent: 1,
                    items: Vec::new(),
                    end: None,
                    files: None,
                    from: None,
                    switch: None,
                    previews: vec![crate::ChatPreview {
                        original: 0,
                        width: 1,
                        height: 1,
                        rgba: vec![25, 50, 75, 255].into(),
                    }],
                }),
            })]),
        })),
    ));
    samples.push((
        crate::wire::TOOL_IMAGES,
        Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Item {
                turn: 0,
                item: 0,
                value: Item::Tool(Tool {
                    content: vec![ToolContent::Image(crate::ChatPreview {
                        original: 0,
                        width: 1,
                        height: 1,
                        rgba: vec![25, 50, 75, 255].into(),
                    })],
                    ..tool()
                }),
            })]),
        })),
    ));
    samples.push((
        crate::wire::CHAT_ORIGINALS,
        Client(ClientMessage::Request(Request::ChatOriginal {
            terminal,
            original: 42,
        })),
    ));
    samples.push((
        crate::wire::CHAT_ORIGINALS,
        Server(ServerMessage::Response(Response::Text("cG5n".into()))),
    ));
    samples.push((
        crate::wire::CHAT_HANDOFF,
        Client(ClientMessage::Request(Request::Chat {
            terminal,
            action: ChatAction::SendTo {
                agent: "codex".into(),
                model: Some("gpt-6-luna".into()),
                budget: 12000,
                text: "Continue here".into(),
                images: Vec::new(),
            },
        })),
    ));
    for action in [
        ChatAction::Submit {
            text: "next".into(),
            images: Vec::new(),
            target: Some(crate::ChatTarget {
                agent: "claude".into(),
                model: Some("sonnet".into()),
                budget: 4096,
            }),
            steer: true,
        },
        ChatAction::RemoveQueued { id: 8 },
        ChatAction::SendQueued {
            id: 9,
            steer: false,
        },
    ] {
        samples.push((
            crate::wire::CHAT_QUEUE,
            Client(ClientMessage::Request(Request::Chat { terminal, action })),
        ));
    }
    samples.push((
        crate::wire::CHAT_QUEUE,
        Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Delivery(
                crate::ChatDelivery {
                    paused: true,
                    sending: Some(9),
                    native_steering: true,
                    queued: vec![crate::QueuedMessage {
                        id: 9,
                        text: "next".into(),
                        images: 1,
                        target: None,
                    }],
                },
            ))]),
        })),
    ));
    samples.push((
        crate::wire::PROVIDER_USAGE,
        Client(ClientMessage::Request(Request::Chat {
            terminal,
            action: ChatAction::RefreshUsage { force: true },
        })),
    ));
    samples.push((
        crate::wire::PROVIDER_USAGE,
        Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::ProviderUsage(
                chat().provider_usage,
            ))]),
        })),
    ));
    samples.extend([
        (
            crate::wire::CHAT_HISTORY,
            Client(ClientMessage::Request(Request::AttachRecent {
                terminal,
                first: Some(12),
            })),
        ),
        (
            crate::wire::CHAT_HISTORY,
            Client(ClientMessage::Request(Request::ChatHistory {
                terminal,
                before: 12,
            })),
        ),
        (
            crate::wire::CHAT_HISTORY,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::History {
                    first: 11,
                    before: 12,
                    messages_before: 10,
                    turns: vec![turn()],
                })]),
            })),
        ),
        (
            crate::wire::CHAT_HISTORY,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Snapshot(View {
                    title: "recent".into(),
                    cwd: None,
                    root: Node::Chat(Box::new(ChatNode {
                        first_turn: 12,
                        first_message: 10,
                        ..chat()
                    })),
                }),
            })),
        ),
    ]);
    let activity = crate::Activity {
        id: "build-1".into(),
        owner: terminal,
        turn: 2,
        title: "Build".into(),
        command: "make".into(),
        cwd: "/src".into(),
        pane: Some(PaneId(8)),
        started: 10,
        ended: Some(20),
        state: crate::ActivityState::Exited(0),
        result: "done".into(),
    };
    samples.extend(
        [
            Client(ClientMessage::NativeGrant {
                chat: terminal,
                access: crate::NativeAccess::Control,
            }),
            Client(ClientMessage::NativeReply {
                id: 7,
                result: Ok(crate::NativeReply {
                    revision: 4,
                    description: "window".into(),
                    png: Some("abc".into()),
                }),
            }),
            Client(ClientMessage::Request(Request::HarnessCapabilities {
                terminal,
            })),
            Client(ClientMessage::Request(Request::CancelActivity {
                terminal,
                id: "build-1".into(),
            })),
            Server(ServerMessage::Event(Event::NativeRequest {
                id: 7,
                chat: terminal,
                action: crate::NativeAction::Click {
                    revision: 4,
                    x: 20,
                    y: 40,
                },
            })),
            Server(ServerMessage::Event(Event::NativeGrantAck {
                chat: terminal,
                access: crate::NativeAccess::Inspect,
                reason: "granted".into(),
            })),
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Activities(vec![
                    activity.clone(),
                ]))]),
            })),
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Snapshot(View {
                    title: "activities".into(),
                    cwd: None,
                    root: Node::Chat(Box::new(ChatNode {
                        activities: vec![activity],
                        ..chat()
                    })),
                }),
            })),
        ]
        .map(|sample| (crate::wire::HARNESS, sample)),
    );
    for action in [
        crate::NativeAction::Snapshot,
        crate::NativeAction::Type {
            revision: 4,
            text: "hello".into(),
        },
        crate::NativeAction::Key {
            revision: 4,
            key: "<Esc>".into(),
        },
    ] {
        samples.push((
            crate::wire::HARNESS,
            Server(ServerMessage::Event(Event::NativeRequest {
                id: 8,
                chat: terminal,
                action,
            })),
        ));
    }
    for state in [
        crate::ActivityState::Starting,
        crate::ActivityState::Running,
        crate::ActivityState::Cancelling,
        crate::ActivityState::Unknown,
        crate::ActivityState::Refused,
    ] {
        samples.push((
            crate::wire::HARNESS,
            Server(ServerMessage::Event(Event::View {
                terminal,
                update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Activities(vec![
                    crate::Activity {
                        id: "status".into(),
                        owner: terminal,
                        turn: 2,
                        title: "work".into(),
                        command: "true".into(),
                        cwd: "/src".into(),
                        pane: None,
                        started: 1,
                        ended: None,
                        state,
                        result: String::new(),
                    },
                ]))]),
            })),
        ));
    }
    samples.extend(
        undo_state()
            .into_iter()
            .map(|sample| (crate::wire::UNDO_STATE, sample)),
    );
    // An agent's past sessions on another machine (CLACKA-163).
    samples.push((
        crate::wire::ELSEWHERE,
        Client(ClientMessage::ListSessions {
            id: 4,
            agent: "claude".into(),
            cwd: "/src".into(),
            on: Some(crate::MachineId([9; 16])),
        }),
    ));
    // A shell another machine lends as a background tab (CLACKA-122).
    let mut lent = workspaces();
    lent.list[0].lent = Some(Lent {
        to: crate::MachineId([7; 16]),
        key: 0x0123_4567_89ab_cdef,
        placed: false,
        background: Some(crate::LentBackground {
            state: crate::BackgroundState::Ended(crate::TerminalStatus::Exited(2)),
            failed: Some(2),
        }),
    });
    samples.extend(
        [
            Client(ClientMessage::Request(Request::Lend {
                key: 0x0123_4567_89ab_cdef,
                beside: Some(PaneId(1)),
                terminal: PaneTerminal::New(NewTerminal {
                    size: Size::new(80, 24).expect("a size"),
                    command: None,
                    cwd: None,
                    record_to: None,
                }),
                command: Some("trail serve".into()),
            })),
            Server(ServerMessage::Event(Event::Workspaces(lent))),
            Client(ClientMessage::Request(Request::LentBackground {
                key: 0x0123_4567_89ab_cdef,
                action: crate::LentBackgroundAction::Bring,
            })),
            Client(ClientMessage::Request(Request::LentBackground {
                key: 0x0123_4567_89ab_cdef,
                action: crate::LentBackgroundAction::Seen,
            })),
        ]
        .map(|sample| (crate::wire::ELSEWHERE, sample)),
    );
    // A terminal's search for the match a count says (CLACKA-171).
    samples.extend(
        [
            Client(ClientMessage::Request(Request::Search {
                terminal: TerminalId(1),
                query: "q".into(),
                from: Position { line: 3, col: 4 },
                backward: false,
                exact: false,
                count: Some(3),
            })),
            Server(ServerMessage::Response(Response::Searched {
                found: Some(Match {
                    start: Position { line: 1, col: 2 },
                    cells: 1,
                }),
                wrapped: true,
            })),
        ]
        .map(|sample| (crate::wire::SEARCH_COUNT, sample)),
    );
    // The profile a chat an agent started was started as (CLACKA-179).
    let profiled = ChatSummary {
        profile: Some("explore".into()),
        ..summary()
    };
    samples.push((
        crate::wire::SUBAGENT_PROFILE,
        Server(ServerMessage::Event(Event::Chats(vec![profiled]))),
    ));
    samples.extend(
        agent_tasks()
            .into_iter()
            .map(|sample| (crate::wire::AGENT_TASKS, sample)),
    );
    // A turn an agent took by itself (CLACKA-180).
    samples.push((
        crate::wire::AGENT_TURNS,
        Sample::Server(ServerMessage::Event(Event::View {
            terminal: TerminalId(3),
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Turn {
                index: 1,
                turn: Box::new(Turn {
                    prompt: String::new(),
                    woke: Some("Background command \"npm test\" completed".into()),
                    ..turn()
                }),
            })]),
        })),
    ));
    samples.extend(
        agent_plan()
            .into_iter()
            .map(|sample| (crate::wire::AGENT_PLAN, sample)),
    );
    samples.extend(
        elicitation()
            .into_iter()
            .map(|sample| (crate::wire::ELICITATION, sample)),
    );
    samples.extend(
        agent_commands()
            .into_iter()
            .map(|sample| (crate::wire::AGENT_COMMANDS, sample)),
    );
    samples.extend(
        chat_facts()
            .into_iter()
            .map(|sample| (crate::wire::CHAT_FACTS, sample)),
    );
    // Whether a chat's last turn went wrong, for its notification
    // (CLACKA-194).
    let failed = ChatSummary {
        failed: Some(true),
        ..summary()
    };
    samples.push((
        crate::wire::TURN_FAILED,
        Server(ServerMessage::Event(Event::Chats(vec![failed]))),
    ));
    samples.extend(
        nested_work()
            .into_iter()
            .map(|sample| (crate::wire::NESTED_WORK, sample)),
    );
    samples.extend(
        prompt_box()
            .into_iter()
            .map(|sample| (crate::wire::PROMPT_BOX, sample)),
    );
    samples.extend(
        chat_branches()
            .into_iter()
            .map(|sample| (crate::wire::CHAT_BRANCHES, sample)),
    );
    samples.push((
        crate::wire::CHAT_PATHS,
        Server(ServerMessage::Event(Event::View {
            terminal: TerminalId(3),
            update: ViewUpdate::Snapshot(View {
                title: "paths".into(),
                cwd: None,
                root: Node::Chat(Box::new(ChatNode {
                    paths: Some(crate::ChatPaths {
                        cwd: "/home/j/p/src".into(),
                        project: "/home/j/p".into(),
                    }),
                    ..chat()
                })),
            }),
        })),
    ));
    // Where a background task's output goes (CLACKA-199).
    let mut task = agent_task(crate::AgentTaskKind::Shell, crate::AgentTaskState::Running);
    task.output = Some("/tmp/claude-1000/-home-jack-app/0d9c/tasks/bh1jmizrr.output".into());
    samples.push((
        crate::wire::TASK_OUTPUT,
        Server(ServerMessage::Event(Event::View {
            terminal: TerminalId(3),
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Tasks(vec![task]))]),
        })),
    ));
    samples
}

/// Samples of protocol 118: the chat's prompt box, the user's messages
/// and a draft in a snapshot, a message sent emptying the draft, and a
/// window keeping a draft and taking a queued message back (CLACKA-192).
fn prompt_box() -> Vec<Sample> {
    let terminal = TerminalId(3);
    vec![
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Snapshot(View {
                title: "prompts".into(),
                cwd: None,
                root: Node::Chat(Box::new(ChatNode {
                    prompts: vec!["fix the tests".into(), "now the docs\nand NEWS".into()],
                    draft: Some("and then ".into()),
                    ..chat()
                })),
            }),
        })),
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::DraftCleared {
                sent: "fix the lint".into(),
            })]),
        })),
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::DraftCleared {
                sent: String::new(),
            })]),
        })),
        Sample::Client(ClientMessage::Request(Request::Chat {
            terminal,
            action: ChatAction::Draft {
                text: "half a thought".into(),
            },
        })),
        Sample::Client(ClientMessage::Request(Request::Chat {
            terminal,
            action: ChatAction::TakeBack { queued: 9 },
        })),
    ]
}

/// Samples of protocol 121: where a chat's conversation's branches part,
/// and rewinding it and switching between them (CLACKA-198).
fn chat_branches() -> Vec<Sample> {
    let terminal = TerminalId(3);
    let place = crate::Divergence {
        at: 4,
        current: 2,
        count: 3,
        dropped: 1,
        branches: vec![
            crate::BranchSummary {
                first: "use the old parser".into(),
                turns: 6,
                used: 1_760_000_000_000,
                summary: false,
            },
            crate::BranchSummary {
                first: "use the new parser".into(),
                turns: 2,
                used: 1_760_000_100_000,
                summary: true,
            },
            crate::BranchSummary::default(),
        ],
    };
    let mut samples = vec![Sample::Server(ServerMessage::Event(Event::View {
        terminal,
        update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Branches(vec![place]))]),
    }))];
    for files in [
        crate::ForkFiles::Keep,
        crate::ForkFiles::Undo,
        crate::ForkFiles::UndoForce,
    ] {
        samples.push(Sample::Client(ClientMessage::Request(Request::Chat {
            terminal,
            action: ChatAction::Rewind {
                at: 4,
                files,
                edit: files == crate::ForkFiles::Keep,
                stop_tasks: files == crate::ForkFiles::UndoForce,
            },
        })));
    }
    samples.push(Sample::Client(ClientMessage::Request(Request::Chat {
        terminal,
        action: ChatAction::SwitchBranch {
            at: 4,
            to: 1,
            files: crate::ForkFiles::Undo,
            stop_tasks: true,
        },
    })));
    samples
}

/// Samples of protocol 117: what a Task subagent did, under the tool
/// call that started it: its words, its thinking and a tool, in a
/// snapshot, a turn and an item patch (CLACKA-195).
fn nested_work() -> Vec<Sample> {
    let terminal = TerminalId(3);
    let task = Tool {
        id: "toolu_task".into(),
        title: "Find the parser".into(),
        kind: ToolKind::Think,
        status: ToolStatus::Running,
        content: vec![ToolContent::Text("Find where lines are parsed.".into())],
        locations: Vec::new(),
        exit: None,
    };
    let nested = |item| Item::nested("toolu_task".into(), item);
    let items = vec![
        Item::Tool(task),
        nested(Item::Words("I'll search.".into())),
        nested(Item::Thought("grep first".into())),
        Item::Nested {
            parent: "toolu_task".into(),
            item: Box::new(Item::Tool(tool())),
        },
    ];
    let turn = Turn {
        items: items.clone(),
        end: None,
        ..turn()
    };
    vec![
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![
                Patch::Chat(ChatPatch::Turn {
                    index: 0,
                    turn: Box::new(turn.clone()),
                }),
                Patch::Chat(ChatPatch::Item {
                    turn: 0,
                    item: 3,
                    value: items[3].clone(),
                }),
                // A question about the subagent's tool, which names it.
                Patch::Chat(ChatPatch::Question(Some(Question {
                    tool: Some("toolu_read".into()),
                    ..question()
                }))),
            ]),
        })),
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Snapshot(View {
                title: "nested".into(),
                cwd: None,
                root: Node::Chat(Box::new(ChatNode {
                    turns: vec![turn],
                    ..chat()
                })),
            }),
        })),
    ]
}

/// Samples of protocol 114: the agent's slash commands, one with a hint
/// and one without, in a snapshot and a patch, and a patch that empties
/// them (CLACKA-189).
fn agent_commands() -> Vec<Sample> {
    use crate::AgentCommand;
    let terminal = TerminalId(3);
    let commands = vec![
        AgentCommand {
            name: "compact".into(),
            description: "Summarise the conversation so far".into(),
            hint: None,
        },
        AgentCommand {
            name: "review-branch".into(),
            description: "Review the changes against a branch".into(),
            hint: Some("branch name".into()),
        },
    ];
    vec![
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Commands(commands.clone()))]),
        })),
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Commands(Vec::new()))]),
        })),
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Snapshot(View {
                title: "commands".into(),
                cwd: None,
                root: Node::Chat(Box::new(ChatNode { commands, ..chat() })),
            }),
        })),
    ]
}

/// Samples of protocol 115: what an agent says of a turn beyond its words
/// (CLACKA-196). A chat trying again, with a line where it compacted its
/// context, a command that failed with its exit code, a failure in plain
/// words with the agent's own, what the turn spent and what the session
/// cost, and an answer of a kind the daemon doesn't know; that in patches;
/// and sending a failed message again.
fn chat_facts() -> Vec<Sample> {
    use crate::{Cost, Spent};
    let terminal = TerminalId(3);
    let cost = Cost::of(0.0421, "USD");
    let spent = Spent {
        input: 12_000,
        output: 800,
        cached: 9_000,
        cost: Some(Cost {
            micros: 31_000,
            currency: *b"USD",
        }),
    };
    let turn = Turn {
        items: vec![
            Item::Compacted("automatic · 182k → 31k tokens".into()),
            Item::Tool(Tool {
                id: "t2".into(),
                title: "cargo test".into(),
                status: ToolStatus::Failed,
                kind: ToolKind::Execute,
                content: vec![ToolContent::Output("failed\n".into())],
                locations: Vec::new(),
                exit: Some(101),
            }),
        ],
        end: Some(TurnEnd {
            at: 1_760_000_090,
            outcome: Outcome::Failed("Claude is overloaded: send it again in a moment".into()),
            raw: Some("Internal error: API Error: 529 overloaded".into()),
            spent: Some(spent),
        }),
        ..turn()
    };
    let question = Question {
        choices: vec![
            Choice {
                id: "allow".into(),
                name: "Allow".into(),
                kind: ChoiceKind::AllowOnce,
            },
            Choice {
                id: "allow_session".into(),
                name: "Allow for this session".into(),
                kind: ChoiceKind::Other,
            },
        ],
        ..question()
    };
    vec![
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Snapshot(View {
                title: "facts".into(),
                cwd: None,
                root: Node::Chat(Box::new(ChatNode {
                    turns: vec![turn.clone()],
                    question: Some(question.clone()),
                    usage: Some(Usage { cost, ..usage() }),
                    retrying: Some("Retrying Claude, attempt 2 of 10.".into()),
                    ..chat()
                })),
            }),
        })),
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![
                Patch::Chat(ChatPatch::Retrying(Some("Reconnecting... 1/5".into()))),
                Patch::Chat(ChatPatch::Item {
                    turn: 0,
                    item: 0,
                    value: Item::Compacted(String::new()),
                }),
                Patch::Chat(ChatPatch::Turn {
                    index: 0,
                    turn: Box::new(turn),
                }),
                Patch::Chat(ChatPatch::Question(Some(question))),
                Patch::Chat(ChatPatch::Retrying(None)),
            ]),
        })),
        Sample::Client(ClientMessage::Request(Request::Chat {
            terminal,
            action: ChatAction::Resend,
        })),
    ]
}

/// Samples of protocol 112: the agent's plan, with a step in every state,
/// in a snapshot and a patch, and a patch that empties it (CLACKA-190).
fn agent_plan() -> Vec<Sample> {
    use crate::{PlanState, PlanStep};
    let terminal = TerminalId(3);
    let plan: Vec<PlanStep> = [
        ("Read the code", PlanState::Done),
        ("Write the tests", PlanState::Doing),
        ("Make them pass", PlanState::ToDo),
    ]
    .into_iter()
    .map(|(content, state)| PlanStep {
        content: content.into(),
        state,
    })
    .collect();
    vec![
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Plan(plan.clone()))]),
        })),
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Plan(Vec::new()))]),
        })),
        Sample::Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Snapshot(View {
                title: "plan".into(),
                cwd: None,
                root: Node::Chat(Box::new(ChatNode { plan, ..chat() })),
            }),
        })),
    ]
}

/// Samples of protocol 113: forms an agent asks to be filled in, with a
/// field of every kind, and a link to open; a plan to approve; and every
/// kind of answer (CLACKA-191).
#[allow(clippy::too_many_lines, reason = "one sample of each kind of field")]
fn elicitation() -> Vec<Sample> {
    use crate::{Field, FieldInput, FieldOption, FieldValue, Form, FormAsk, FormReply, Question};
    use Sample::{Client, Server};
    let terminal = TerminalId(3);
    let option = |value: &str, description: Option<&str>| FieldOption {
        value: value.into(),
        title: value.to_uppercase(),
        description: description.map(Into::into),
    };
    let field = |key: &str, required, input| Field {
        key: key.into(),
        label: key.into(),
        title: Some(key.to_uppercase()),
        description: None,
        required,
        input,
    };
    let fields = vec![
        field(
            "db",
            true,
            FieldInput::One {
                options: vec![option("pg", Some("Server")), option("lite", None)],
                default: Some("pg".into()),
            },
        ),
        field(
            "note",
            false,
            FieldInput::Text {
                default: Some("none".into()),
                min: Some(1),
                max: Some(200),
                secret: true,
                format: Some("email".into()),
            },
        ),
        field(
            "count",
            false,
            FieldInput::Number {
                integer: true,
                min: Some("1".into()),
                max: Some("9".into()),
                default: Some("3".into()),
            },
        ),
        field(
            "go",
            false,
            FieldInput::Toggle {
                default: Some(true),
            },
        ),
        field(
            "also",
            false,
            FieldInput::Many {
                options: vec![option("tests", None), option("docs", Some("Learn"))],
                min: Some(0),
                max: Some(2),
                default: vec!["tests".into()],
            },
        ),
    ];
    let decline = Choice {
        id: "decline".into(),
        name: "Decline".into(),
        kind: ChoiceKind::RejectOnce,
    };
    let question = |id, form| Question {
        id,
        title: "ask you something this window is too old to answer: `Which?`".into(),
        choices: vec![decline.clone()],
        own: false,
        form: Some(form),
        plan: None,
        tool: None,
    };
    let asked = |question| {
        Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Question(Some(question)))]),
        }))
    };
    let fill = |question, reply| {
        Client(ClientMessage::Request(Request::Chat {
            terminal,
            action: ChatAction::Fill { question, reply },
        }))
    };
    vec![
        asked(question(
            7,
            Form {
                message: "Which database?".into(),
                ask: FormAsk::Fields {
                    fields,
                    complete: true,
                },
            },
        )),
        asked(question(
            8,
            Form {
                message: "Authenticate with MCP server linear".into(),
                ask: FormAsk::Url("https://linear.app/oauth/authorize?state=x".into()),
            },
        )),
        asked(Question {
            id: 9,
            title: "Approve Plan".into(),
            choices: vec![decline],
            own: false,
            form: None,
            plan: Some("## Plan\n\n1. Write the tests.".into()),
            tool: None,
        }),
        fill(
            7,
            FormReply::Accept(vec![
                ("db".into(), FieldValue::Text("pg".into())),
                ("count".into(), FieldValue::Number("4".into())),
                ("go".into(), FieldValue::Toggle(false)),
                ("also".into(), FieldValue::Many(vec!["docs".into()])),
            ]),
        ),
        fill(8, FormReply::Decline),
        fill(8, FormReply::Cancel),
    ]
}

/// Samples of protocol 110: what an agent runs in the background of a
/// chat, of every kind and state, in a snapshot, a patch and a subagent's
/// card, and asking to stop one (CLACKA-177).
fn agent_tasks() -> Vec<Sample> {
    use Sample::{Client, Server};

    use crate::{AgentTaskKind, AgentTaskState};
    let terminal = TerminalId(3);
    let kinds = [
        AgentTaskKind::Shell,
        AgentTaskKind::Monitor,
        AgentTaskKind::Workflow,
        AgentTaskKind::Turn,
        AgentTaskKind::Other,
    ];
    let states = [
        AgentTaskState::Running,
        AgentTaskState::Paused,
        AgentTaskState::Stopping,
        AgentTaskState::Done,
        AgentTaskState::Failed,
        AgentTaskState::Stopped,
        AgentTaskState::Lost,
    ];
    let mut tasks: Vec<crate::AgentTask> = (kinds.iter())
        .map(|&kind| agent_task(kind, AgentTaskState::Running))
        .chain(
            states
                .iter()
                .map(|&state| agent_task(AgentTaskKind::Shell, state)),
        )
        .collect();
    tasks[0].command = None;
    tasks[0].summary = None;
    tasks[1].ended = Some(1_760_000_009);
    tasks[2].can_stop = false;
    let mut card = subagent();
    card.tasks = vec![agent_task(AgentTaskKind::Shell, AgentTaskState::Running)];
    vec![
        Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Tasks(tasks.clone()))]),
        })),
        Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Subagents(vec![card]))]),
        })),
        Server(ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Snapshot(View {
                title: "tasks".into(),
                cwd: None,
                root: Node::Chat(Box::new(ChatNode { tasks, ..chat() })),
            }),
        })),
        Client(ClientMessage::Request(Request::Chat {
            terminal,
            action: ChatAction::StopTask {
                id: "bh1jmizrr".into(),
            },
        })),
    ]
}

/// Samples of protocol 106: undo steps that keep where the cursor was and
/// the last selection, of every kind; one that changes nothing; and an
/// undo and a redo that give them back.
fn undo_state() -> Vec<Sample> {
    use Sample::{Client, Server};
    use clackatoo_text::{StepPlace, StepSelection, StepState, StepVisual};
    let buffer = BufferId(3);
    let visual = |kind| StepVisual {
        start: StepPlace { line: 4, col: 1 },
        end: StepPlace { line: 9, col: 3 },
        kind,
        backward: true,
        to_end: false,
    };
    let state = |kind| StepState {
        cursor: StepPlace { line: 4, col: 2 },
        taken: true,
        visual: Some(visual(kind)),
    };
    vec![
        Client(ClientMessage::Edit {
            buffer,
            seq: 12,
            change: Change::Step {
                edit: Edit::replace(13, 3, "main", "start"),
                base: 4,
                state: state(StepSelection::Characters),
            },
        }),
        Client(ClientMessage::Edit {
            buffer,
            seq: 13,
            change: Change::Empty {
                base: 5,
                state: StepState {
                    cursor: StepPlace { line: 0, col: 0 },
                    taken: false,
                    visual: None,
                },
            },
        }),
        Client(ClientMessage::Edit {
            buffer,
            seq: 14,
            change: Change::Reverse {
                redo: false,
                base: 5,
                visual: Some(visual(StepSelection::Lines)),
            },
        }),
        Client(ClientMessage::Edit {
            buffer,
            seq: 15,
            change: Change::Reverse {
                redo: true,
                base: 6,
                visual: None,
            },
        }),
        Server(ServerMessage::Event(Event::Buffer {
            buffer,
            update: BufferUpdate::Reversed {
                version: 6,
                seq: 14,
                edit: Edit::replace(14, 3, "start", "main"),
                state: Some(state(StepSelection::Block)),
            },
        })),
        Server(ServerMessage::Event(Event::Buffer {
            buffer,
            update: BufferUpdate::Reversed {
                version: 6,
                seq: 15,
                edit: Edit::keep(13),
                state: None,
            },
        })),
    ]
}

/// Samples of protocol 96: a workspace's background tabs, one running, one
/// whose program ended by a signal, and one whose shell is idle after a
/// command failed; and what's asked of them: a tab and a pane sent to the
/// background, one brought back, one dismissed, their failures seen, a
/// command started in a new one, and the ports they listen on, found.
fn background() -> Vec<Sample> {
    use Sample::{Client, Server};
    let tab = |id, pane, terminal| Tab {
        id: TabId(id),
        layout: Layout::Pane(Pane {
            id: PaneId(pane),
            content: Content::Terminal(TerminalId(terminal)),
            home: None,
            key: None,
        }),
        focused: PaneId(pane),
        name: Some("server".into()),
    };
    let mut workspaces = workspaces();
    workspaces.list[0].background = vec![
        crate::Background {
            tab: tab(8, 9, 10),
            state: crate::BackgroundState::Running,
            failed: None,
            name: "trail serve".into(),
        },
        crate::Background {
            tab: tab(11, 12, 13),
            state: crate::BackgroundState::Ended(TerminalStatus::Killed(9)),
            failed: Some(137),
            name: "~/src/site".into(),
        },
        crate::Background {
            tab: tab(14, 15, 16),
            state: crate::BackgroundState::Idle,
            failed: Some(2),
            name: "cargo build".into(),
        },
    ];
    let asked = |action| Client(ClientMessage::Request(Request::Background(action)));
    vec![
        Server(ServerMessage::Event(Event::Workspaces(workspaces))),
        asked(crate::BackgroundAction::Tab { tab: TabId(2) }),
        asked(crate::BackgroundAction::Pane { pane: PaneId(1) }),
        asked(crate::BackgroundAction::Bring { tab: TabId(8) }),
        asked(crate::BackgroundAction::Dismiss { tab: TabId(11) }),
        asked(crate::BackgroundAction::Seen {
            workspace: WorkspaceId(1),
        }),
        asked(crate::BackgroundAction::Start {
            workspace: WorkspaceId(1),
            command: "trail serve".into(),
            cwd: Some("/home/jack/site".into()),
            pane: Some(PaneId(1)),
            size: size(),
        }),
        Server(ServerMessage::Response(Response::PaneCreated(PaneId(17)))),
        asked(crate::BackgroundAction::Ports),
        Server(ServerMessage::Event(Event::Listening(vec![
            crate::Listening {
                tab: TabId(8),
                ports: vec![
                    crate::ListeningPort {
                        port: 8080,
                        program: "trail".into(),
                    },
                    crate::ListeningPort {
                        port: 35729,
                        program: "trail".into(),
                    },
                ],
            },
            crate::Listening {
                tab: TabId(14),
                ports: Vec::new(),
            },
        ]))),
    ]
}

/// Samples of protocol 96 too: terminals marked to keep running, each
/// way, and the mark taken away; what the daemon says it keeps running;
/// and the marks, one given up on.
fn kept() -> Vec<Sample> {
    use Sample::{Client, Server};
    vec![
        Client(ClientMessage::Request(Request::Keep {
            pane: PaneId(9),
            how: crate::Keeping::Restore,
        })),
        Client(ClientMessage::Request(Request::Keep {
            pane: PaneId(15),
            how: crate::Keeping::Off,
        })),
        Client(ClientMessage::Request(Request::Keep {
            pane: PaneId(12),
            how: crate::Keeping::Always,
        })),
        Server(ServerMessage::Response(Response::Text(
            "pane 12 runs `trail serve` again as it ends".into(),
        ))),
        Server(ServerMessage::Event(Event::Workspaces(Workspaces {
            kept: vec![
                crate::Kept {
                    pane: PaneId(9),
                    command: "trail serve".into(),
                    always: false,
                    given_up: false,
                    paused: true,
                },
                crate::Kept {
                    pane: PaneId(12),
                    command: "cargo watch -x run".into(),
                    always: true,
                    given_up: true,
                    paused: false,
                },
            ],
            ..self::workspaces()
        }))),
    ]
}

/// Samples of protocol 95: a daemon's workspaces saying which machines it
/// passes on, one straight there, vouched for, and one through yet
/// another, not connected now; a daemon asking for a stream to carry its
/// connection on to one of them, the answer, and the certificate signed
/// that comes through it; forgetting a machine's key; a connection asked
/// not to be passed on; and the connections, one to a machine passed on,
/// going through the daemon that passes it on, and one not passed on.
fn chain() -> Vec<Sample> {
    use Sample::{Client, Server};
    let server = MachineId([0x5e; 16]);
    let nas = MachineId([0x0a; 16]);
    let mut workspaces = workspaces();
    workspaces.passed = vec![
        crate::Passed {
            id: server,
            name: "server".into(),
            run: 0x5e5e,
            through: Vec::new(),
            reached: true,
            key: Some([0x4b; 32]),
        },
        crate::Passed {
            id: nas,
            name: "nas".into(),
            run: 0x0a0a,
            through: vec![server],
            reached: false,
            key: None,
        },
    ];
    vec![
        Server(ServerMessage::Event(Event::Workspaces(workspaces))),
        Client(ClientMessage::Request(Request::Reach { machine: server })),
        Server(ServerMessage::Response(Response::Done)),
        Server(ServerMessage::Response(Response::Carrying {
            certificate: vec![1, 2, 3],
            udp: None,
            signed: Some(crate::Signed {
                machine: server,
                key: [0x4b; 32],
                signature: vec![0x51; 64],
            }),
        })),
        Client(ClientMessage::Request(Request::Forget {
            host: "server".into(),
        })),
        Client(ClientMessage::Request(Request::Connect {
            host: "shared.example.org".into(),
            command: None,
            share: Some(false),
        })),
        Server(ServerMessage::Event(Event::Connections(vec![
            Connection {
                host: "server".into(),
                machine: Some(Machine {
                    id: server,
                    name: "jack-server".into(),
                    run: 0x5e5e,
                }),
                state: ConnectionState::Connected { wire: 95 },
                through: vec!["desk".into()],
                unshared: false,
            },
            Connection {
                host: "shared.example.org".into(),
                machine: None,
                state: ConnectionState::Connecting,
                through: Vec::new(),
                unshared: true,
            },
        ]))),
    ]
}

/// Samples of protocol 94: a daemon putting a pane of its machine's in a
/// layout of the daemon it's connected to; that daemon's workspaces,
/// holding it, with its key, its home and what that machine is called;
/// and the workspace holding it on its own machine, lent and placed, as
/// that machine's daemon shows another.
fn mixed() -> Vec<Sample> {
    use Sample::{Client, Server};
    let desktop = MachineId([7; 16]);
    let laptop = MachineId([0xa1; 16]);
    let mut workspaces = workspaces();
    if let Layout::Split { parts, .. } = &mut workspaces.list[0].tabs[0].layout
        && let Layout::Pane(pane) = &mut parts[0]
    {
        pane.home = Some(laptop);
        pane.key = Some(0x0123_4567_89ab_cdef);
    }
    workspaces.others = vec![crate::OtherMachine {
        id: laptop,
        name: "jack-ubuntu-laptop".into(),
    }];
    let mut lent = self::workspaces();
    lent.list[0].lent = Some(Lent {
        to: desktop,
        key: 0x0123_4567_89ab_cdef,
        placed: true,
        background: None,
    });
    vec![
        Client(ClientMessage::Request(Request::Split {
            pane: PaneId(1),
            side: Side::Below,
            terminal: PaneTerminal::Lent {
                key: 0x0123_4567_89ab_cdef,
                kind: PaneKind::new("terminal"),
            },
            on: None,
        })),
        Server(ServerMessage::Event(Event::Workspaces(workspaces))),
        Server(ServerMessage::Event(Event::Workspaces(lent))),
    ]
}

/// Samples of protocol 93: new panes and a workspace asked for on a
/// machine named; a daemon saying which machine it is as it relays, and
/// asking for a pane to be lent it; the workspaces it's shown, one of
/// them holding the pane lent it, beside one of its own holding a pane
/// from the lending machine; giving one back, listing them, and what the
/// bar is told of them.
fn lend() -> Vec<Sample> {
    use Sample::{Client, Server};
    let desktop = MachineId([7; 16]);
    let laptop = MachineId([0xa1; 16]);
    let new = || {
        PaneTerminal::New(NewTerminal {
            size: size(),
            command: None,
            cwd: None,
            record_to: None,
        })
    };
    let mut workspaces = workspaces();
    workspaces.list[0].lent = Some(Lent {
        to: laptop,
        key: 0x0123_4567_89ab_cdef,
        placed: false,
        background: None,
    });
    let mut own = workspaces.list[0].clone();
    own.id = WorkspaceId(9);
    own.lent = None;
    if let Layout::Split { parts, .. } = &mut own.tabs[0].layout
        && let Layout::Pane(pane) = &mut parts[0]
    {
        pane.home = Some(desktop);
    }
    workspaces.list.push(own);
    vec![
        Client(ClientMessage::Request(Request::NewWorkspace {
            name: Some("notes".into()),
            terminal: new(),
            on: Some(desktop),
        })),
        Client(ClientMessage::Request(Request::NewTab {
            workspace: WorkspaceId(1),
            terminal: new(),
            on: Some(desktop),
        })),
        Client(ClientMessage::Request(Request::Split {
            pane: PaneId(1),
            side: Side::Right,
            terminal: new(),
            on: Some(laptop),
        })),
        Client(ClientMessage::Request(Request::SplitTab {
            tab: TabId(2),
            side: Side::Left,
            terminal: new(),
            share: Some(250),
            on: Some(desktop),
        })),
        Client(ClientMessage::Request(Request::RelayAs {
            machine: Machine {
                id: laptop,
                name: "jack-ubuntu-laptop".into(),
                run: 0x5eed,
            },
        })),
        Client(ClientMessage::Request(Request::Lend {
            key: 0x0123_4567_89ab_cdef,
            beside: Some(PaneId(1)),
            terminal: new(),
            command: None,
        })),
        Server(ServerMessage::Response(Response::PaneCreated(PaneId(12)))),
        Server(ServerMessage::Event(Event::Workspaces(workspaces))),
        Client(ClientMessage::Request(Request::Unlend {
            workspace: WorkspaceId(1),
            close_idle: true,
        })),
        Client(ClientMessage::Request(Request::Lent)),
        Server(ServerMessage::Response(Response::Text(
            "1  a terminal in ~/src  lent to jack-ubuntu-laptop, connected".into(),
        ))),
        Server(ServerMessage::Event(Event::Said(
            "desk has lost 2 panes it lent this machine: they've closed".into(),
        ))),
    ]
}

/// Samples of protocol 92: a daemon saying it relays, a workspace from
/// another machine with panes of every kind, a file operation asked from
/// one of its panes, an offer to install a language there answered both
/// ways, and ports forwarded there, listed, stopped and told of.
fn relay() -> Vec<Sample> {
    use Sample::{Client, Server};
    let desktop = MachineId([7; 16]);
    let mut workspaces = workspaces();
    workspaces.list[0].home = Some(desktop);
    for tab in &mut workspaces.list[0].tabs {
        tab.layout
            .for_each_pane_mut(&mut |pane| pane.home = Some(desktop));
    }
    let buffer = Some(BufferId(3));
    let forwards = |forwarding| {
        Client(ClientMessage::Request(Request::Forwards {
            pane: PaneId(2),
            forwarding,
        }))
    };
    vec![
        Client(ClientMessage::Request(Request::Relay)),
        Server(ServerMessage::Event(Event::Workspaces(workspaces))),
        Client(ClientMessage::FileOp {
            id: 10,
            op: FileOp::Create {
                path: "/home/jack/notes".into(),
                dir: true,
            },
            pane: Some(PaneId(3)),
        }),
        Client(ClientMessage::Request(Request::Install {
            language: "rust".into(),
            grammar: true,
            server: Some("rust-analyzer".into()),
            buffer,
        })),
        Client(ClientMessage::Request(Request::Decline {
            language: "rust".into(),
            buffer,
        })),
        forwards(Forwarding::List),
        forwards(Forwarding::Add {
            local: 3000,
            host: "localhost".into(),
            port: 8080,
        }),
        forwards(Forwarding::Stop(Some(3000))),
        forwards(Forwarding::Stop(None)),
        Server(ServerMessage::Response(Response::Text(
            "Forwarding port 3000 here to localhost:8080 on desk".into(),
        ))),
        Server(ServerMessage::Event(Event::Forwarded(
            "nothing is listening at localhost:8080 on desk".into(),
        ))),
        Server(ServerMessage::Event(Event::Buffer {
            buffer: BufferId(3),
            update: BufferUpdate::Reattach,
        })),
    ]
}

/// Samples of protocol 91: a daemon saying which it is, workspaces and
/// panes from another, connecting to another daemon in every state, and
/// a question ssh asks, answered, turned down and taken back.
fn federation() -> Vec<Sample> {
    use Sample::{Client, Server};
    let desktop = MachineId([7; 16]);
    let laptop = Machine {
        id: MachineId([0xa1; 16]),
        name: "jack-ubuntu-laptop".into(),
        run: 0x1a2b_3c4d_5e6f_7081,
    };
    let mut workspaces = workspaces();
    workspaces.machine = Some(Machine {
        id: desktop,
        name: "jack-ubuntu-desktop".into(),
        run: 7,
    });
    workspaces.list[0].home = Some(laptop.id);
    if let Layout::Split { parts, .. } = &mut workspaces.list[0].tabs[0].layout
        && let Layout::Pane(pane) = &mut parts[0]
    {
        pane.home = Some(desktop);
    }
    let connection = |state| Connection {
        host: "jack@laptop:2222".into(),
        machine: Some(laptop.clone()),
        state,
        through: Vec::new(),
        unshared: false,
    };
    let reason = || "ssh: connect to host laptop port 2222: Connection refused".to_owned();
    vec![
        Server(ServerMessage::Event(Event::Workspaces(workspaces))),
        Client(ClientMessage::Request(Request::Connect {
            host: "jack@laptop:2222".into(),
            command: Some("~/.cargo/bin/clackatoo".into()),
            share: None,
        })),
        Server(ServerMessage::Response(Response::Done)),
        Client(ClientMessage::Request(Request::WatchConnections {
            answer: true,
        })),
        Server(ServerMessage::Response(Response::Connections(vec![
            connection(ConnectionState::Connecting),
            connection(ConnectionState::Asking),
        ]))),
        Server(ServerMessage::Event(Event::Connections(vec![
            connection(ConnectionState::NeedsPassword),
            connection(ConnectionState::Connected { wire: 90 }),
            connection(ConnectionState::Down { reason: reason() }),
            connection(ConnectionState::Stopped { reason: reason() }),
            connection(ConnectionState::Other),
        ]))),
        Server(ServerMessage::Event(Event::Login {
            ask: 3,
            host: "jack@laptop:2222".into(),
            question: Some("jack@laptop's password: ".into()),
        })),
        Client(ClientMessage::Login {
            ask: 3,
            answer: Some(Secret("hunter2".into())),
        }),
        Client(ClientMessage::Login {
            ask: 4,
            answer: None,
        }),
        Server(ServerMessage::Event(Event::Login {
            ask: 4,
            host: "jack@laptop:2222".into(),
            question: None,
        })),
        Client(ClientMessage::Request(Request::Disconnect {
            host: "jack@laptop:2222".into(),
        })),
    ]
}

/// Samples of protocol 82: a forwarded connection asked for, made, and
/// failed for each reason.
fn forwards() -> Vec<Sample> {
    use Sample::{Client, Server};
    let mut samples = vec![
        Client(ClientMessage::Request(Request::Forward(Forward {
            host: "localhost".into(),
            port: 8080,
            unpaced: false,
        }))),
        Server(ServerMessage::Response(Response::Done)),
    ];
    samples.extend(
        [
            ForwardFailure::Refused,
            ForwardFailure::NoSuchHost,
            ForwardFailure::TimedOut,
            ForwardFailure::Other("no route to host".into()),
        ]
        .map(|why| {
            Server(ServerMessage::Response(Response::Error(Error::Forward(
                why,
            ))))
        }),
    );
    samples
}

/// Samples of protocol 80: inlay hints asked for, and answered with one of
/// every kind, padded and not.
fn inlay_hints(buffer: BufferId) -> Vec<Sample> {
    use Sample::{Client, Server};
    let hint = |at, label: &str, kind, (pad_left, pad_right)| InlayHint {
        at,
        label: label.into(),
        kind,
        pad_left,
        pad_right,
    };
    vec![
        Server(ServerMessage::Event(Event::Buffer {
            buffer,
            update: BufferUpdate::Features(ServerFeatures {
                completion: None,
                signature: None,
                signature_again: Vec::new(),
                prepare_rename: true,
                uses: true,
                inlay_hints: true,
            }),
        })),
        Client(ClientMessage::Query {
            buffer,
            id: 13,
            version: 8,
            at: 0,
            query: Query::InlayHints { end: 400 },
        }),
        Server(ServerMessage::Event(Event::Buffer {
            buffer,
            update: BufferUpdate::Answer {
                id: 13,
                version: 8,
                answer: Answer::InlayHints {
                    start: 0,
                    end: 400,
                    hints: vec![
                        hint(9, ": Vec<u8>", InlayHintKind::Type, (false, false)),
                        hint(40, "value:", InlayHintKind::Parameter, (false, true)),
                        hint(77, "impl Fn", InlayHintKind::Other, (true, false)),
                    ],
                },
            },
        })),
    ]
}

/// Samples of protocol 79: the other uses of a name, of every kind, and
/// part of a file formatted.
fn uses(buffer: BufferId) -> Vec<Sample> {
    use Sample::{Client, Server};
    let query = |id, at, query| {
        Client(ClientMessage::Query {
            buffer,
            id,
            version: 7,
            at,
            query,
        })
    };
    let used = |start, kind| Use {
        start,
        end: start + 5,
        kind,
    };
    vec![
        Server(ServerMessage::Event(Event::Buffer {
            buffer,
            update: BufferUpdate::Features(ServerFeatures {
                completion: None,
                signature: None,
                signature_again: Vec::new(),
                prepare_rename: false,
                uses: true,
                // Not on the wire before protocol 80.
                inlay_hints: false,
            }),
        })),
        query(11, 4, Query::Uses),
        Server(ServerMessage::Event(Event::Buffer {
            buffer,
            update: BufferUpdate::Answer {
                id: 11,
                version: 7,
                answer: Answer::Uses(vec![
                    used(2, UseKind::Text),
                    used(30, UseKind::Read),
                    used(61, UseKind::Write),
                ]),
            },
        })),
        query(
            12,
            10,
            Query::FormatRange {
                end: 48,
                tab_size: 4,
                spaces: true,
            },
        ),
    ]
}

/// Samples of protocol 77: a file's symbols, and a project's, found; and a
/// rename checked before the name's asked for.
fn symbols(buffer: BufferId) -> Vec<Sample> {
    use Sample::{Client, Server};
    let query = |id, query| {
        Client(ClientMessage::Query {
            buffer,
            id,
            version: 6,
            at: 2,
            query,
        })
    };
    let answer = |id, answer| {
        Server(ServerMessage::Event(Event::Buffer {
            buffer,
            update: BufferUpdate::Answer {
                id,
                version: 6,
                answer,
            },
        }))
    };
    let symbol = Symbol {
        name: "start".into(),
        kind: 6,
        depth: 1,
        container: "Engine".into(),
        line: 3,
        col: 7,
        offset: Some(40),
    };
    vec![
        Server(ServerMessage::Event(Event::Buffer {
            buffer,
            update: BufferUpdate::Features(ServerFeatures {
                completion: Some(vec!['.']),
                signature: Some(vec!['(']),
                signature_again: vec![','],
                prepare_rename: true,
                // Not on the wire before protocol 79.
                uses: false,
                // Nor this before protocol 80.
                inlay_hints: false,
            }),
        })),
        query(8, Query::Symbols),
        answer(
            8,
            Answer::Symbols {
                symbols: vec![symbol.clone()],
                more: true,
            },
        ),
        query(9, Query::PrepareRename),
        answer(
            9,
            Answer::Renamable {
                name: Some("start".into()),
            },
        ),
        Client(ClientMessage::Find {
            id: 10,
            pane: Some(PaneId(2)),
            search: Search::Symbols {
                query: "sta".into(),
            },
        }),
        Server(ServerMessage::Event(Event::Found {
            id: 10,
            found: Found::Symbols(vec![FoundSymbol {
                path: "src/engine.rs".into(),
                symbol: Symbol {
                    offset: None,
                    ..symbol
                },
            }]),
        })),
    ]
}

/// More samples of protocol 72: the rest of what a window and the daemon
/// say, and the last variant of every enum the samples reach, so a variant
/// appended to any of them would show.
#[allow(clippy::too_many_lines, reason = "a list of samples")]
fn more() -> Vec<Sample> {
    use Sample::{Client, Server};
    let terminal = TerminalId(7);
    let buffer = BufferId(3);
    let files = FilesId(4);
    let board = BoardId(5);
    let review = ReviewId(6);
    let request = |request| Client(ClientMessage::Request(request));
    let response = |response| Server(ServerMessage::Response(response));
    let event = |event| Server(ServerMessage::Event(event));
    let buffer_update = |update| event(Event::Buffer { buffer, update });
    let new_pane = |terminal| {
        request(Request::Split {
            pane: PaneId(1),
            side: Side::Above,
            terminal,
            on: None,
        })
    };
    let mut samples = vec![
        // The last of each top-level enum.
        Client(ClientMessage::ListSessions {
            id: 3,
            agent: "claude".into(),
            cwd: "/src".into(),
            on: None,
        }),
        Client(ClientMessage::Upgrade {
            protocol_version: 72,
            executable: b"/bin/clackatoo".to_vec(),
        }),
        Server(ServerMessage::NotUpgrading {
            reason: "no".into(),
        }),
        response(Response::Error(Error::SessionOpen(terminal))),
        response(Response::Error(Error::RecordingFailed {
            path: "/x".into(),
            reason: "no".into(),
        })),
        response(Response::TerminalCreated(terminal)),
        response(Response::Terminals(vec![TerminalInfo {
            id: terminal,
            pid: None,
            size: size(),
            title: String::new(),
            status: TerminalStatus::Killed(9),
            cwd: None,
            command: None,
        }])),
        response(Response::View(View {
            title: "t".into(),
            cwd: Some("/".into()),
            root: Node::Grid(GridNode {
                size: size(),
                state: grid_state(),
                lines: vec![line("x")],
                links: links(),
                marks: BlockMarks::default(),
            }),
        })),
        response(Response::Text("text".into())),
        request(Request::Search {
            terminal,
            query: "q".into(),
            from: Position { line: 3, col: 4 },
            backward: true,
            exact: true,
            count: None,
        }),
        response(Response::Found(Some(Match {
            start: Position { line: 3, col: 4 },
            cells: 2,
        }))),
        request(Request::Block {
            terminal,
            line: 3,
            which: Which::After,
        }),
        response(Response::Block(Some(Block {
            first: 1,
            last: 4,
            output: Some(2),
            command: Some("ls".into()),
            cwd: Some("/".into()),
            running: true,
            exit: Some(1),
        }))),
        request(Request::CarryQuic),
        response(Response::Carrying {
            certificate: vec![1, 2, 3],
            udp: Some(Udp {
                port: 60000,
                probe_key: [7; 16],
            }),
            signed: None,
        }),
        response(Response::Reviewed("kept".into())),
        // Workspaces: what a new pane can show, every kind.
        new_pane(PaneTerminal::Existing(terminal)),
        new_pane(PaneTerminal::File(OpenFile {
            path: "a.rs".into(),
            cwd: Some("/src".into()),
        })),
        new_pane(PaneTerminal::Files(OpenFiles {
            dir: Some("/src".into()),
        })),
        new_pane(PaneTerminal::Board(OpenBoard::Named("plan".into()))),
        new_pane(PaneTerminal::Board(OpenBoard::Id(board))),
        new_pane(PaneTerminal::Review(NewReview {
            chat: Some(terminal),
            scope: ReviewScope::Turn(Some(2)),
            cwd: Some("/src".into()),
        })),
        new_pane(PaneTerminal::Review(NewReview {
            chat: None,
            scope: ReviewScope::Base,
            cwd: None,
        })),
        new_pane(PaneTerminal::Browser(terminal)),
        // Chats: what's asked of them, and choosing one.
        request(Request::Chat {
            terminal,
            action: ChatAction::Set {
                setting: "model".into(),
                value: "haiku".into(),
            },
        }),
        request(Request::Chat {
            terminal,
            action: ChatAction::Switch {
                agent: "codex".into(),
                model: Some("gpt".into()),
                budget: 4000,
            },
        }),
        request(Request::Chat {
            terminal,
            action: ChatAction::Open {
                chat: TerminalId(8),
            },
        }),
        request(Request::ChatChoices {
            pane: Some(PaneId(1)),
        }),
        response(Response::ChatChoices(ChatChoices {
            agents: vec![Provider {
                agent: "codex".into(),
                title: "Codex".into(),
                models: ProviderModels::Failed("no".into()),
            }],
            default: "claude".into(),
            cwd: "/src".into(),
            home: "/home/jack".into(),
            unshown: vec![summary()],
        })),
        request(Request::WatchChats),
        response(Response::Chats(vec![summary()])),
        event(Event::Chats(vec![summary()])),
        event(Event::Sessions {
            id: 3,
            sessions: Err("can't list".into()),
        }),
        event(Event::View {
            terminal,
            update: ViewUpdate::Snapshot(View {
                title: "Codex".into(),
                cwd: None,
                root: Node::Chat(Box::new(ChatNode {
                    state: ChatState::Starting,
                    settings: vec![Setting {
                        kind: SettingKind::Browser,
                        ..setting()
                    }],
                    turns: vec![Turn {
                        items: vec![Item::Tool(Tool {
                            status: ToolStatus::Failed,
                            kind: ToolKind::Other,
                            ..tool()
                        })],
                        end: Some(TurnEnd {
                            at: 1,
                            outcome: Outcome::Done,
                            raw: None,
                            spent: None,
                        }),
                        files: Some(TurnFiles {
                            count: 1,
                            state: TurnFilesState::TooOld,
                        }),
                        ..turn()
                    }],
                    question: Some(Question {
                        choices: vec![Choice {
                            id: "never".into(),
                            name: "Never".into(),
                            kind: ChoiceKind::RejectAlways,
                        }],
                        ..question()
                    }),
                    subagents: vec![Subagent {
                        status: SubagentStatus::Closed,
                        ..subagent()
                    }],
                    ..chat()
                })),
            }),
        }),
        // Buffers: the rest of what the daemon says of one.
        buffer_update(BufferUpdate::NoChange(6)),
        buffer_update(BufferUpdate::Modified(false)),
        buffer_update(BufferUpdate::Features(ServerFeatures {
            completion: Some(vec!['.']),
            signature: Some(vec!['(']),
            signature_again: vec![','],
            // Not on the wire before protocol 77.
            prepare_rename: false,
            // Nor this before protocol 79.
            uses: false,
            // Nor this before protocol 80.
            inlay_hints: false,
        })),
        buffer_update(BufferUpdate::Offer {
            language: "rust".into(),
            grammar: true,
            server: Some("rust-analyzer".into()),
        }),
        buffer_update(BufferUpdate::Moved("/src/b.rs".into())),
        buffer_update(BufferUpdate::Notice("saved".into())),
        buffer_update(BufferUpdate::ChangedOnDisk),
        buffer_update(BufferUpdate::Diagnostics {
            version: 6,
            diagnostics: vec![Diagnostic {
                start: 0,
                end: 1,
                severity: Severity::Hint,
                message: "hint".into(),
                source: Some("clippy".into()),
                code: Some("H".into()),
            }],
        }),
        buffer_update(BufferUpdate::Highlights {
            version: 6,
            start: 0,
            end: 3,
            spans: vec![Span {
                start: 0,
                end: 3,
                kind: Kind::Tag,
            }],
        }),
        Client(ClientMessage::Edit {
            buffer,
            seq: 11,
            change: Change::Redo,
        }),
        Client(ClientMessage::Query {
            buffer,
            id: 4,
            version: 6,
            at: 2,
            query: Query::Indent {
                end: 9,
                shiftwidth: 4,
                tabstop: 8,
            },
        }),
        Client(ClientMessage::Query {
            buffer,
            id: 5,
            version: 6,
            at: 2,
            query: Query::Completion(Trigger::Again),
        }),
        Client(ClientMessage::Query {
            buffer,
            id: 6,
            version: 6,
            at: 2,
            query: Query::Starts {
                construct: Construct::Argument,
                backward: true,
                count: 2,
            },
        }),
        Client(ClientMessage::Rename {
            buffer,
            id: 6,
            version: 6,
            at: 2,
            name: "start".into(),
        }),
        Client(ClientMessage::Stage {
            buffer,
            id: 7,
            version: 6,
            at: 2,
        }),
    ];
    let answers = [
        Answer::Locations(vec![FileLocation {
            path: "/src/a.rs".into(),
            line: 3,
            col: 4,
            offset: Some(20),
            text: "fn a()".into(),
        }]),
        Answer::Completions {
            items: vec![CompletionItem {
                label: "start".into(),
                kind: 3,
                detail: "fn()".into(),
                filter: Some("st".into()),
                sort: Some("a".into()),
                preselect: true,
                start: Some(1),
                text: "start()".into(),
                snippet: true,
                more: vec![TextChange {
                    start: 0,
                    end: 0,
                    text: "use x;\n".into(),
                }],
            }],
            incomplete: true,
        },
        Answer::Resolved {
            detail: Some("d".into()),
            documentation: Some(("doc".into(), true)),
        },
        Answer::Signature {
            label: "f(a)".into(),
            parameter: Some((2, 3)),
            index: 0,
            count: 1,
        },
        Answer::CodeActions(vec![CodeAction {
            title: "fix".into(),
            kind: "quickfix".into(),
            preferred: true,
            disabled: Some("not here".into()),
        }]),
        Answer::Pieces(vec![Piece {
            start: 0,
            end: 9,
            inner: Some((2, 7)),
        }]),
        Answer::Indents(vec![LineIndent {
            line: 1,
            width: Some(4),
        }]),
        Answer::Staged,
    ];
    samples.extend(answers.into_iter().map(|answer| {
        buffer_update(BufferUpdate::Answer {
            id: 4,
            version: 6,
            answer,
        })
    }));
    samples.extend([
        Client(ClientMessage::Find {
            id: 8,
            pane: Some(PaneId(1)),
            search: Search::Preview {
                path: "/src/a.rs".into(),
                line: 3,
                lines: 20,
            },
        }),
        event(Event::Found {
            id: 8,
            found: Found::Lines(vec![FoundLine {
                path: "/src/a.rs".into(),
                line: 3,
                col: 1,
                text: "fn a()".into(),
                matches: vec![(0, 2)],
            }]),
        }),
        event(Event::Found {
            id: 8,
            found: Found::Done(Done::Failed("no".into())),
        }),
        Client(ClientMessage::Clipboard {
            terminal,
            id: 1,
            text: Some("copied".into()),
        }),
        event(Event::Clipboard {
            terminal,
            selection: Selection::Primary,
            text: "copied".into(),
        }),
        event(Event::ClipboardAsked {
            terminal,
            id: 1,
            selection: Selection::Primary,
        }),
        event(Event::Installed {
            language: "rust".into(),
            outcome: Err("offline".into()),
        }),
        event(Event::Show {
            pane: PaneId(1),
            at: Some(FileLocation {
                path: "/src/a.rs".into(),
                line: 1,
                col: 0,
                offset: Some(0),
                text: String::new(),
            }),
        }),
        Client(ClientMessage::Present { here: true }),
        // File explorers, and changing files.
        request(Request::AttachFiles { files }),
        Client(ClientMessage::Files {
            files,
            action: FilesAction::Refresh,
        }),
        Client(ClientMessage::Files {
            files,
            action: FilesAction::Expand("src".into()),
        }),
    ]);
    samples.extend(
        [
            FilesUpdate::Root {
                path: "/src".into(),
                name: "src".into(),
            },
            FilesUpdate::Listed {
                dir: "/src".into(),
                entries: vec![
                    entry(EntryKind::File { executable: true }),
                    entry(EntryKind::Dir),
                    entry(EntryKind::Link { dir: true }),
                    entry(EntryKind::Other),
                ],
                cut_short: true,
            },
            FilesUpdate::Unlisted {
                dir: "/root".into(),
                reason: "denied".into(),
            },
            FilesUpdate::Git(vec![("a.rs".into(), GitMark::Conflicted)]),
            FilesUpdate::NoGit("too old".into()),
        ]
        .into_iter()
        .map(|update| event(Event::Files { files, update })),
    );
    let expect = Some(Expect {
        kind: EntryKind::File { executable: false },
        stamp: Some(stamp()),
    });
    samples.extend(
        [
            FileOp::Create {
                path: "/src/new".into(),
                dir: true,
            },
            FileOp::Move {
                from: "/src/a".into(),
                to: "/src/b".into(),
                overwrite: Some(stamp()),
                expect,
            },
            FileOp::Copy {
                from: "/src/a".into(),
                to: "/src/c".into(),
                overwrite: Some(stamp()),
                expect,
            },
            FileOp::Delete {
                path: "/src/a".into(),
                trash: true,
                expect,
            },
        ]
        .into_iter()
        .map(|op| {
            Client(ClientMessage::FileOp {
                id: 9,
                op,
                pane: None,
            })
        }),
    );
    samples.extend([
        event(Event::FileOp {
            id: 9,
            outcome: Ok(()),
        }),
        event(Event::FileOp {
            id: 9,
            outcome: Err(FileOpError::Exists {
                path: "/src/b".into(),
                what: "a file".into(),
                stamp: stamp(),
            }),
        }),
        event(Event::FileOp {
            id: 9,
            outcome: Err(FileOpError::Failed("no".into())),
        }),
        // Reviews.
        request(Request::AttachReview { review }),
        event(Event::Review {
            review,
            list: ReviewList {
                chat: Some(terminal),
                chat_name: "Claude".into(),
                scope: Some(ReviewScope::Chat),
                files: vec![ReviewFile {
                    path: "/src/a.rs".into(),
                    shown: "a.rs".into(),
                    buffer: Some(buffer),
                    hunks: vec![ReviewHunk {
                        line: 3,
                        before: vec!["a".into()],
                        base: "b\n".into(),
                        text: "c\n".into(),
                        after: vec!["d".into()],
                        agents: vec![1],
                        by: Some(Made {
                            chat: terminal,
                            turn: 2,
                        }),
                        reviewed: true,
                    }],
                }],
                comments: vec![ReviewComment {
                    id: 1,
                    path: "/src/a.rs".into(),
                    shown: "a.rs".into(),
                    line: 3,
                    text: "why?".into(),
                    in_change: true,
                }],
                note: Some("note".into()),
                chats: vec![ReviewChat {
                    chat: terminal,
                    name: "Claude".into(),
                    task: Some("look".into()),
                    turn: Some(2),
                    closed: true,
                }],
            },
        }),
        request(Request::Review(ReviewAction::Reject {
            hunks: Hunks::Turn {
                chat: terminal,
                turn: 2,
            },
            force: true,
        })),
        request(Request::Review(ReviewAction::Keep(Hunks::Chat(terminal)))),
        request(Request::Review(ReviewAction::Comment {
            chat: terminal,
            place: CommentPlace {
                path: "/src/a.rs".into(),
                line: 3,
                at: Some((buffer, 6, 2)),
            },
            text: "why?".into(),
        })),
        request(Request::Review(ReviewAction::Refresh(review))),
        // Boards.
        request(Request::AttachBoard { board }),
        event(Event::Board {
            board,
            update: BoardUpdate::Snapshot {
                summary: board_summary(),
                entries: vec![BoardEntry {
                    id: 1,
                    kind: PostKind::Result,
                    by: BoardAuthor::Chat {
                        chat: terminal,
                        agent: "claude".into(),
                        title: "Helper".into(),
                    },
                    at: 1,
                    text: "done".into(),
                    links: vec![
                        BoardLink::File {
                            path: "/src/a.rs".into(),
                            line: Some(3),
                        },
                        BoardLink::Turn {
                            chat: terminal,
                            turn: 2,
                        },
                    ],
                    subject: Some("a".into()),
                    expires: Some(9),
                    released: Some((5, BoardAuthor::User)),
                    edits: vec![(4, BoardAuthor::User, "first".into())],
                }],
            },
        }),
        event(Event::Board {
            board,
            update: BoardUpdate::Gone("archived".into()),
        }),
        Client(ClientMessage::Board {
            board,
            action: BoardAction::Post {
                kind: PostKind::Claim,
                text: "mine".into(),
            },
        }),
        Client(ClientMessage::Board {
            board,
            action: BoardAction::Archive(true),
        }),
        // A chat's browser.
        request(Request::AttachBrowser {
            chat: terminal,
            width: 800,
            height: 600,
        }),
        event(Event::Browser {
            chat: terminal,
            update: BrowserUpdate::Status(BrowserStatus {
                url: "https://example.com".into(),
                title: "Example".into(),
                action: Some("clicked".into()),
                running: true,
                failure: Some("slow".into()),
            }),
        }),
        event(Event::Browser {
            chat: terminal,
            update: BrowserUpdate::Frame(BrowserFrame {
                jpeg: vec![0xff, 0xd8, 0xff],
            }),
        }),
        // Devices on the web app.
        request(Request::Devices),
        response(Response::Devices(vec![Device {
            id: 2,
            name: "iPhone".into(),
            paired: 1,
            used: 2,
            login: Some("jack@".into()),
            notifications: true,
            connected: true,
        }])),
        event(Event::Pairing(PairingState {
            asking: vec![PairingAsk {
                name: "iPad".into(),
                from: "100.64.0.2".into(),
                until: 9,
            }],
            locked: true,
        })),
        // A view whose modes and colours are the last of each.
        event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![Patch::Grid {
                node: NodeId::ROOT,
                state: GridState {
                    modes: InputModes {
                        mouse_tracking: MouseTracking::Motion,
                        mouse_encoding: MouseEncoding::Urxvt,
                        ..InputModes::default()
                    },
                    shell: ShellState::Busy,
                    ..grid_state()
                },
                lines: vec![(
                    0,
                    Line {
                        runs: vec![Run {
                            style: Style {
                                fg: Color::Rgb(9, 8, 7),
                                underline: Underline::Dashed,
                                ..Style::DEFAULT
                            },
                            text: "x".into(),
                            separate: false,
                        }],
                        wrapped: false,
                    },
                )],
                links: Vec::new(),
                marks: BlockMarks::default(),
            }]),
        }),
    ]);
    samples
}

fn summary() -> ChatSummary {
    ChatSummary {
        chat: TerminalId(8),
        title: "Helper".into(),
        agent: "claude".into(),
        state: ChatState::Failed("gone".into()),
        asking: true,
        unseen: true,
        parent: Some(TerminalId(7)),
        workspace: Some(WorkspaceId(1)),
        active: 9,
        profile: None,
        failed: None,
    }
}

fn stamp() -> FileStamp {
    FileStamp {
        device: 1,
        inode: 2,
        len: 3,
        seconds: 4,
        nanoseconds: 5,
    }
}

fn entry(kind: EntryKind) -> Entry {
    Entry {
        name: "a".into(),
        kind,
        ignored: true,
        stamp: Some(stamp()),
    }
}

fn board_summary() -> BoardSummary {
    BoardSummary {
        id: BoardId(5),
        name: "plan".into(),
        purpose: "the plan".into(),
        scope: BoardScope::Workspace("w".into()),
        archived: true,
        created: 1,
        entries: 1,
    }
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
            home: None,
            key: None,
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
            home: None,
            lent: None,
            background: Vec::new(),
        }],
        active: Some(WorkspaceId(1)),
        home: None,
        restored: Vec::new(),
        machine: None,
        others: Vec::new(),
        passed: Vec::new(),
        kept: Vec::new(),
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
        activities: Vec::new(),
        tasks: Vec::new(),
        plan: Vec::new(),
        commands: Vec::new(),
        prompts: Vec::new(),
        draft: None,
        paths: None,
        branches: Vec::new(),
        provider_usage: vec![crate::ProviderUsage {
            agent: "codex".into(),
            title: "Codex".into(),
            checked: 1,
            refreshing: false,
            limits: vec![crate::UsageLimit {
                id: "weekly".into(),
                label: "Weekly".into(),
                used: 42,
                resets: Some(2),
            }],
            notice: None,
        }],
        delivery: crate::ChatDelivery::default(),
        first_turn: 0,
        first_message: 0,
        retrying: None,
    }
}

fn turn() -> Turn {
    Turn {
        woke: None,
        previews: Vec::new(),
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
            raw: None,
            spent: None,
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
        exit: None,
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
        form: None,
        plan: None,
        tool: None,
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
        // What older versions' fixtures hold: limits came at 81, sampled
        // there.
        five_hour: None,
        weekly: None,
        cost: None,
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
        tasks: Vec::new(),
    }
}

/// A shell an agent runs in the background, `state`.
fn agent_task(kind: crate::AgentTaskKind, state: crate::AgentTaskState) -> crate::AgentTask {
    crate::AgentTask {
        id: "bh1jmizrr".into(),
        kind,
        title: "Start the dev server".into(),
        command: Some("npm run dev".into()),
        state,
        summary: Some("exit code 0".into()),
        started: 1_760_000_000,
        ended: None,
        can_stop: true,
        output: None,
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
    fn each_fixture_reads_back_and_encodes_again_to_the_same_bytes() {
        for (wire, bytes) in fixtures() {
            let read = read_fixture(wire, &bytes).unwrap();
            let again: Vec<u8> = (read.iter())
                .flat_map(|sample| {
                    match sample {
                        Sample::Client(message) => frame::encode(wire, message),
                        Sample::Server(message) => frame::encode(wire, message),
                    }
                    .unwrap()
                })
                .collect();
            assert!(
                again == bytes,
                "protocol {wire}'s fixture changed on the way through"
            );
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
