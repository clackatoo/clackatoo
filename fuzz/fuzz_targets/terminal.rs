//! The whole terminal: what a program writes, onto the grid, with
//! resizes, scrollback and searches as windows ask for them. It must never
//! panic, and a snapshot of it, as handed over to the next build, must
//! make the same terminal again.
//!
//! The input is four bytes, then what the program writes: the width less
//! one, the height less one (mod 64), how much is written between looks
//! less one, and the scrollback in fours of lines, its lowest bit asking
//! for a resize at every fourth look, its next two the width rules (VS16
//! widening, grapheme clusters), and its next the size of a cell in
//! pixels said at every fourth look too. Any stream is a seed, with four
//! bytes before it.

#![no_main]

use clackatoo_term::{Compaction, Config, Damage, Hints, Position, Size, Terminal, Widths};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&[width, height, chunk, scrollback], stream)) = data.split_first_chunk::<4>() else {
        return;
    };
    let size = |cols: u8, rows: u8| {
        Size::new(u16::from(cols) + 1, u16::from(rows % 64) + 1).expect("in range")
    };
    let config = Config {
        scrollback_lines: usize::from(scrollback) * 4,
        scrollback_compaction: match scrollback % 3 {
            0 => Compaction::Off,
            1 => Compaction::Lazy,
            _ => Compaction::Eager,
        },
        local_host: None,
        widths: Widths {
            vs16: scrollback & 2 != 0,
            graphemes: scrollback & 4 != 0,
        },
    };
    let mut terminal = Terminal::new(size(width, height), &config);
    let mut damage = Damage::new();
    for (look, piece) in stream.chunks(usize::from(chunk) + 1).enumerate() {
        terminal.advance(piece);
        let _ = terminal.take_forget();
        let _ = terminal.take_replies();
        let _ = terminal.take_clipboard();
        let _ = terminal.take_history_cleared();
        let _ = terminal.take_notice();
        terminal.take_damage(&mut damage);
        damage.clear();
        if terminal.scrollback_needs_compaction() {
            terminal.compact_scrollback();
        }
        if scrollback & 1 == 1 && look % 4 == 3 {
            let (&last, rest) = piece.split_last().expect("chunks aren't empty");
            terminal.resize(size(last, rest.last().copied().unwrap_or(height)));
        }
        if scrollback & 8 != 0 && look % 4 == 1 {
            let first = piece[0];
            terminal.set_cell_pixels(Some([u16::from(first), u16::from(chunk)]));
        }
    }
    let _ = (terminal.title(), terminal.cwd(), terminal.cursor());
    let _ = (terminal.input_modes(), terminal.shell_state());
    let _ = (terminal.pointer(), terminal.margins(), terminal.pixels());
    terminal.compact_scrollback();

    // Read as windows ask: text, and searches from anywhere.
    let (first, last) = terminal.document_bounds();
    let at = |line: u64, col: usize| Position { line, col };
    let _ = terminal.screen_text();
    let _ = terminal.text(at(first, 0), at(last, usize::MAX), false);
    let _ = terminal.text(at(last.saturating_add(3), 7), at(first, 2), true);
    let shown = |line: u64, _: &clackatoo_term::Row, _: Hints| line % 3 == 0;
    let _ = terminal.text_masked(at(first, 0), at(last, usize::MAX), false, Hints::all(), &shown);
    let _ = terminal.block_text_masked(at(last, 3), at(first, 1), Hints::HIDE, &shown);
    let _ = terminal.text_for_agents(at(first, 0), at(last, usize::MAX), true);
    let query = String::from_utf8_lossy(stream.get(..3).unwrap_or(stream));
    for (backward, count) in [(false, 1), (true, 3), (false, usize::MAX)] {
        let from = at(u64::from(chunk) + first, usize::from(width));
        let _ = terminal.search_nth(&query, from, backward, count > 1, count);
        let _ = terminal.search_nth_masked(&query, from, backward, true, count, Hints::HIDE, &shown);
    }

    // Handed over and taken back, it's the same terminal.
    let (snapshot, links) = terminal.snapshot_with_links();
    let blocks = terminal.blocks_snapshot();
    let regions = terminal.regions();
    let mut restored = Terminal::restore_with_links(&snapshot, &links, &config)
        .expect("a terminal's own snapshot restores");
    restored.restore_blocks(&blocks);
    if let Some(regions) = &regions {
        restored.restore_regions(regions);
    }
    assert_eq!(restored.regions(), regions);
    let (again, again_links) = restored.snapshot_with_links();
    assert_eq!(again, snapshot);
    assert_eq!(again_links, links);
    assert_eq!(restored.blocks_snapshot(), blocks);
    // As the handover writes it.
    let written = postcard::to_allocvec(&(&snapshot, &links, &blocks)).expect("it encodes");
    let read: (_, _, _) = postcard::from_bytes(&written).expect("it decodes");
    assert_eq!(read, (snapshot, links, blocks));
});
