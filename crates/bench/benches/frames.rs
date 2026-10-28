//! Encoding and decoding view updates: what the daemon spends on each
//! update it sends a window, and the window on each it reads.
//!
//! Two shapes of update: a snapshot of a large terminal, 300 columns by
//! 80 rows of coloured text, as attaching to one sends; and a stream of
//! small patches, a few lines and the cursor each, as typing or a
//! program's output sends, one per frame.

use std::fmt::Write as _;
use std::hint::black_box;

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    BlockMarks, Event, GridState, Line, NodeId, PROTOCOL_VERSION, Patch, ServerMessage, TerminalId,
    View, ViewUpdate,
};
use clackatoo_term::{Config, Size, Terminal};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

/// A terminal 300 by 80, every row full of words in colours.
fn terminal() -> Terminal {
    let size = Size::new(300, 80).expect("300x80 is a size");
    let mut terminal = Terminal::new(size, &Config::default());
    let mut output = String::new();
    for row in 0..80 {
        for word in 0..40 {
            let _ = write!(output, "\x1b[3{}mword{:03} ", (row + word) % 8, word);
        }
        output.push_str("\x1b[0m\r\n");
    }
    terminal.advance(output.as_bytes());
    terminal
}

/// A snapshot of the terminal, as attaching to it sends.
fn snapshot(terminal: &Terminal) -> ServerMessage {
    ServerMessage::Event(Event::View {
        terminal: TerminalId(1),
        update: ViewUpdate::Snapshot(View::capture(terminal)),
    })
}

/// A thousand patches of two lines each, and the cursor.
fn patches(terminal: &Terminal) -> Vec<ServerMessage> {
    let lines: Vec<Line> = terminal.grid().rows().map(Line::from_row).collect();
    let state = GridState::of(terminal);
    (0..1000_u16)
        .map(|n| {
            let at = n % 79;
            ServerMessage::Event(Event::View {
                terminal: TerminalId(1),
                update: ViewUpdate::Patch(vec![Patch::Grid {
                    node: NodeId::ROOT,
                    state,
                    lines: vec![
                        (at, lines[usize::from(at)].clone()),
                        (at + 1, lines[usize::from(at) + 1].clone()),
                    ],
                    links: Vec::new(),
                    marks: BlockMarks::default(),
                }]),
            })
        })
        .collect()
}

fn encode(message: &ServerMessage) -> Vec<u8> {
    frame::encode(PROTOCOL_VERSION, message).expect("updates encode")
}

fn decode(decoder: &mut FrameDecoder) -> Option<ServerMessage> {
    decoder.decode(PROTOCOL_VERSION).expect("updates decode")
}

fn frames(c: &mut Criterion) {
    let terminal = terminal();
    let snapshot = snapshot(&terminal);
    let patches = patches(&terminal);
    let snapshot_bytes = encode(&snapshot);
    let patch_bytes: Vec<u8> = patches.iter().flat_map(encode).collect();
    eprintln!(
        "a snapshot is {} bytes; 1000 patches are {} bytes",
        snapshot_bytes.len(),
        patch_bytes.len()
    );

    let mut group = c.benchmark_group("frames");
    group.throughput(Throughput::Elements(1));
    group.bench_function("encode a 300x80 snapshot", |b| {
        b.iter(|| encode(black_box(&snapshot)));
    });
    group.bench_function("decode a 300x80 snapshot", |b| {
        b.iter(|| {
            let mut decoder = FrameDecoder::new();
            decoder.push(black_box(&snapshot_bytes));
            decode(&mut decoder)
        });
    });
    group.throughput(Throughput::Elements(1000));
    group.bench_function("encode 1000 two-line patches", |b| {
        b.iter(|| {
            for patch in black_box(&patches) {
                black_box(encode(patch));
            }
        });
    });
    group.bench_function("decode 1000 two-line patches", |b| {
        b.iter(|| {
            let mut decoder = FrameDecoder::new();
            decoder.push(black_box(&patch_bytes));
            while let Some(message) = decode(&mut decoder) {
                black_box(message);
            }
        });
    });
    group.finish();
}

/// Compare the former whole-history opening path with the paged path on
/// the same 12 MiB conversation, including projection, cloning and encoding.
fn chat_history(c: &mut Criterion) {
    use clackatoo_protocol::{ChatNode, Item, Node, Turn};
    let node = ChatNode {
        turns: (0..96)
            .map(|i| Turn {
                woke: None,
                prompt: format!("Turn {i}"),
                sent: 0,
                items: vec![Item::Words("answer ".repeat(18_000))],
                end: None,
                files: None,
                from: None,
                switch: None,
                previews: vec![],
            })
            .collect(),
        ..ChatNode::default()
    };
    let snapshot = |node| {
        ServerMessage::Event(Event::View {
            terminal: TerminalId(1),
            update: ViewUpdate::Snapshot(View {
                title: "history".into(),
                cwd: None,
                root: Node::Chat(Box::new(node)),
            }),
        })
    };
    eprintln!(
        "chat full={} bytes, recent={} bytes",
        encode(&snapshot(node.clone())).len(),
        encode(&snapshot(node.since_turn(None))).len()
    );
    let mut group = c.benchmark_group("chat-history");
    group.bench_function("full snapshot", |b| {
        b.iter(|| encode(&snapshot(black_box(&node).clone())));
    });
    group.bench_function("recent snapshot", |b| {
        b.iter(|| encode(&snapshot(black_box(&node).since_turn(None))));
    });
    group.bench_function("older page", |b| {
        b.iter(|| {
            let page = black_box(&node).history_page(92).unwrap();
            encode(&ServerMessage::Event(Event::View {
                terminal: TerminalId(1),
                update: ViewUpdate::Patch(vec![Patch::Chat(page)]),
            }))
        });
    });
    group.finish();
}

criterion_group!(benches, frames, chat_history);
criterion_main!(benches);
