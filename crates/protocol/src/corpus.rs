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
use crate::wire::{EXITED, FALLBACKS, HOVER, PHONE_TERMINALS, SYMBOLS, WORDS};
use crate::{
    AgentSession, AgentTools, Author, Axis, BoardId, BufferId, BufferSnapshot, BufferUpdate,
    Change, ChatAction, ChatName, ChatNode, ChatPatch, ChatState, Choice, ChoiceKind,
    ClientMessage, Content, CursorState, Diff, Error, Event, FilesId, GridNode, GridState,
    HistoryLine, HistoryLines, Item, Layout, Line, LinkTarget, Location, NewAgent, NewReview,
    NewTerminal, Node, NodeId, Outcome, Pane, PaneId, PaneKind, PaneTerminal, Patch, Provider,
    ProviderModels, Question, Request, Response, Resume, ResumeSession, ReviewId, Run, Sender,
    ServerMessage, Setting, SettingChoice, SettingKind, Side, Subagent, SubagentStatus, Switch,
    Tab, TabId, TerminalId, TerminalInfo, TerminalStatus, Tool, ToolContent, ToolKind, ToolStatus,
    Turn, TurnEnd, TurnFiles, TurnFilesState, Usage, View, ViewUpdate, Workspace, WorkspaceId,
    Workspaces,
};
use crate::{
    Answer, BoardAction, BoardAuthor, BoardEntry, BoardLink, BoardScope, BoardSummary, BoardUpdate,
    BrowserFrame, BrowserStatus, BrowserUpdate, ChatChoices, ChatSummary, CodeAction, CommentPlace,
    CompletionItem, Device, Done, Entry, EntryKind, Expect, FileLocation, FileOp, FileOpError,
    FileStamp, FilesAction, FilesUpdate, Found, FoundLine, FoundSymbol, GitMark, Hunks, Made,
    OpenBoard, OpenFile, OpenFiles, PairingAsk, PairingState, PostKind, Query, ReviewAction,
    ReviewChat, ReviewComment, ReviewFile, ReviewHunk, ReviewList, ReviewScope, Search,
    ServerFeatures, Symbol, TextChange, Trigger, Udp,
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
fn tagged(wire: u32) -> Vec<(u32, Sample)> {
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
                        }),
                        focused: PaneId(1),
                        name: None,
                    }],
                    active_tab: TabId(2),
                }],
                active: None,
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
    samples
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
        })
    };
    let mut samples = vec![
        // The last of each top-level enum.
        Client(ClientMessage::ListSessions {
            id: 3,
            agent: "claude".into(),
            cwd: "/src".into(),
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
            }),
        })),
        response(Response::Text("text".into())),
        request(Request::Search {
            terminal,
            query: "q".into(),
            from: Position { line: 3, col: 4 },
            backward: true,
            exact: true,
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
        .map(|op| Client(ClientMessage::FileOp { id: 9, op })),
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
