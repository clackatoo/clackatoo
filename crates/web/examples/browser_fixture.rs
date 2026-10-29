//! Synthetic, in-memory daemon for browser regression tests. Hex-framed
//! protocol on stdin/stdout; never opens a real daemon, account or terminal.
use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    ChatAction, ChatDelivery, ChatNode, ChatState, ChatSummary, Choice, ChoiceKind, ClientMessage,
    Error, Event, Item, Node, Outcome, PROTOCOL_VERSION, ProviderUsage, Question, QueuedMessage,
    Request, Response, ServerMessage, Setting, SettingChoice, SettingKind, TabId, TerminalId, Turn,
    TurnEnd, Usage, UsageLimit, View, ViewUpdate, Workspace, WorkspaceId, Workspaces,
};
use std::fmt::Write as _;
use std::io::{self, BufRead as _, Write as _};

use clackatoo_protocol::{
    Background, BackgroundAction, BackgroundState, ChatPreview, Content, GridNode, GridState, Kept,
    Layout, Line, Listening, ListeningPort, Pane, PaneId, Run, Size, Style, Tab, TerminalInfo,
    TerminalStatus,
};

fn terminal_info() -> TerminalInfo {
    TerminalInfo {
        id: TerminalId(5),
        pid: None,
        size: Size::new(80, 24).unwrap(),
        title: "Development shell".into(),
        status: TerminalStatus::Running,
        cwd: Some("~/projects/clackatoo".into()),
        command: None,
    }
}
fn terminal_snapshot(size: Size) {
    shell_snapshot(TerminalId(5), size);
}

fn shell_snapshot(terminal: TerminalId, size: Size) {
    let mut lines = vec![Line::default(); usize::from(size.rows())];
    for (row, text) in [
        "~/projects/clackatoo > cargo check",
        "    Checking clackatoo-web",
        "    Finished dev profile",
        "~/projects/clackatoo >",
    ]
    .into_iter()
    .enumerate()
    .take(lines.len())
    {
        lines[row] = Line {
            runs: vec![Run {
                style: Style::DEFAULT,
                text: text.into(),
                separate: false,
            }],
            wrapped: false,
        };
    }
    reply(&ServerMessage::Event(Event::View {
        terminal,
        update: ViewUpdate::Snapshot(View {
            title: "Development shell".into(),
            cwd: None,
            root: Node::Grid(GridNode {
                size,
                state: GridState::default(),
                lines,
                links: vec![],
                marks: clackatoo_protocol::BlockMarks::default(),
            }),
        }),
    }));
}

fn now() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn reply(message: &ServerMessage) {
    let bytes = frame::encode(PROTOCOL_VERSION, message).unwrap();
    let mut hex = String::new();
    for byte in bytes {
        write!(hex, "{byte:02x}").unwrap();
    }
    println!("{hex}");
    io::stdout().flush().unwrap();
}

fn workspaces() -> Workspaces {
    // Background tabs: a dev server, a test watcher that failed, and a
    // docs server kept running.
    let background = |id: u32, name: &str, state, failed| Background {
        tab: Tab {
            id: TabId(id),
            layout: Layout::Pane(Pane {
                id: PaneId(id + 10),
                content: Content::Terminal(TerminalId(id + 20)),
                home: None,
                key: None,
            }),
            focused: PaneId(id + 10),
            name: None,
        },
        state,
        failed,
        name: name.into(),
    };
    let mut workspaces = Workspaces {
        list: ["Clackatoo", "Website"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| Workspace {
                id: WorkspaceId(u32::try_from(i).unwrap() + 1),
                name: name.into(),
                tabs: vec![],
                active_tab: TabId(1),
                home: None,
                lent: None,
                background: vec![],
            })
            .collect(),
        ..Workspaces::default()
    };
    workspaces.list[0].background = vec![
        background(21, "pnpm dev", BackgroundState::Running, None),
        background(
            22,
            "cargo watch -x test",
            BackgroundState::Ended(TerminalStatus::Exited(101)),
            Some(101),
        ),
    ];
    workspaces.list[1].background =
        vec![background(23, "trail serve", BackgroundState::Idle, None)];
    workspaces.kept = vec![Kept {
        pane: PaneId(33),
        command: "trail serve".into(),
        always: true,
        given_up: false,
        paused: false,
    }];
    workspaces
}

/// The ports the background tabs listen on.
fn listening() -> Vec<Listening> {
    let port = |port, program: &str| ListeningPort {
        port,
        program: program.into(),
    };
    vec![
        Listening {
            tab: TabId(21),
            ports: vec![port(5173, "node"), port(24678, "node")],
        },
        Listening {
            tab: TabId(22),
            ports: vec![],
        },
        Listening {
            tab: TabId(23),
            ports: vec![port(8080, "trail")],
        },
    ]
}

/// What a background action does to `workspaces`, answered as the daemon
/// would answer it.
fn background_action(workspaces: &mut Workspaces, action: &BackgroundAction) {
    eprintln!("Background {action:?}");
    let take = |workspaces: &mut Workspaces, tab: TabId| {
        workspaces.list.iter_mut().find_map(|workspace| {
            let at = (workspace.background.iter()).position(|b| b.tab.id == tab)?;
            Some((workspace.id, workspace.background.remove(at)))
        })
    };
    let response = match *action {
        BackgroundAction::Ports => {
            reply(&ServerMessage::Response(Response::Done));
            reply(&ServerMessage::Event(Event::Listening(listening())));
            return;
        }
        BackgroundAction::Bring { tab } => {
            let Some((workspace, background)) = take(workspaces, tab) else {
                return reply(&ServerMessage::Response(Response::Error(Error::NoSuchTab(
                    tab,
                ))));
            };
            let pane = background.tab.focused;
            let workspace = (workspaces.list.iter_mut()).find(|w| w.id == workspace);
            workspace.unwrap().tabs.push(background.tab);
            Response::PaneCreated(pane)
        }
        BackgroundAction::Dismiss { tab } => {
            if take(workspaces, tab).is_none() {
                return reply(&ServerMessage::Response(Response::Error(Error::NoSuchTab(
                    tab,
                ))));
            }
            Response::Done
        }
        BackgroundAction::Seen { workspace } => {
            for w in workspaces.list.iter_mut().filter(|w| w.id == workspace) {
                for background in &mut w.background {
                    background.failed = None;
                }
            }
            Response::Done
        }
        _ => Response::Error(Error::Other("not in this fixture".into())),
    };
    reply(&ServerMessage::Event(Event::Workspaces(workspaces.clone())));
    reply(&ServerMessage::Response(response));
}

fn chats() -> Vec<ChatSummary> {
    [
        (
            1,
            "Polish the mobile experience",
            "Codex",
            ChatState::Answering,
            false,
            false,
            1,
        ),
        (
            2,
            "Review authentication changes",
            "Claude Code",
            ChatState::Answering,
            true,
            false,
            1,
        ),
        (
            3,
            "Refresh the project documentation",
            "Gemini",
            ChatState::Ready,
            false,
            true,
            2,
        ),
        (
            4,
            "Fix the deployment pipeline",
            "Codex",
            ChatState::Failed("Build failed".into()),
            false,
            false,
            2,
        ),
    ]
    .into_iter()
    .map(
        |(id, title, agent, state, asking, unseen, workspace)| ChatSummary {
            chat: TerminalId(id),
            title: title.into(),
            agent: agent.into(),
            state,
            asking,
            unseen,
            parent: None,
            workspace: Some(WorkspaceId(workspace)),
            active: now().saturating_sub(120_000),
            profile: None,
            failed: Some(false),
        },
    )
    .collect()
}

fn setting(id: &str, name: &str, kind: SettingKind, choices: &[&str]) -> Setting {
    Setting {
        id: id.into(),
        name: name.into(),
        kind,
        value: choices[0].into(),
        choices: choices
            .iter()
            .map(|s| SettingChoice {
                name: (*s).into(),
                value: (*s).into(),
                description: None,
            })
            .collect(),
    }
}

#[allow(clippy::too_many_lines, reason = "complete browser fixture state")]
fn node(id: TerminalId) -> ChatNode {
    let turn = |i| {
        Turn {
        woke: None,
        prompt: if i == 39 { "Make the remote experience feel as good as the desktop, especially on my phone.".into() } else { format!("Check the behavior in case {i}.") },
        sent: now().saturating_sub(30_000), items: vec![Item::Words("The layout now keeps the conversation readable and the controls within reach.\n\n- Review changes without losing your place\n- Keep drafts when you switch chats\n- Respond to an approval from your phone\n\n```rust\nlet message = queue.submit(draft);\n```".into())],
        end: Some(TurnEnd { at: now().saturating_sub(8_000), outcome: Outcome::Done, raw: None, spent: None }), files: None, from: None, switch: None, previews: if i == 39 { vec![ChatPreview { width: 64, height: 40, rgba: [35, 90, 65, 255].repeat(64 * 40).into(), original: 0 }] } else { vec![] },
    }
    };
    let mut node = ChatNode {
        activities: vec![clackatoo_protocol::Activity {
            id: "web-build".into(),
            owner: id,
            turn: 39,
            title: "Build mobile client".into(),
            command: "cargo build".into(),
            cwd: "/project".into(),
            pane: Some(clackatoo_protocol::PaneId(9)),
            started: now(),
            ended: None,
            state: clackatoo_protocol::ActivityState::Running,
            result: String::new(),
        }],
        agent: "Codex".into(),
        state: ChatState::Answering,
        turns: (0..40).map(turn).collect(),
        settings: vec![
            setting(
                "model",
                "Model",
                SettingKind::Model,
                &["6 Astra", "6 Sol", "6 Luna"],
            ),
            setting(
                "mode",
                "Permissions",
                SettingKind::Mode,
                &["Auto review", "Ask"],
            ),
            setting(
                "effort",
                "Reasoning",
                SettingKind::Effort,
                &["High", "Medium", "Low"],
            ),
        ],
        usage: Some(Usage {
            used: 41000,
            size: 258_000,
            five_hour: None,
            weekly: None,
            cost: None,
        }),
        delivery: ChatDelivery {
            native_steering: true,
            queued: vec![QueuedMessage {
                id: 10,
                text: "Also check landscape mode".into(),
                images: 0,
                target: None,
            }],
            ..ChatDelivery::default()
        },
        provider_usage: vec![
            ProviderUsage {
                agent: "codex".into(),
                title: "Codex".into(),
                limits: vec![UsageLimit {
                    id: "weekly".into(),
                    label: "Weekly".into(),
                    used: 17,
                    resets: None,
                }],
                checked: 1000,
                ..ProviderUsage::default()
            },
            ProviderUsage {
                agent: "claude".into(),
                title: "Claude Code".into(),
                limits: vec![UsageLimit {
                    id: "session".into(),
                    label: "Session".into(),
                    used: 60,
                    resets: None,
                }],
                checked: 1000,
                ..ProviderUsage::default()
            },
        ],
        ..ChatNode::default()
    };
    if id == TerminalId(2) {
        node.question = Some(Question {
            id: 9,
            title: "run `cargo test --workspace`".into(),
            own: false,
            choices: vec![
                Choice {
                    id: "allow".into(),
                    name: "Allow once".into(),
                    kind: ChoiceKind::AllowOnce,
                },
                Choice {
                    id: "deny".into(),
                    name: "Reject".into(),
                    kind: ChoiceKind::RejectOnce,
                },
            ],
            form: None,
            plan: None,
            tool: None,
        });
    }
    node
}

fn snapshot(id: TerminalId, node: &ChatNode) {
    reply(&ServerMessage::Event(Event::View {
        terminal: id,
        update: ViewUpdate::Snapshot(View {
            title: chats().into_iter().find(|c| c.chat == id).unwrap().title,
            cwd: None,
            root: Node::Chat(Box::new(node.clone())),
        }),
    }));
}

#[allow(
    clippy::too_many_lines,
    reason = "isolated protocol fixture with one branch per browser action"
)]
fn main() {
    let mut decoder = FrameDecoder::new();
    let mut nodes = std::collections::BTreeMap::new();
    let mut starts = std::collections::BTreeMap::new();
    let mut shown = workspaces();
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let bytes: Vec<_> = line
            .as_bytes()
            .chunks_exact(2)
            .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
            .collect();
        decoder.push(&bytes);
        while let Some(message) = decoder.decode::<ClientMessage>(PROTOCOL_VERSION).unwrap() {
            match message {
                ClientMessage::Hello { .. } => reply(&ServerMessage::Hello {
                    protocol_version: PROTOCOL_VERSION,
                }),
                ClientMessage::Request(request) => match request {
                    Request::HarnessCapabilities { .. } => {
                        reply(&ServerMessage::Response(Response::Text(
                            serde_json::json!([
                                ["Native window", "Off — no window granted"],
                                ["Managed activities", "Available"],
                                ["Model acknowledgment", "Unconfirmed"]
                            ])
                            .to_string(),
                        )));
                    }
                    Request::CancelActivity { terminal, id } => {
                        eprintln!("CancelActivity {id}");
                        let node = nodes.entry(terminal).or_insert_with(|| node(terminal));
                        for a in &mut node.activities {
                            if a.id == id {
                                a.state = clackatoo_protocol::ActivityState::Cancelling;
                            }
                        }
                        reply(&ServerMessage::Response(Response::Done));
                        snapshot(
                            terminal,
                            &node.since_turn(starts.get(&terminal).copied().or(Some(0))),
                        );
                    }

                    Request::WatchWorkspaces => {
                        reply(&ServerMessage::Response(Response::Workspaces(
                            shown.clone(),
                        )));
                    }
                    Request::Background(action) => background_action(&mut shown, &action),
                    Request::Attach { terminal } if (41..=43).contains(&terminal.0) => {
                        reply(&ServerMessage::Response(Response::Done));
                        shell_snapshot(terminal, terminal_info().size);
                    }
                    Request::WatchChats => {
                        reply(&ServerMessage::Response(Response::Chats(chats())));
                    }
                    Request::ListTerminals => {
                        reply(&ServerMessage::Response(Response::Terminals(vec![
                            terminal_info(),
                        ])));
                    }
                    Request::Attach {
                        terminal: TerminalId(5),
                    } => {
                        reply(&ServerMessage::Response(Response::Done));
                        terminal_snapshot(terminal_info().size);
                    }
                    Request::SendInput { bytes, .. } => {
                        let mut hex = String::with_capacity(bytes.len() * 2);
                        for byte in bytes {
                            write!(hex, "{byte:02x}").unwrap();
                        }
                        eprintln!("TerminalInput {hex}");
                        reply(&ServerMessage::Response(Response::Done));
                    }
                    Request::Fit { size, .. } => {
                        reply(&ServerMessage::Response(Response::Done));
                        terminal_snapshot(size.unwrap_or_else(|| terminal_info().size));
                    }
                    Request::AttachRecent { terminal, first } => {
                        reply(&ServerMessage::Response(Response::Done));
                        let node = nodes.entry(terminal).or_insert_with(|| node(terminal));
                        let recent = node.since_turn(first);
                        eprintln!("Recent {} {}", recent.first_turn, recent.turns.len());
                        starts.insert(terminal, recent.first_turn);
                        snapshot(terminal, &recent);
                    }
                    Request::ChatHistory { terminal, before } => {
                        reply(&ServerMessage::Response(Response::Done));
                        let node = nodes.entry(terminal).or_insert_with(|| node(terminal));
                        let page = node.history_page(before).unwrap();
                        if let clackatoo_protocol::ChatPatch::History { first, .. } = &page {
                            starts.insert(terminal, *first);
                            eprintln!("History {first} {before}");
                        }
                        std::thread::sleep(std::time::Duration::from_millis(80));
                        reply(&ServerMessage::Event(Event::View {
                            terminal,
                            update: ViewUpdate::Patch(vec![clackatoo_protocol::Patch::Chat(page)]),
                        }));
                    }
                    Request::Attach { terminal } => {
                        reply(&ServerMessage::Response(Response::Done));
                        snapshot(
                            terminal,
                            nodes.entry(terminal).or_insert_with(|| node(terminal)),
                        );
                    }
                    Request::Chat { terminal, action } => {
                        eprintln!("{action:?}");
                        let node = nodes.entry(terminal).or_insert_with(|| node(terminal));
                        if matches!(&action, ChatAction::Submit{text,..} if text=="reject") {
                            reply(&ServerMessage::Response(Response::Error(
                                Error::NoSuchTerminal(terminal),
                            )));
                            continue;
                        }
                        match action {
                            ChatAction::Submit { text, images, .. } => {
                                if text == "delayed" {
                                    std::thread::sleep(std::time::Duration::from_millis(350));
                                }
                                node.delivery.queued.push(QueuedMessage {
                                    id: 20,
                                    text,
                                    images: u8::try_from(images.len()).unwrap(),
                                    target: None,
                                });
                            }
                            ChatAction::RemoveQueued { id } | ChatAction::SendQueued { id, .. } => {
                                node.delivery.queued.retain(|q| q.id != id);
                            }
                            ChatAction::Answer { .. } | ChatAction::Fill { .. } => {
                                node.question = None;
                            }
                            ChatAction::Set { setting, value } => {
                                if let Some(s) = node.settings.iter_mut().find(|s| s.id == setting)
                                {
                                    s.value = value;
                                }
                            }
                            _ => {}
                        }
                        reply(&ServerMessage::Response(Response::Done));
                        snapshot(
                            terminal,
                            &node.since_turn(starts.get(&terminal).copied().or(Some(0))),
                        );
                    }
                    _ => reply(&ServerMessage::Response(Response::Done)),
                },
                _ => {}
            }
        }
    }
}
