//! The terminal: an escape-sequence parser driving the emulator state.

use std::ops::Range;

use std::collections::HashSet;
use std::sync::Arc;

use bitflags::bitflags;

use crate::blocks::{self, Blocks, Mark};
use crate::charset::Charsets;
use crate::clipboard::{self, ClipboardRequest};
use crate::colours::{self, Palette};
use crate::damage::Damage;
use crate::graphics::{
    Graphics, GraphicsSnapshot, ImageError, ImageKept, ImageKey, ImageRequest, Picture, Virtual,
};
use crate::grid::Grid;
use crate::keyboard;
use crate::links::{self, Hyperlink, LinkId, Links, local_file_uri};
use crate::mouse::{MouseEncoding, MouseTracking};
use crate::notify::{Notice, Notifications};
use crate::parser::Parser;
use crate::pointer::{self, Pointer};
use crate::progress::TaskProgress;
use crate::regions::Regions;
use crate::requests::{self, Request};
use crate::row::Row;
use crate::scrollback::{Compaction, Scrollback};
use crate::sgr;
use crate::size::Size;
use crate::snapshot::{
    AskedSnapshot, BlocksSnapshot, LineLinks, LinksSnapshot, RestoreError, Snapshot, position,
};
use crate::style::Style;
use crate::tabs::TabStops;
use crate::width::{self, Widths};

/// Settings fixed when a terminal is created.
#[derive(Clone, Debug)]
pub struct Config {
    /// How many lines scrolled off the top of the primary screen are kept.
    pub scrollback_lines: usize,
    /// When those lines are compacted to save memory.
    pub scrollback_compaction: Compaction,
    /// The name of the machine the terminal's programs run on. A
    /// `file://` link naming it, as `ls --hyperlink` prints them, is kept
    /// as a link to a local file, `file:///path`, so a window anywhere
    /// knows it's on the daemon's machine.
    pub local_host: Option<String>,
    /// The rules its cells are laid out by (see [`Widths`]): by default,
    /// variation selector 16 widens an emoji, as in kitty and Ghostty.
    pub widths: Widths,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrollback_lines: 10_000,
            scrollback_compaction: Compaction::default(),
            local_host: None,
            widths: Widths {
                vs16: true,
                graphemes: true,
            },
        }
    }
}

/// Where the cursor is and whether it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Screen line, counting from 0 at the top.
    pub line: usize,
    /// Column, counting from 0 at the left.
    pub col: usize,
    /// Whether the application wants the cursor drawn (DECTCEM).
    pub visible: bool,
    /// The shape the application asked for (DECSCUSR).
    pub shape: CursorShape,
}

/// The cursor shapes a program can ask for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CursorShape {
    /// Whatever the user prefers: the program hasn't asked, or asked for
    /// the default.
    #[default]
    Default,
    /// A block covering the cell.
    Block,
    /// A line under the cell.
    Underline,
    /// A bar at the cell's left edge.
    Bar,
}

/// How many bytes of replies to queries may wait to be taken. One read of
/// a program's output can hold far more queries than this, each answered
/// with several times its own length.
const MAX_REPLIES: usize = 64 * 1024;

/// The longest command a block keeps, in bytes: clackatoo's shell
/// integration says none longer, and none longer is offered to run again,
/// so a longer one only costs memory, for as long as the block is kept.
const LONGEST_COMMAND: usize = 1024;

/// The longest working directory taken from OSC 7, in bytes: Linux's
/// `PATH_MAX`.
const LONGEST_PATH: usize = 4096;

/// How many titles a program may push (`CSI 22 t`) to pop again later
/// (`CSI 23 t`), as xterm keeps. Pushing more drops the oldest.
const MOST_TITLES: usize = 10;

/// The longest title kept on that stack, in bytes. A title can be as long
/// as an OSC string, and a stack of them would hold ten times that.
const LONGEST_PUSHED_TITLE: usize = 4096;

/// A terminal emulator, minus everything that touches the outside world.
///
/// Bytes from the program go in through [`advance`](Self::advance). The
/// screen they produce comes out through [`grid`](Self::grid) and
/// [`cursor`](Self::cursor). Some sequences are queries the terminal must
/// answer, such as "where is the cursor?". Their answers collect in a buffer
/// that the owner drains with [`take_replies`](Self::take_replies) and writes
/// back to the program.
#[derive(Debug)]
pub struct Terminal {
    parser: Parser,
    emulator: Emulator,
}

impl Terminal {
    /// A blank terminal of the given size.
    #[must_use]
    pub fn new(size: Size, config: &Config) -> Self {
        Self {
            parser: Parser::new(),
            emulator: Emulator::new(size, config),
        }
    }

    /// Processes output from the program.
    ///
    /// Splitting a stream into calls never changes the result. An escape
    /// sequence or UTF-8 character cut in half is completed by the next call.
    ///
    /// An image the program sends needs an owner to keep it (see
    /// [`advance_until_waiting`](Self::advance_until_waiting)): here, it's
    /// as if it couldn't be.
    pub fn advance(&mut self, mut bytes: &[u8]) {
        loop {
            let used = self.parser.advance(&mut self.emulator, bytes);
            bytes = &bytes[used..];
            if !self.emulator.graphics.waiting() {
                break;
            }
            self.give_up_image();
        }
    }

    /// Processes output from the program, as [`advance`](Self::advance)
    /// does, until an image's data is all here: then it stops, and says
    /// how many bytes it took. The owner keeps the image
    /// ([`take_image_requests`](Self::take_image_requests)), says what
    /// came of it ([`image_kept`](Self::image_kept)), and gives the
    /// terminal the rest. Nothing more is taken while it waits.
    pub fn advance_until_waiting(&mut self, bytes: &[u8]) -> usize {
        if self.emulator.graphics.waiting() {
            return 0;
        }
        self.parser.advance(&mut self.emulator, bytes)
    }

    /// Whether it waits for an image to be kept.
    #[must_use]
    pub fn waiting_for_image(&self) -> bool {
        self.emulator.graphics.waiting()
    }

    /// The images to be kept, for the owner to keep and say what came of
    /// each with [`image_kept`](Self::image_kept).
    pub fn take_image_requests(&mut self) -> Vec<ImageRequest> {
        self.emulator.graphics.take_requests()
    }

    /// What came of keeping the image `token` names: it's shown, and
    /// what's waited for it goes on.
    ///
    /// Says whether it was the image waited for: if it wasn't, as one
    /// given up on, the terminal holds nothing of it, and the owner lets
    /// it go.
    pub fn image_kept(&mut self, token: u64, result: Result<ImageKept, ImageError>) -> bool {
        self.emulator.image_kept(token, result)
    }

    /// The bytes of images being sent that it holds now: kitty's being
    /// sent in parts, a sixel image's, an iTerm2 image's.
    #[must_use]
    pub fn sending_images(&self) -> usize {
        self.emulator.graphics.sending() + self.parser.image_osc_len()
    }

    /// Whether an image is being sent in parts, or a sixel image's data
    /// collected: one that never ends is to be given up on
    /// ([`abandon_sending`](Self::abandon_sending)).
    #[must_use]
    pub fn sending_parts(&self) -> bool {
        self.emulator.graphics.sending_parts() || self.parser.image_osc_len() > 0
    }

    /// Gives up on the image being sent, letting go of what it holds.
    pub fn abandon_sending(&mut self) {
        self.emulator.graphics.abandon();
        self.parser.abandon_image_osc();
    }

    /// Lets an image being sent take no more than `bytes` now, as the
    /// images all terminals hold leave room for.
    pub fn set_upload_room(&mut self, bytes: usize) {
        self.emulator.graphics.set_room(bytes);
        self.parser.set_image_osc_room(bytes);
    }

    /// Gives up waiting for an image to be kept, as if it couldn't be.
    pub fn give_up_image(&mut self) {
        self.emulator.graphics.take_requests();
        if let Some(token) = self.emulator.graphics.waiting_token() {
            self.emulator.image_kept(token, Err(ImageError::Busy));
        }
    }

    /// The images it no longer holds, for the owner to let go of.
    pub fn take_released_images(&mut self) -> Vec<ImageKey> {
        self.emulator.graphics.take_released()
    }

    /// Lets it hold no more than `bytes` of images, and no image bigger.
    pub fn set_image_limit(&mut self, bytes: u64) {
        self.emulator.graphics.set_limit(bytes);
        // An iTerm2 image's string, base64 and its arguments.
        let most = usize::try_from(bytes.div_ceil(3) * 4).unwrap_or(usize::MAX);
        self.parser.set_image_osc_len(most.saturating_add(4096));
    }

    /// The size of the part of its image each picture shows, by number.
    pub(crate) fn picture_sizes(&self) -> std::collections::HashMap<u32, [u32; 2]> {
        self.emulator.picture_sizes()
    }

    /// Whether it shows or keeps any image.
    #[must_use]
    pub fn has_images(&self) -> bool {
        self.emulator.graphics.has_images()
    }

    /// Whether `image` is placed, but only on lines whose pictures may not
    /// be shown to a client that has lines with any of the hints `hidden`
    /// masked (see [`crate::graphics::may_show`]): one the client isn't
    /// sent.
    #[must_use]
    pub fn image_hidden(&self, image: ImageKey, hidden: crate::Hints) -> bool {
        self.emulator.image_hidden(image, hidden)
    }

    /// The hints of the virtual placement `virtual_`: those of the regions
    /// it was made in, or its placeholders were printed in.
    #[must_use]
    pub fn virtual_hints(&self, virtual_: &Virtual) -> crate::Hints {
        self.emulator.virtual_hints(virtual_)
    }

    /// The images its screen shows, and those its placeholders may.
    #[must_use]
    pub fn shown_images(&self) -> std::collections::HashSet<ImageKey> {
        self.emulator.shown_images()
    }

    /// The picture numbered `number` (see [`crate::graphics`]).
    #[must_use]
    pub fn picture(&self, number: u32) -> Option<&Picture> {
        self.emulator.graphics.picture(number)
    }

    /// The virtual placements its Unicode placeholders show.
    pub fn virtuals(&self) -> impl Iterator<Item = &Virtual> + '_ {
        self.emulator.graphics.virtuals()
    }

    /// Its images, as plain data (see [`GraphicsSnapshot`]).
    #[must_use]
    pub fn graphics_snapshot(&self) -> GraphicsSnapshot {
        self.emulator.graphics_snapshot()
    }

    /// Takes the images `snapshot` holds, after the rest of the terminal
    /// is restored.
    pub fn restore_graphics(&mut self, snapshot: &GraphicsSnapshot) {
        self.emulator.restore_graphics(snapshot);
    }

    /// Whether a program cleared the history (`CSI 3J`, which `clear` sends)
    /// since the last call, freeing the memory it held.
    pub fn take_history_cleared(&mut self) -> bool {
        std::mem::take(&mut self.emulator.history_cleared)
    }

    /// Takes the block whose command the shell last said ended (OSC 133
    /// `D`) since the last call, and its exit status if the shell said:
    /// by the marks alone, which any program can print. Kept for its
    /// taker even when the shell's next prompt, in the same read, leaves
    /// no row marked with the block.
    pub fn take_ended(&mut self) -> Option<(u32, Option<i32>)> {
        self.emulator.ended.take()
    }

    /// Takes the replies to queries seen so far. These must be written back to
    /// the program, which may be waiting for them.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.emulator.replies)
    }

    /// Takes what the program asked of the clipboard (OSC 52) since the
    /// last call, oldest first. Each [`ClipboardRequest::Read`] must be
    /// answered, with [`ClipboardRead::answer`](crate::ClipboardRead::answer) written back to the
    /// program, which may be waiting for it.
    pub fn take_clipboard(&mut self) -> Vec<ClipboardRequest> {
        self.emulator.clipboard.take()
    }

    /// Takes the notification the program last asked to be shown, or to
    /// be taken back, since the last call (see [`crate::notify`]). How
    /// often one is shown is the caller's to bound.
    pub fn take_notice(&mut self) -> Option<Notice> {
        self.emulator.notifications.take()
    }

    /// The terminal's size.
    #[must_use]
    pub fn size(&self) -> Size {
        self.emulator.size
    }

    /// Changes the terminal's size. Lines on the primary screen are rewrapped
    /// to the new width, and the cursor stays on the same character; the
    /// alternate screen is cropped or extended, since the programs that use
    /// it redraw after a resize. A program that asked to be told (mode
    /// 2048) is.
    pub fn resize(&mut self, size: Size) {
        self.resize_to(size, self.emulator.cell_pixels);
    }

    /// Says how big a cell is drawn, in pixels, as `[width, height]`, or
    /// that nobody has said, with `None`: the size in pixels programs are
    /// told of (mode 2048, `CSI 14 t`, `CSI 16 t`) is the cells' times
    /// this. A program that asked to be told of resizes is told of a
    /// change.
    pub fn set_cell_pixels(&mut self, cell: Option<[u16; 2]>) {
        self.resize_to(self.emulator.size, cell);
    }

    /// [`resize`](Self::resize) and
    /// [`set_cell_pixels`](Self::set_cell_pixels) at once: a program that
    /// asked to be told of resizes is told once.
    pub fn resize_to(&mut self, size: Size, cell: Option<[u16; 2]>) {
        let cell = cell.filter(|&[width, height]| width > 0 && height > 0);
        if size == self.emulator.size && cell == self.emulator.cell_pixels {
            return;
        }
        self.emulator.cell_pixels = cell;
        self.emulator.resize(size);
        if self.emulator.modes.contains(Modes::RESIZE_REPORTS) {
            self.emulator.report_size();
        }
    }

    /// The size of the text area in pixels, as `[width, height]`, if a
    /// cell's size in pixels was said (see
    /// [`set_cell_pixels`](Self::set_cell_pixels)).
    #[must_use]
    pub fn pixels(&self) -> Option<[u16; 2]> {
        self.emulator.text_pixels()
    }

    /// The primary screen, whose history the terminal keeps, whichever
    /// screen is showing.
    #[must_use]
    pub fn primary(&self) -> &Grid {
        &self.emulator.primary
    }

    /// Whether scrollback holds rows waiting for
    /// [`compact_scrollback`](Self::compact_scrollback). Only ever true with
    /// [`Compaction::Lazy`].
    #[must_use]
    pub fn scrollback_needs_compaction(&self) -> bool {
        self.emulator.primary.scrollback_needs_compaction()
    }

    /// Adds the screen lines that changed since the last call to `into`.
    /// A viewer that redraws those lines stays in sync with the screen.
    pub fn take_damage(&mut self, into: &mut Damage) {
        self.emulator.grid_mut().take_damage(into);
    }

    /// Compacts the scrollback rows that aren't yet, to save memory. With
    /// [`Compaction::Lazy`], call this once output pauses. It takes well
    /// under a millisecond per thousand rows.
    pub fn compact_scrollback(&mut self) {
        self.emulator.primary.compact_scrollback();
        if self.emulator.graphics.has_pictures() && self.emulator.sweep_due() {
            self.emulator.sweep_pictures();
        }
    }

    /// The screen being shown: the primary screen, or the alternate screen
    /// that full-screen programs switch to.
    #[must_use]
    pub fn grid(&self) -> &Grid {
        self.emulator.grid()
    }

    pub(crate) fn primary_mut(&mut self) -> &mut Grid {
        &mut self.emulator.primary
    }

    pub(crate) fn shown_grid_mut(&mut self) -> &mut Grid {
        self.emulator.grid_mut()
    }

    pub(crate) fn emulator_regions(&self) -> &Regions {
        &self.emulator.regions
    }

    pub(crate) fn emulator_regions_mut(&mut self) -> &mut Regions {
        &mut self.emulator.regions
    }

    /// Whether the alternate screen is showing.
    #[must_use]
    pub fn is_alternate_screen(&self) -> bool {
        self.emulator.screen == Screen::Alternate
    }

    /// Whether the program is part way through drawing its screen, and
    /// asked for none of it to be shown until it's done (mode 2026,
    /// synchronized output). The owner holds back what it shows while
    /// this is so, for long enough to look like one frame, but not for a
    /// program that never says it's done.
    #[must_use]
    pub fn is_synchronized(&self) -> bool {
        self.emulator.modes.contains(Modes::SYNCHRONIZED)
    }

    /// The cursor.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        Cursor {
            line: self.emulator.cursor.line,
            col: self.emulator.cursor.col,
            visible: self.emulator.modes.contains(Modes::CURSOR_VISIBLE),
            shape: self.emulator.cursor_shape,
        }
    }

    /// How the program wants keys and pastes encoded.
    #[must_use]
    pub fn input_modes(&self) -> InputModes {
        let modes = self.emulator.modes;
        InputModes {
            app_cursor_keys: modes.contains(Modes::APP_CURSOR_KEYS),
            app_keypad: modes.contains(Modes::APP_KEYPAD),
            bracketed_paste: modes.contains(Modes::BRACKETED_PASTE),
            mouse_tracking: self.emulator.mouse_tracking,
            mouse_encoding: self.emulator.mouse_encoding,
            focus_events: modes.contains(Modes::FOCUS_EVENTS),
        }
    }

    /// The rules its cells are laid out by, which whatever rebuilds its
    /// rows from their text must use too (see [`Widths`]).
    #[must_use]
    pub fn widths(&self) -> Widths {
        self.emulator.config.widths
    }

    /// The kitty keyboard protocol's flags in effect: how the program on
    /// the screen shown wants keys encoded, beyond
    /// [`input_modes`](Self::input_modes) (see [`crate::keyboard`]). 0
    /// means xterm's encoding.
    #[must_use]
    pub fn keyboard(&self) -> u8 {
        self.emulator.keyboard().flags()
    }

    /// The mouse pointer's shape the program on the screen shown asked
    /// for (OSC 22), until it ends: `None` for the window's own.
    #[must_use]
    pub fn pointer(&self) -> Option<Pointer> {
        match self.emulator.screen {
            Screen::Primary => self.emulator.pointer_primary.shown(),
            Screen::Alternate => self.emulator.pointer_alternate.shown(),
        }
    }

    /// The window title the program last set, or empty.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.emulator.title
    }

    /// Says what colours the terminal is drawn in, for the programs that
    /// ask (see [`Palette`]), or that nobody has said, with `None`: then
    /// they're told nothing. A reset keeps them: they're the window's.
    pub fn set_palette(&mut self, palette: Option<Palette>) {
        self.emulator.palette = palette;
    }

    /// How far the program's task has got, as it last said (OSC 9;4),
    /// until it says it's done or the shell shows its next prompt.
    #[must_use]
    pub fn progress(&self) -> Option<TaskProgress> {
        self.emulator.progress
    }

    /// The working directory the shell last reported (OSC 7), as a path.
    #[must_use]
    pub fn cwd(&self) -> Option<&str> {
        self.emulator.cwd.as_deref()
    }

    /// What the link `id` points to, if any row may still be part of it.
    /// Cells name their link in their [`Style`].
    #[must_use]
    pub fn link(&self, id: LinkId) -> Option<&Hyperlink> {
        self.emulator.links.get(id)
    }

    /// What the shell is doing, as its OSC 133 marks say.
    #[must_use]
    pub fn shell_state(&self) -> blocks::ShellState {
        self.emulator.blocks.state()
    }

    /// What the shell said about block `block`, if it is still kept.
    pub(crate) fn block_entry(&self, block: u32) -> Option<&blocks::Entry> {
        self.emulator.blocks.get(block)
    }

    /// The blocks the shell has marked.
    pub(crate) fn emulator_blocks(&self) -> &Blocks {
        &self.emulator.blocks
    }

    /// The terminal's whole state, to carry into another process, but its
    /// links. See [`Snapshot`] for what isn't kept.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot_with_links().0
    }

    /// The terminal's whole state, to carry into another process, with
    /// its links. See [`Snapshot`] for what isn't kept.
    #[must_use]
    pub fn snapshot_with_links(&self) -> (Snapshot, LinksSnapshot) {
        let emulator = &self.emulator;
        let (primary, history) = emulator.primary.lines();
        let (alternate, alternate_links) = match &emulator.alternate {
            Some(grid) => {
                let captured = grid.lines().0;
                (Some(captured.lines), captured.links)
            }
            None => (None, Vec::new()),
        };
        let links = LinksSnapshot::capture(
            &emulator.links,
            primary.links,
            history.links,
            alternate_links,
            [
                emulator.cursor.pen.link,
                emulator.saved_primary.cursor.pen.link,
                emulator.saved_alternate.cursor.pen.link,
            ],
        );
        let snapshot = Snapshot {
            cols: emulator.size.cols(),
            rows: emulator.size.rows(),
            title: emulator.title.clone(),
            primary: primary.lines,
            history: history.lines,
            history_end: emulator.primary.scrollback_end(),
            alternate,
            alternate_shown: emulator.screen == Screen::Alternate,
            cursor: emulator.cursor.into(),
            saved_primary: emulator.saved_primary.into(),
            saved_alternate: emulator.saved_alternate.into(),
            modes: crate::snapshot::Modes::capture(
                emulator.modes,
                emulator.mouse_tracking,
                emulator.mouse_encoding,
            ),
            scroll_region: (
                position(emulator.scroll_region.start),
                position(emulator.scroll_region.end),
            ),
            tab_stops: emulator.tabs.columns().map(position).collect(),
            cursor_shape: emulator.cursor_shape.into(),
            last_printed: emulator.last_printed,
        };
        (snapshot, links)
    }

    /// The terminal a [`snapshot`](Self::snapshot) was taken of, without
    /// links, with `config`'s settings: history beyond its line limit is
    /// dropped.
    ///
    /// # Errors
    ///
    /// If the snapshot is inconsistent, so can't have come from a terminal.
    pub fn restore(snapshot: &Snapshot, config: &Config) -> Result<Self, RestoreError> {
        Self::restore_with_links(snapshot, &LinksSnapshot::default(), config)
    }

    /// The terminal a [`snapshot_with_links`](Self::snapshot_with_links)
    /// was taken of, links and all, with `config`'s settings.
    ///
    /// # Errors
    ///
    /// If the snapshots are inconsistent, so can't have come from a
    /// terminal.
    pub fn restore_with_links(
        snapshot: &Snapshot,
        links: &LinksSnapshot,
        config: &Config,
    ) -> Result<Self, RestoreError> {
        let size = Size::new(snapshot.cols, snapshot.rows)
            .map_err(|_| RestoreError("its size is out of range"))?;
        let (width, height) = (usize::from(size.cols()), usize::from(size.rows()));
        let fits = |lines: &[crate::snapshot::Line]| lines.len() == height;
        if !fits(&snapshot.primary)
            || snapshot
                .alternate
                .as_deref()
                .is_some_and(|lines| !fits(lines))
        {
            return Err(RestoreError("a screen has the wrong number of lines"));
        }
        if snapshot.alternate_shown && snapshot.alternate.is_none() {
            return Err(RestoreError("the alternate screen is shown but missing"));
        }
        let region = usize::from(snapshot.scroll_region.0)..usize::from(snapshot.scroll_region.1);
        if region.is_empty() || region.end > height {
            return Err(RestoreError("the scroll region is off the screen"));
        }
        if usize::from(snapshot.cursor.line) >= height || usize::from(snapshot.cursor.col) >= width
        {
            return Err(RestoreError("the cursor is off the screen"));
        }

        let (table, pens) = links.table()?;
        let mut history_links = LineLinks::new(&links.history, &table);
        let history = Scrollback::restored(
            config.scrollback_lines,
            config.scrollback_compaction,
            (snapshot.history.iter().enumerate()).map(|(number, line)| {
                line.to_linked_row(&history_links.of(number)?, config.widths)
            }),
            snapshot.history_end,
            config.widths,
        )?;
        history_links.finish()?;
        let mut emulator = Emulator::new(size, config);
        let primary_links = LineLinks::new(&links.primary, &table);
        emulator.primary = Grid::restored(
            width,
            &snapshot.primary,
            primary_links,
            history,
            config.widths,
        )?;
        let alternate_links = LineLinks::new(&links.alternate, &table);
        emulator.alternate = if let Some(screen) = snapshot.alternate.as_deref() {
            let history = Scrollback::new(0, Compaction::Off);
            Some(Grid::restored(
                width,
                screen,
                alternate_links,
                history,
                config.widths,
            )?)
        } else {
            alternate_links.finish()?;
            None
        };
        if snapshot.alternate_shown {
            emulator.screen = Screen::Alternate;
        }
        emulator.cursor = snapshot.cursor.into();
        emulator.saved_primary = snapshot.saved_primary.into();
        emulator.saved_alternate = snapshot.saved_alternate.into();
        emulator.cursor.pen.link = pens[0];
        emulator.saved_primary.cursor.pen.link = pens[1];
        emulator.saved_alternate.cursor.pen.link = pens[2];
        emulator.links = table;
        (
            emulator.modes,
            emulator.mouse_tracking,
            emulator.mouse_encoding,
        ) = snapshot.modes.restore();
        emulator.scroll_region = region;
        emulator.tabs = TabStops::at(
            width,
            snapshot.tab_stops.iter().map(|&col| usize::from(col)),
        );
        emulator.title.clone_from(&snapshot.title);
        emulator.cursor_shape = snapshot.cursor_shape.into();
        emulator.last_printed = snapshot.last_printed;
        Ok(Self {
            parser: Parser::new(),
            emulator,
        })
    }

    /// What the shell marked, to carry into another process with a
    /// [`snapshot`](Self::snapshot).
    #[must_use]
    pub fn blocks_snapshot(&self) -> BlocksSnapshot {
        let emulator = &self.emulator;
        BlocksSnapshot::capture(
            emulator.cwd.as_deref(),
            emulator.primary.marks(),
            &emulator.blocks,
        )
    }

    /// Puts back what the shell marked, from a
    /// [`blocks_snapshot`](Self::blocks_snapshot) taken with the
    /// [`snapshot`](Self::snapshot) this terminal was restored from.
    pub fn restore_blocks(&mut self, snapshot: &BlocksSnapshot) {
        let (marks, blocks) = snapshot.restore();
        let emulator = &mut self.emulator;
        emulator.primary.set_marks(&marks);
        // Snapshots don't carry the count, which counts only this
        // process's renumberings.
        emulator.blocks = blocks.renumbered_as(emulator.blocks.renumbered());
        emulator.cwd = snapshot.cwd.as_deref().map(Arc::from);
    }

    /// The left and right margins a program set (DECSLRM), as the first
    /// column and the one past the last, if they narrow the screen.
    #[must_use]
    pub fn margins(&self) -> Option<(u16, u16)> {
        let emulator = &self.emulator;
        emulator.margins_on().then(|| {
            let column = |col: usize| u16::try_from(col).unwrap_or(u16::MAX);
            (column(emulator.margins.start), column(emulator.margins.end))
        })
    }

    /// Puts back margins [`margins`](Self::margins) gave, as a terminal
    /// restored with DECLRMM set had them; none that don't fit.
    pub fn restore_margins(&mut self, (start, end): (u16, u16)) {
        let emulator = &mut self.emulator;
        let (start, end) = (usize::from(start), usize::from(end));
        // As DECSLRM sets them: at least two columns apart.
        if emulator.modes.contains(Modes::LEFT_RIGHT_MARGINS)
            && start + 1 < end
            && end <= emulator.width()
        {
            emulator.margins = start..end;
        }
    }

    /// What the program asked of the terminal that a
    /// [`snapshot`](Self::snapshot) doesn't keep, to carry into another
    /// process with it: the keyboard's flags, the titles it pushed and
    /// its progress.
    #[must_use]
    pub fn asked_snapshot(&self) -> AskedSnapshot {
        let emulator = &self.emulator;
        AskedSnapshot::capture(
            [&emulator.keyboard_primary, &emulator.keyboard_alternate],
            &emulator.titles,
            emulator.progress,
        )
    }

    /// Puts back what the program asked, from an
    /// [`asked_snapshot`](Self::asked_snapshot) taken with the
    /// [`snapshot`](Self::snapshot) this terminal was restored from.
    pub fn restore_asked(&mut self, snapshot: &AskedSnapshot) {
        let ([primary, alternate], titles, progress) = snapshot.restore();
        let emulator = &mut self.emulator;
        emulator.progress = progress;
        emulator.keyboard_primary = primary;
        emulator.keyboard_alternate = alternate;
        let newest = titles.len().saturating_sub(MOST_TITLES);
        emulator.titles = titles[newest..]
            .iter()
            .map(|title| shortened(title))
            .collect();
    }

    /// The number of links the terminal holds.
    #[cfg(test)]
    pub(crate) fn link_count(&self) -> usize {
        self.emulator.link_count()
    }

    /// Says the shell's integration adds `secret` to the `C` marks it
    /// makes, as `clackatoo=SECRET`, or that it adds none. A command
    /// marked with it is the shell's for certain (see
    /// [`trusted_running_command`](Self::trusted_running_command)): no
    /// program the shell runs is told the secret. It's compared, and
    /// never kept anywhere else.
    pub fn expect_secret(&mut self, secret: Option<&str>) {
        self.emulator.secret = secret.map(|secret| secret.as_bytes().into());
    }

    /// Takes the title and working directory as they are now as the
    /// owner's: from now on what the program prints can't change them
    /// (OSC 0, 2 and 7 are ignored), nor a reset clear them. For a
    /// terminal whose owner writes into it what a program printed
    /// elsewhere, which mustn't name the window or say where paths are.
    pub fn keep_names(&mut self) {
        self.emulator.names_kept = true;
    }

    /// Takes block `block`'s command to be the shell's, as a
    /// [`trusted_block`](Self::trusted_block) of the terminal this one
    /// was restored from said.
    pub fn trust_block(&mut self, block: u32) {
        self.emulator.blocks.trust(block);
    }

    /// The printing state the screen doesn't show: the cursor's position and
    /// pending wrap, and the character REP would repeat.
    #[cfg(test)]
    pub(crate) fn emulator_state(&self) -> (usize, usize, bool, Option<char>) {
        let cursor = &self.emulator.cursor;
        (
            cursor.line,
            cursor.col,
            cursor.pending_wrap,
            self.emulator.last_printed,
        )
    }

    /// Whether no escape sequence or character is cut off part way.
    #[cfg(test)]
    pub(crate) fn parser_is_idle(&self) -> bool {
        self.parser.is_idle()
    }

    /// Makes this terminal print through the simple per-character path.
    #[cfg(test)]
    pub(crate) fn use_simple_print(&mut self) {
        self.emulator.simple_print = true;
    }

    /// Sets the left and right margins to `margins` whatever they are, as
    /// no sequence may: for checking what printing does with ones too
    /// narrow.
    #[cfg(test)]
    pub(crate) fn force_margins(&mut self, margins: std::ops::Range<usize>) {
        self.emulator.modes.insert(Modes::LEFT_RIGHT_MARGINS);
        self.emulator.margins = margins;
    }

    /// Panics if any internal invariant is broken.
    #[cfg(test)]
    pub(crate) fn assert_invariants(&self) {
        let emulator = &self.emulator;
        for grid in std::iter::once(&emulator.primary).chain(&emulator.alternate) {
            assert_eq!(grid.rows().len(), emulator.height());
            grid.assert_scrollback_invariants();
            // History keeps the width each row had when it scrolled off.
            for row in grid.scrollback() {
                row.assert_invariants();
            }
            for row in grid.rows() {
                assert_eq!(row.width(), emulator.width());
                row.assert_invariants();
            }
        }
        assert!(
            emulator
                .alternate
                .as_ref()
                .is_none_or(|grid| grid.scrollback_len() == 0),
            "the alternate screen has scrollback"
        );
        assert!(
            emulator.screen == Screen::Primary || emulator.alternate.is_some(),
            "the alternate screen is shown but doesn't exist"
        );
        assert!(
            emulator
                .alternate
                .as_ref()
                .is_none_or(|grid| grid.marks().next().is_none()),
            "the alternate screen has marks"
        );
        assert!(
            emulator.cursor.line < emulator.height(),
            "cursor below the screen"
        );
        assert!(
            emulator.cursor.col < emulator.width(),
            "cursor right of the screen"
        );
        let region = &emulator.scroll_region;
        assert!(
            region.start < region.end && region.end <= emulator.height(),
            "bad scroll region {region:?}"
        );
        let margins = &emulator.margins;
        assert!(
            margins.start < margins.end && margins.end <= emulator.width(),
            "bad margins {margins:?}"
        );
    }

    /// The visible screen as text: one line per row, trailing spaces trimmed,
    /// and trailing empty lines omitted.
    #[must_use]
    pub fn screen_text(&self) -> String {
        let mut text = String::new();
        for row in self.grid().rows() {
            text.push_str(&row.text());
            text.push('\n');
        }
        text.truncate(text.trim_end_matches('\n').len());
        text
    }
}

bitflags! {
    /// Terminal modes that change how output is processed.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Modes: u16 {
        /// IRM: printing shifts the rest of the line right instead of overwriting.
        const INSERT = 1 << 0;
        /// LNM: line feed also returns to the first column.
        const LINEFEED_NEWLINE = 1 << 1;
        /// DECOM: cursor addressing is relative to the scroll region.
        const ORIGIN = 1 << 2;
        /// DECAWM: text reaching the right edge continues on the next line.
        const AUTOWRAP = 1 << 3;
        /// DECTCEM: the cursor is drawn.
        const CURSOR_VISIBLE = 1 << 4;
        /// DECCKM: arrow keys send application sequences (`ESC O A`).
        const APP_CURSOR_KEYS = 1 << 5;
        /// DECKPAM: the keypad sends application sequences.
        const APP_KEYPAD = 1 << 6;
        /// Pasted text is wrapped in `ESC [200~` and `ESC [201~`.
        const BRACKETED_PASTE = 1 << 7;
        /// Mode 1004: the program is told when the terminal gains or loses
        /// focus.
        const FOCUS_EVENTS = 1 << 8;
        /// Mode 2026: the program is drawing a frame, to be shown whole
        /// once it resets this.
        const SYNCHRONIZED = 1 << 9;
        /// Mode 2027: characters are printed by grapheme clusters, as
        /// Unicode's segmentation has them, a cluster to a cell.
        const GRAPHEMES = 1 << 10;
        /// Mode 2048: the program is told of each resize, in-band, as
        /// `CSI 48 ; rows ; cols ; height ; width t`.
        const RESIZE_REPORTS = 1 << 11;
        /// DECLRMM (mode 69): left and right margins can be set (DECSLRM,
        /// `CSI l ; r s`), and `CSI s` sets them rather than saving the
        /// cursor.
        const LEFT_RIGHT_MARGINS = 1 << 12;
    }
}

/// Modes that change how a client should encode keys and pastes for the
/// program. The emulator only records them; the client does the encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(clippy::struct_excessive_bools, reason = "each mode is on or off")]
pub struct InputModes {
    /// DECCKM: arrow keys send `ESC O A` rather than `ESC [ A`.
    pub app_cursor_keys: bool,
    /// DECKPAM: the numeric keypad sends application sequences.
    pub app_keypad: bool,
    /// Pasted text must be wrapped in `ESC [200~` and `ESC [201~`, so the
    /// program can tell it from typing.
    pub bracketed_paste: bool,
    /// Which mouse events the program wants: see [`mouse`](crate::mouse).
    pub mouse_tracking: MouseTracking,
    /// How it wants them written.
    pub mouse_encoding: MouseEncoding,
    /// Mode 1004: the program wants to know when the terminal gains or
    /// loses focus.
    pub focus_events: bool,
}

/// Why the alternate screen can be unwrapped while it is shown.
const ALTERNATE_EXISTS: &str = "the alternate screen exists once it has been shown";

/// U+200D ZERO WIDTH JOINER, which glues emoji like 👩‍💻 into one grapheme.
pub(crate) const ZWJ: char = '\u{200D}';

/// The column of the character just before the cursor on its line, if
/// there is one. That's the cursor's own cell during a pending wrap, since
/// the cursor hasn't moved off the character it just printed.
fn previous_cell(row: &Row, cursor: &CursorState) -> Option<usize> {
    let col = if cursor.pending_wrap {
        cursor.col
    } else {
        cursor.col.checked_sub(1)?
    };
    if row.cells()[col].is_wide_spacer() {
        Some(col - 1)
    } else {
        Some(col)
    }
}

/// Text the printer walks: valid UTF-8 without control characters.
trait Printable {
    /// Whether every byte is known to be ASCII, which skips checking.
    const ASCII: bool;

    fn bytes(&self) -> &[u8];

    /// The character starting at byte `at`, a character boundary, and its
    /// length in bytes.
    fn char_at(&self, at: usize) -> (char, usize);
}

/// Printable ASCII, as the parser hands it over.
struct Ascii<'a>(&'a [u8]);

impl Printable for Ascii<'_> {
    const ASCII: bool = true;

    fn bytes(&self) -> &[u8] {
        self.0
    }

    fn char_at(&self, at: usize) -> (char, usize) {
        (char::from(self.0[at]), 1)
    }
}

impl Printable for &str {
    const ASCII: bool = false;

    fn bytes(&self) -> &[u8] {
        self.as_bytes()
    }

    fn char_at(&self, at: usize) -> (char, usize) {
        // Decoding through `str` trusts that it is valid, so it is cheaper
        // than decoding raw bytes, which would check them again.
        let ch = self[at..].chars().next().expect("at is within the text");
        (ch, ch.len_utf8())
    }
}

/// Whether a character may join the cell at `col`, to be looked at a
/// character at a time: as one after a ZWJ does, or by `clusters`, as one
/// after a character past ASCII may (see [`joins_cell`]).
fn may_join(row: &Row, col: usize, clusters: bool) -> bool {
    ends_in_zwj(row, col) || (clusters && !row.last_char(col).is_ascii())
}

/// Whether `ch`, a character that takes columns, joins the cell at `col`
/// rather than starting one: after a ZWJ, it does; by `clusters`, as mode
/// 2027 prints, it does if Unicode says it goes on with the cell's
/// cluster.
fn joins_cell(row: &Row, col: usize, ch: char, clusters: bool) -> bool {
    ends_in_zwj(row, col)
        || (clusters
            && width::continues_cluster(
                row.cells()[col].ch(),
                row.grapheme(col).unwrap_or_default(),
                ch,
            ))
}

/// Whether the cell at `col` ends in a ZWJ, so the next character joins it.
fn ends_in_zwj(row: &Row, col: usize) -> bool {
    // A ZWJ is zero-width, so it is only ever stored as a cell's extra
    // codepoint. Checking the flag first skips the grapheme lookup for almost
    // every cell.
    row.cells()[col].has_grapheme() && row.last_char(col) == ZWJ
}

/// Moves the cursor past `cells` cells just printed. At the right edge it
/// stays on the last column, waiting to wrap if autowrap is on.
fn advance(cursor: &mut CursorState, cells: usize, width: usize, autowrap: bool) {
    if cursor.col + cells < width {
        cursor.col += cells;
    } else {
        cursor.col = width - 1;
        cursor.pending_wrap = autowrap;
    }
}

impl Default for Modes {
    fn default() -> Self {
        Self::AUTOWRAP | Self::CURSOR_VISIBLE
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Screen {
    #[default]
    Primary,
    Alternate,
}

/// The cursor's full state: position, plus what it will draw with.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CursorState {
    pub(crate) line: usize,
    pub(crate) col: usize,
    /// Set after printing in the last column. The cursor stays on the
    /// character just printed, and the line only wraps when the next one
    /// arrives. Any cursor movement cancels it. Without this, a line that
    /// exactly fills the width would wrap early and push everything down.
    pub(crate) pending_wrap: bool,
    /// The style printed characters get, set by SGR.
    pub(crate) pen: Style,
    pub(crate) charsets: Charsets,
}

/// What DECSC (save cursor) saves.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SavedCursor {
    pub(crate) cursor: CursorState,
    pub(crate) origin: bool,
}

/// All of a terminal's state apart from the parser.
///
/// Split from [`Terminal`] so the parser can borrow it mutably as its
/// [`Perform`](crate::parser::Perform) handler. The handler lives in `dispatch.rs` and turns
/// escape sequences into calls to the operations here.
#[derive(Debug)]
pub(crate) struct Emulator {
    size: Size,
    config: Config,
    pub(crate) primary: Grid,
    /// Made the first time a program switches to it. Most terminals only
    /// ever run a shell, which never does.
    alternate: Option<Grid>,
    pub(crate) screen: Screen,
    pub(crate) cursor: CursorState,
    /// DECSC's saved cursor. Each screen has its own, as in xterm.
    saved_primary: SavedCursor,
    saved_alternate: SavedCursor,
    pub(crate) modes: Modes,
    pub(crate) mouse_tracking: MouseTracking,
    pub(crate) mouse_encoding: MouseEncoding,
    /// The lines that scroll, set by DECSTBM. The whole screen unless an app
    /// has narrowed it, as vim does to keep its status line still.
    scroll_region: Range<usize>,
    /// The columns between the left and right margins, set by DECSLRM
    /// while DECLRMM is: every column unless a program has narrowed them.
    margins: Range<usize>,
    tabs: TabStops,
    pub(crate) title: String,
    /// Titles pushed to be popped again (XTWINOPS 22 and 23), oldest
    /// first.
    titles: Vec<String>,
    /// The kitty keyboard protocol's flags programs pushed on each
    /// screen (see [`crate::keyboard`]).
    keyboard_primary: keyboard::Stack,
    keyboard_alternate: keyboard::Stack,
    /// The pointer shapes programs asked for on each screen (see
    /// [`crate::pointer`]).
    pointer_primary: pointer::Stack,
    pointer_alternate: pointer::Stack,
    /// How far the program's task has got, as it last said.
    progress: Option<TaskProgress>,
    /// The colours the terminal is drawn in, as a window said them.
    palette: Option<Palette>,
    /// How big a cell is drawn, in pixels, as a window said it.
    cell_pixels: Option<[u16; 2]>,
    /// The device control string being collected, if it's a request this
    /// terminal answers (see [`crate::requests`]).
    pub(crate) request: Option<Request>,
    /// Its images (see [`crate::graphics`]).
    pub(crate) graphics: Graphics,
    /// The working directory the shell last reported, shared with the
    /// blocks that ran in it.
    cwd: Option<Arc<str>>,
    /// The commands the shell marked (see [`crate::blocks`]).
    blocks: Blocks,
    /// The secret clackatoo's own shell integration adds to its marks in
    /// this terminal, if it has one (see [`Terminal::expect_secret`]).
    secret: Option<Box<[u8]>>,
    /// What the links cells are part of point to (see [`crate::links`]).
    links: Links,
    /// The cursor shape the program asked for.
    cursor_shape: CursorShape,
    pub(crate) replies: Vec<u8>,
    /// What the program asked of the clipboard, waiting to be taken.
    pub(crate) clipboard: clipboard::Requests,
    /// The notifications the program asked for (see [`crate::notify`]).
    pub(crate) notifications: Notifications,
    /// The last character printed, for REP (repeat).
    pub(crate) last_printed: Option<char>,
    /// History was cleared since [`Terminal::take_history_cleared`] last
    /// asked, freeing the memory it held.
    history_cleared: bool,
    /// The block whose command the shell last said ended, and how, until
    /// [`Terminal::take_ended`] takes it.
    ended: Option<(u32, Option<i32>)>,
    /// The title and working directory are the owner's, which what the
    /// program prints can't change (see [`Terminal::keep_names`]).
    pub(crate) names_kept: bool,
    /// The regions programs marked as private that are open (see
    /// [`crate::regions`]).
    pub(crate) regions: Regions,
    /// Prints everything through the simple per-character path, so tests can
    /// check the fast path against it.
    #[cfg(test)]
    pub(crate) simple_print: bool,
}

impl Emulator {
    fn new(size: Size, config: &Config) -> Self {
        let (width, height) = (usize::from(size.cols()), usize::from(size.rows()));
        let mut primary = Grid::new(
            width,
            height,
            config.scrollback_lines,
            config.scrollback_compaction,
        );
        primary.set_widths(config.widths);
        Self {
            size,
            config: config.clone(),
            primary,
            alternate: None,
            screen: Screen::Primary,
            cursor: CursorState::default(),
            saved_primary: SavedCursor::default(),
            saved_alternate: SavedCursor::default(),
            modes: Modes::default(),
            mouse_tracking: MouseTracking::Off,
            mouse_encoding: MouseEncoding::Default,
            scroll_region: 0..height,
            margins: 0..width,
            tabs: TabStops::new(width),
            title: String::new(),
            titles: Vec::new(),
            keyboard_primary: keyboard::Stack::default(),
            keyboard_alternate: keyboard::Stack::default(),
            pointer_primary: pointer::Stack::default(),
            pointer_alternate: pointer::Stack::default(),
            progress: None,
            palette: None,
            cell_pixels: None,
            request: None,
            graphics: Graphics::default(),
            cwd: None,
            blocks: Blocks::default(),
            secret: None,
            links: Links::default(),
            cursor_shape: CursorShape::Default,
            replies: Vec::new(),
            clipboard: clipboard::Requests::default(),
            notifications: Notifications::default(),
            last_printed: None,
            history_cleared: false,
            ended: None,
            names_kept: false,
            regions: Regions::default(),
            #[cfg(test)]
            simple_print: false,
        }
    }

    pub(crate) fn grid(&self) -> &Grid {
        match self.screen {
            Screen::Primary => &self.primary,
            Screen::Alternate => self.alternate.as_ref().expect(ALTERNATE_EXISTS),
        }
    }

    pub(crate) fn grid_mut(&mut self) -> &mut Grid {
        match self.screen {
            Screen::Primary => &mut self.primary,
            Screen::Alternate => self.alternate.as_mut().expect(ALTERNATE_EXISTS),
        }
    }

    /// Both screens' grids: the primary's, and the alternate's if made.
    pub(crate) fn grids(&self) -> (&Grid, Option<&Grid>) {
        (&self.primary, self.alternate.as_ref())
    }

    pub(crate) fn grids_mut(&mut self) -> (&mut Grid, Option<&mut Grid>) {
        (&mut self.primary, self.alternate.as_mut())
    }

    /// How big a window draws a cell, in pixels, if one said.
    pub(crate) fn cell_pixels(&self) -> Option<[u16; 2]> {
        self.cell_pixels
    }

    /// Whether printing past the last column wraps.
    pub(crate) fn autowraps(&self) -> bool {
        self.modes.contains(Modes::AUTOWRAP)
    }

    pub(crate) fn width(&self) -> usize {
        usize::from(self.size.cols())
    }

    pub(crate) fn height(&self) -> usize {
        usize::from(self.size.rows())
    }

    /// The style cleared cells get right now.
    fn blank(&self) -> Style {
        self.cursor.pen.erased()
    }

    // --- Printing ---------------------------------------------------------

    /// Prints text from the parser, which never contains control
    /// characters.
    pub(crate) fn print_text(&mut self, text: &str) {
        if self.prints_simply() {
            for ch in text.chars() {
                self.print(ch);
            }
        } else {
            self.print_printable(&text);
        }
    }

    /// Prints printable ASCII from the parser. Knowing that up front skips
    /// both UTF-8 decoding and checking each byte.
    pub(crate) fn print_ascii(&mut self, text: &[u8]) {
        if self.prints_simply() {
            for &byte in text {
                self.print(char::from(byte));
            }
        } else {
            self.print_printable(&Ascii(text));
        }
    }

    /// Whether to print a character at a time, through [`print`](Self::print).
    /// Insert mode is rare enough not to need the fast path, and so is
    /// printing in a region marked private, whose lines it marks.
    fn prints_simply(&self) -> bool {
        let simple = self.modes.contains(Modes::INSERT) || !self.regions.hinting.is_empty();
        #[cfg(test)]
        let simple = simple || self.simple_print;
        simple
    }

    /// Whether left and right margins narrow the screen: DECLRMM is set,
    /// and they're narrower than it.
    fn margins_on(&self) -> bool {
        self.modes.contains(Modes::LEFT_RIGHT_MARGINS) && self.margins != (0..self.width())
    }

    /// Whether the cursor is within the left and right margins, if they
    /// narrow the screen.
    fn within_margins(&self) -> bool {
        self.margins.contains(&self.cursor.col)
    }

    /// The column past the last that printing at the cursor fills before
    /// it wraps, and the column it wraps to: the right and left margins
    /// while the cursor is left of the right one, or else the screen's
    /// edges.
    fn print_span(&self) -> (usize, usize) {
        if self.margins_on() && self.cursor.col < self.margins.end {
            (self.margins.end, self.margins.start)
        } else {
            (self.width(), 0)
        }
    }

    /// Prints `text`, line by line.
    fn print_printable(&mut self, text: &impl Printable) {
        let mut at = 0;
        while at < text.bytes().len() {
            let (end, wrap) = self.print_on_line(text, at);
            at = end;
            if wrap {
                self.wrap();
                // The character that wraps was found to start a cell of its
                // own before the wrap, as the simple way finds it. Wrapped
                // to a left margin, there's a cell before it, past the
                // margin, that the fast way would look at again and might
                // join, so it's printed the simple way.
                if self.cursor.col > 0 {
                    let (ch, len) = text.char_at(at);
                    let ch = self.cursor.charsets.map(ch);
                    if let Some(cells @ 1..) = width::of(ch) {
                        self.print_cell(ch, cells == 2);
                    }
                    at += len;
                }
            }
        }
    }

    /// Prints as much of `text` as goes on the cursor's line: the hot path of
    /// the whole emulator. It means the same as [`print`](Self::print) on
    /// each character in turn, but looks the row and style up once per line,
    /// and writes runs of ASCII straight from the bytes. Insert mode must be
    /// off.
    ///
    /// Starts at byte `start` of `text`. Returns where it stopped, and
    /// whether the line must wrap before the rest.
    fn print_on_line<T: Printable>(&mut self, text: &T, start: usize) -> (usize, bool) {
        // Where text wraps, as the margins say, and the columns between
        // there and where it wraps to: never fewer than two, as margins are
        // set, but a character wider than that is dropped all the same, as
        // it would wrap for ever.
        let (edge, wraps_to) = self.print_span();
        let width = self.width().min(edge - wraps_to);
        let autowrap = self.modes.contains(Modes::AUTOWRAP);
        let Self {
            primary,
            alternate,
            screen,
            cursor,
            last_printed,
            config,
            modes,
            ..
        } = self;
        let widths = config.widths;
        let clusters = modes.contains(Modes::GRAPHEMES);
        // Whether `ch` joining the cell at `col` would widen it with no
        // room: then it doesn't join (see `widening_blocked`).
        let blocked =
            |row: &Row, col: usize, ch: char| col + 1 >= edge && row.would_widen(col, ch, widths);
        let grid = match screen {
            Screen::Primary => primary,
            Screen::Alternate => alternate.as_mut().expect(ALTERNATE_EXISTS),
        };
        let row = grid.row_mut(cursor.line);
        // Whether the next character may join the cell before it, as one
        // after a ZWJ does, or by clusters one after a character past
        // ASCII may: then even ASCII is looked at a character at a time.
        let mut joins = previous_cell(row, cursor).is_some_and(|col| may_join(row, col, clusters));
        // Nothing below interns again, so the index stays valid.
        let style = row.intern(&cursor.pen);
        let verbatim = cursor.charsets.maps_ascii_verbatim();

        let bytes = text.bytes();
        let mut used = start;
        while let Some(&byte) = bytes.get(used) {
            // The parser never passes control characters, so any ASCII byte
            // is printable.
            if byte.is_ascii() && verbatim && !joins {
                if cursor.pending_wrap {
                    return (used, true);
                }
                let room = edge - cursor.col;
                let run = if T::ASCII {
                    room.min(bytes.len() - used)
                } else {
                    bytes[used..]
                        .iter()
                        .take(room)
                        .take_while(|byte| byte.is_ascii())
                        .count()
                };
                row.write_ascii(cursor.col, &bytes[used..used + run], style);
                advance(cursor, run, edge, autowrap);
                used += run;
                *last_printed = Some(char::from(bytes[used - 1]));
                continue;
            }

            let (ch, len) = text.char_at(used);
            let ch = cursor.charsets.map(ch);
            match width::of(ch) {
                // Control characters never get here, but drop them if so.
                None => {}
                // By clusters, a character past ASCII may go on with an
                // ASCII cell's, which `joins` doesn't say, as a spacing
                // mark or a skin tone after a letter does.
                Some(cells)
                    if cells == 0
                        || ((joins || clusters)
                            && previous_cell(row, cursor).is_some_and(|col| {
                                joins_cell(row, col, ch, clusters) && !blocked(row, col, ch)
                            })) =>
                {
                    joins = match previous_cell(row, cursor) {
                        // A variation selector that can't widen its emoji
                        // is left out.
                        Some(col) if blocked(row, col, ch) => may_join(row, col, clusters),
                        Some(col) => {
                            row.append_to_grapheme(col, ch);
                            // What joined it may make it wide, if there's
                            // room: the cursor is then past both halves.
                            if col + 1 < edge && row.wants_wide(col, widths) {
                                row.widen(col);
                                advance(cursor, 1, edge, autowrap);
                            }
                            // Not just `ch == ZWJ`: a full grapheme drops
                            // what's appended.
                            may_join(row, col, clusters)
                        }
                        None => false,
                    };
                }
                // A wide character can't fit a one-column terminal, or
                // margins one column apart, at all.
                Some(cells) if cells > width => {}
                Some(cells) => {
                    let wide = cells == 2;
                    if cursor.pending_wrap || (wide && cursor.col + 1 == edge && autowrap) {
                        return (used, true);
                    }
                    // Without autowrap, a wide character that won't fit has
                    // no next line to go to, so it's dropped.
                    if !(wide && cursor.col + 1 == edge) {
                        row.write_cell(cursor.col, ch, wide, style);
                        advance(cursor, cells, edge, autowrap);
                        *last_printed = Some(ch);
                        joins = clusters && !ch.is_ascii();
                    }
                }
            }
            used += len;
        }
        (used, false)
    }

    /// Prints one character at the cursor, the simple way. Text normally
    /// takes [`print_on_line`](Self::print_on_line) instead, which must
    /// match this.
    fn print(&mut self, ch: char) {
        let ch = self.cursor.charsets.map(ch);
        match width::of(ch) {
            // Control characters reach here only as invalid input; drop them.
            None => {}
            Some(0) => self.print_zero_width(ch),
            Some(width) => {
                if self.joins_previous_grapheme(ch) {
                    self.print_zero_width(ch);
                } else {
                    self.print_cell(ch, width == 2);
                }
            }
        }
    }

    /// Writes a character that takes up cells, at the cursor.
    fn print_cell(&mut self, ch: char, wide: bool) {
        let cells = if wide { 2 } else { 1 };
        let (edge, wraps_to) = self.print_span();
        if cells > self.width().min(edge - wraps_to) {
            return;
        }
        if self.cursor.pending_wrap {
            self.wrap();
        }
        let (edge, _) = self.print_span();
        if wide && self.cursor.col + 1 == edge {
            // Both halves won't fit on this line. Without autowrap there is
            // no next line to move to, so the character is dropped.
            if !self.modes.contains(Modes::AUTOWRAP) {
                return;
            }
            self.wrap();
        }

        let edge = self.print_span().0;
        let CursorState { line, col, pen, .. } = self.cursor;
        if self.modes.contains(Modes::INSERT) {
            self.insert_cells(line, col, edge, cells, &pen);
        }
        self.grid_mut().row_mut(line).write(col, ch, wide, &pen);
        self.last_printed = Some(ch);
        // A placeholder printed in a region makes its image as private.
        if ch == crate::graphics::PLACEHOLDER && !self.regions.hinting.is_empty() {
            self.hint_virtuals_of(&pen);
        }
        // After it's noted, as a cap reached here closes the region, and
        // forgets it.
        if !self.regions.hinting.is_empty() {
            self.hint_row(line);
        }

        if col + cells < edge {
            self.cursor.col = col + cells;
        } else {
            self.cursor.col = edge - 1;
            self.cursor.pending_wrap = self.modes.contains(Modes::AUTOWRAP);
        }
    }

    /// `OSC 46373 ; forget`: notes, for the terminal's owner, that what it
    /// kept before the cursor's line is to be forgotten, or behind a
    /// full-screen program, all the main screen holds (see
    /// [`crate::regions`]).
    pub(crate) fn forget_asked(&mut self) {
        let rows = match self.screen {
            Screen::Primary => self.cursor.line,
            Screen::Alternate => self.primary.height(),
        };
        let line = self.primary.scrollback_end() + rows as u64;
        let forget = &mut self.regions.forget;
        *forget = Some(forget.map_or(line, |before| before.max(line)));
    }

    /// `OSC 46373 ; prompt ; clackatoo=SECRET`: clackatoo's own shell
    /// integration says its prompt is here, signed with the terminal's
    /// secret, so it's the shell's, never a program's: every region closes
    /// (see [`crate::regions`]). Unsigned, it's nothing.
    pub(crate) fn signed_prompt(&mut self, options: &[&[u8]]) {
        if self.carries_secret(options) {
            self.close_regions();
        }
    }

    /// Closes every region (see [`crate::regions`]). REP never repeats a
    /// character printed in one after.
    pub(crate) fn close_regions(&mut self) {
        self.regions.close_all();
        self.last_printed = None;
    }

    /// Gives the cursor's row, `line`, just printed on, the hints of the
    /// regions open, and counts it against their caps: a low one if no
    /// shell will close them, and a high one whatever (see
    /// [`crate::regions`]).
    #[cold]
    pub(crate) fn hint_row(&mut self, line: usize) {
        self.hint_row_with(line, self.regions.hinting);
    }

    /// As [`hint_row`](Self::hint_row), with `hints`, the regions' as
    /// something began that's printed over several rows.
    pub(crate) fn hint_row_with(&mut self, line: usize, hints: crate::Hints) {
        self.grid_mut().row_mut(line).add_hints(hints);
        let number = self.primary.scrollback_end() + line as u64;
        let most = if self.secret.is_none() {
            crate::regions::MOST_LINES
        } else {
            crate::regions::MOST_LINES_EVER
        };
        if self.regions.printed_on(number) > most {
            log::info!("regions marked private closed after {most} lines");
            self.close_regions();
        }
    }

    /// Inserts `count` blank cells in `style` at column `col` of `line`,
    /// moving what's there right as far as column `end`, past which it
    /// goes: the right margin, or the screen's edge.
    fn insert_cells(&mut self, line: usize, col: usize, end: usize, count: usize, style: &Style) {
        let width = self.width();
        let row = self.grid_mut().row_mut(line);
        if end == width {
            row.insert_blanks(col, count, style);
            return;
        }
        let before = row.clone();
        row.fill(col..end, style);
        if count < end - col {
            row.copy_span(col + count, &before, col, end - col - count);
        }
    }

    /// Deletes `count` cells at column `col` of `line`, moving what's
    /// right of them left, as far as column `end`, and blanks in `style`
    /// in behind them.
    fn delete_cells(&mut self, line: usize, col: usize, end: usize, count: usize, style: &Style) {
        let width = self.width();
        let row = self.grid_mut().row_mut(line);
        if end == width {
            row.delete_cells(col, count, style);
            return;
        }
        let before = row.clone();
        row.fill(col..end, style);
        if count < end - col {
            row.copy_span(col, &before, col + count, end - col - count);
        }
    }

    /// Adds a zero-width codepoint to the character before the cursor,
    /// which it may make wide (see [`Widths`]): then the cursor is past
    /// both halves, and in insert mode, the rest of the line moves right.
    fn print_zero_width(&mut self, ch: char) {
        let Some(col) = self.previous_cell() else {
            return;
        };
        if self.widening_blocked(col, ch) {
            return;
        }
        let (edge, widths) = (self.print_span().0, self.config.widths);
        let autowrap = self.modes.contains(Modes::AUTOWRAP);
        let insert = self.modes.contains(Modes::INSERT);
        let CursorState { line, pen, .. } = self.cursor;
        if !self.regions.hinting.is_empty() {
            self.hint_row(line);
        }
        let row = self.grid_mut().row_mut(line);
        row.append_to_grapheme(col, ch);
        if col + 1 < edge && row.wants_wide(col, widths) {
            if insert {
                self.insert_cells(line, col + 1, edge, 1, &pen);
            }
            self.grid_mut().row_mut(line).widen(col);
            advance(&mut self.cursor, 1, edge, autowrap);
        }
    }

    /// Whether `ch`, the next character, continues the previous grapheme
    /// instead of starting a cell. That happens after a zero-width joiner
    /// (ZWJ), which glues emoji like 👩‍💻 into one, and with mode 2027,
    /// wherever Unicode says the cluster goes on.
    fn joins_previous_grapheme(&self, ch: char) -> bool {
        let row = self.grid().row(self.cursor.line);
        let clusters = self.modes.contains(Modes::GRAPHEMES);
        previous_cell(row, &self.cursor).is_some_and(|col| {
            joins_cell(row, col, ch, clusters) && !self.widening_blocked(col, ch)
        })
    }

    /// Whether `ch` joining the cell at `col` would widen it with no room
    /// for that, in the last column or at the right margin: then it
    /// doesn't join, so that a cell its joiners widen is always wide, as
    /// a row rebuilt from its text has it, wherever it's moved to. A
    /// variation selector is left out, as nothing would show it; a
    /// cluster's next character starts a cell of its own, as with mode
    /// 2027 off.
    fn widening_blocked(&self, col: usize, ch: char) -> bool {
        let edge = self.print_span().0;
        col + 1 >= edge
            && (self.grid().row(self.cursor.line)).would_widen(col, ch, self.config.widths)
    }

    fn previous_cell(&self) -> Option<usize> {
        previous_cell(self.grid().row(self.cursor.line), &self.cursor)
    }

    /// Continues on the next line after text reached the right edge, or
    /// the right margin: then at the left one.
    fn wrap(&mut self) {
        let line = self.cursor.line;
        let (_, start) = self.print_span();
        // Without a pending wrap, this is a wide character that doesn't fit
        // in the last column. Between margins, a line goes on in a box,
        // not across the screen: the row isn't one that continues.
        let early = !self.cursor.pending_wrap;
        let margins = self.margins_on();
        let row = self.grid_mut().row_mut(line);
        if !margins {
            if early {
                row.set_wrapped_early();
            } else {
                row.set_wrapped(true);
            }
        }
        self.index();
        self.cursor.col = start;
    }

    pub(crate) fn repeat_last(&mut self, count: usize) {
        if let Some(ch) = self.last_printed {
            for _ in 0..count {
                self.print_cell(ch, width::of(ch) == Some(2));
            }
        }
    }

    // --- Control characters -----------------------------------------------

    pub(crate) fn line_feed(&mut self) {
        self.index();
        if self.modes.contains(Modes::LINEFEED_NEWLINE) {
            self.cursor.col = 0;
        }
    }

    /// CR: to the line's start, or the left margin, from it or right of
    /// it.
    pub(crate) fn carriage_return(&mut self) {
        self.cursor.col = if self.margins_on() && self.cursor.col >= self.margins.start {
            self.margins.start
        } else {
            0
        };
        self.cursor.pending_wrap = false;
    }

    pub(crate) fn backspace(&mut self) {
        self.move_left(1);
    }

    pub(crate) fn tab_forward(&mut self, count: usize) {
        // Within the margins, no further than the right one.
        let last = if self.margins_on() && self.within_margins() {
            self.margins.end - 1
        } else {
            self.width() - 1
        };
        let mut col = self.cursor.col;
        for _ in 0..count {
            col = self.tabs.next(col).unwrap_or(last).min(last);
        }
        self.set_col(col);
    }

    pub(crate) fn tab_backward(&mut self, count: usize) {
        let mut col = self.cursor.col;
        for _ in 0..count {
            col = self.tabs.previous(col).unwrap_or(0);
        }
        self.set_col(col);
    }

    pub(crate) fn set_tab_stop(&mut self) {
        self.tabs.set(self.cursor.col);
    }

    pub(crate) fn clear_tab_stop(&mut self) {
        self.tabs.clear(self.cursor.col);
    }

    pub(crate) fn clear_all_tab_stops(&mut self) {
        self.tabs.clear_all();
    }

    // --- Vertical movement and scrolling ----------------------------------

    /// IND: moves down a line, scrolling if the cursor is at the bottom of
    /// the scroll region.
    pub(crate) fn index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.line + 1 == self.scroll_region.end {
            // Outside the margins, nothing scrolls.
            if !self.margins_on() || self.within_margins() {
                self.scroll_up(1);
            }
        } else if self.cursor.line + 1 < self.height() {
            self.cursor.line += 1;
        }
    }

    /// RI: moves up a line, scrolling down if the cursor is at the top of
    /// the scroll region.
    pub(crate) fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.line == self.scroll_region.start {
            if !self.margins_on() || self.within_margins() {
                self.scroll_down(1);
            }
        } else {
            self.cursor.line = self.cursor.line.saturating_sub(1);
        }
    }

    /// Scrolls the scroll region up. Lines leaving the top of a full-screen
    /// region go into scrollback. A narrower region discards them, since
    /// they're part of an app's layout, not history.
    pub(crate) fn scroll_up(&mut self, count: usize) {
        let blank = self.blank();
        let region = self.scroll_region.clone();
        if self.margins_on() {
            // A box between the margins, which history isn't.
            let margins = self.margins.clone();
            self.grid_mut()
                .scroll_box(region, margins, count, true, &blank);
        } else if region == (0..self.height()) {
            self.grid_mut().scroll_up_into_scrollback(count, &blank);
        } else {
            self.grid_mut().scroll_up(region, count, &blank);
        }
    }

    pub(crate) fn scroll_down(&mut self, count: usize) {
        let blank = self.blank();
        let region = self.scroll_region.clone();
        if self.margins_on() {
            let margins = self.margins.clone();
            self.grid_mut()
                .scroll_box(region, margins, count, false, &blank);
        } else {
            self.grid_mut().scroll_down(region, count, &blank);
        }
    }

    /// IL: inserts blank lines at the cursor, pushing the lines below down
    /// within the scroll region.
    pub(crate) fn insert_lines(&mut self, count: usize) {
        if self.scroll_region.contains(&self.cursor.line) {
            let blank = self.blank();
            let lines = self.cursor.line..self.scroll_region.end;
            if self.margins_on() {
                // Only within the margins, between them.
                if !self.within_margins() {
                    return;
                }
                let margins = self.margins.clone();
                self.grid_mut()
                    .scroll_box(lines, margins, count, false, &blank);
            } else {
                self.grid_mut().scroll_down(lines, count, &blank);
            }
            self.carriage_return();
        }
    }

    /// DL: deletes lines at the cursor, pulling the lines below up within the
    /// scroll region.
    pub(crate) fn delete_lines(&mut self, count: usize) {
        if self.scroll_region.contains(&self.cursor.line) {
            let blank = self.blank();
            let lines = self.cursor.line..self.scroll_region.end;
            if self.margins_on() {
                if !self.within_margins() {
                    return;
                }
                let margins = self.margins.clone();
                self.grid_mut()
                    .scroll_box(lines, margins, count, true, &blank);
            } else {
                self.grid_mut().scroll_up(lines, count, &blank);
            }
            self.carriage_return();
        }
    }

    /// DECSLRM: sets the left and right margins to columns
    /// `left..=right`, counting from 1, while DECLRMM is set. `None` means
    /// the screen's edge. Margins that leave no room are ignored.
    pub(crate) fn set_margins(&mut self, left: Option<usize>, right: Option<usize>) {
        if !self.modes.contains(Modes::LEFT_RIGHT_MARGINS) {
            return;
        }
        let left = left.unwrap_or(1).max(1) - 1;
        let right = right.unwrap_or(self.width()).min(self.width());
        if left + 1 < right {
            self.margins = left..right;
            self.move_to_origin(0, 0);
        }
    }

    /// DECSTBM: sets the scroll region to lines `top..=bottom`, counting from
    /// 1. `None` means the screen edge. Invalid regions are ignored.
    pub(crate) fn set_scroll_region(&mut self, top: Option<usize>, bottom: Option<usize>) {
        let top = top.unwrap_or(1).max(1) - 1;
        let bottom = bottom.unwrap_or(self.height()).min(self.height());
        if top + 1 < bottom {
            self.scroll_region = top..bottom;
            self.move_to_origin(0, 0);
        }
    }

    // --- Cursor movement --------------------------------------------------

    /// Moves to an absolute screen position, clamped to the screen.
    fn move_to(&mut self, line: usize, col: usize) {
        self.cursor.line = line.min(self.height() - 1);
        self.cursor.col = col.min(self.width() - 1);
        self.cursor.pending_wrap = false;
    }

    /// Moves to a position as an app addresses it (CUP and friends). In
    /// origin mode, lines count from the top of the scroll region and the
    /// cursor can't leave it.
    pub(crate) fn move_to_origin(&mut self, line: usize, col: usize) {
        let origin = self.modes.contains(Modes::ORIGIN);
        let line = if origin {
            (self.scroll_region.start + line).min(self.scroll_region.end - 1)
        } else {
            line
        };
        // Columns count from the left margin, and stop at the right.
        let col = if origin && self.margins_on() {
            (self.margins.start + col).min(self.margins.end - 1)
        } else {
            col
        };
        self.move_to(line, col);
    }

    /// HPA and CHA: to column `col` of the cursor's line, as an app
    /// addresses it: in origin mode, from the left margin.
    pub(crate) fn set_col_origin(&mut self, col: usize) {
        let col = if self.modes.contains(Modes::ORIGIN) && self.margins_on() {
            (self.margins.start + col).min(self.margins.end - 1)
        } else {
            col
        };
        self.set_col(col);
    }

    pub(crate) fn set_line_origin(&mut self, line: usize) {
        self.move_to_origin(line, self.cursor.col);
    }

    pub(crate) fn set_col(&mut self, col: usize) {
        self.move_to(self.cursor.line, col);
    }

    /// CUU: moves up, stopping at the top of the scroll region if the cursor
    /// starts inside it.
    pub(crate) fn move_up(&mut self, count: usize) {
        let top = if self.cursor.line >= self.scroll_region.start {
            self.scroll_region.start
        } else {
            0
        };
        self.move_to(
            self.cursor.line.saturating_sub(count).max(top),
            self.cursor.col,
        );
    }

    /// CUD: moves down, stopping at the bottom of the scroll region if the
    /// cursor starts inside it.
    pub(crate) fn move_down(&mut self, count: usize) {
        let bottom = if self.cursor.line < self.scroll_region.end {
            self.scroll_region.end - 1
        } else {
            self.height() - 1
        };
        self.move_to(
            self.cursor.line.saturating_add(count).min(bottom),
            self.cursor.col,
        );
    }

    /// CUF: right, stopping at the right margin if the cursor starts
    /// within the margins.
    pub(crate) fn move_right(&mut self, count: usize) {
        let col = self.cursor.col.saturating_add(count);
        let col = if self.margins_on() && self.within_margins() {
            col.min(self.margins.end - 1)
        } else {
            col
        };
        self.set_col(col);
    }

    /// CUB: left, stopping at the left margin if the cursor starts from
    /// it or right of it.
    pub(crate) fn move_left(&mut self, count: usize) {
        let col = self.cursor.col.saturating_sub(count);
        let col = if self.margins_on() && self.cursor.col >= self.margins.start {
            col.max(self.margins.start)
        } else {
            col
        };
        self.set_col(col);
    }

    /// The cursor position as reported to apps: 1-based, and relative to
    /// the scroll region in origin mode.
    pub(crate) fn reported_position(&self) -> (usize, usize) {
        let origin = self.modes.contains(Modes::ORIGIN);
        let line = if origin {
            self.cursor.line.saturating_sub(self.scroll_region.start)
        } else {
            self.cursor.line
        };
        let col = if origin && self.margins_on() {
            self.cursor.col.saturating_sub(self.margins.start)
        } else {
            self.cursor.col
        };
        (line + 1, col + 1)
    }

    // --- Erasing and editing ----------------------------------------------

    /// ED: erases part of the screen. 0 erases from the cursor to the end, 1
    /// from the start to the cursor, 2 everything, 3 only scrollback.
    pub(crate) fn erase_in_display(&mut self, mode: u16) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let CursorState { line, col, .. } = self.cursor;
        let (width, height) = (self.width(), self.height());
        let grid = self.grid_mut();
        match mode {
            // Erasing text leaves pictures, as in kitty.
            0 => {
                let row = grid.edit_row(line);
                row.fill(col..width, &blank);
                row.set_wrapped(false);
                for below in line + 1..height {
                    grid.row_mut(below).erase(&blank);
                }
            }
            1 => {
                for above in 0..line {
                    grid.row_mut(above).erase(&blank);
                }
                grid.edit_row(line).fill(0..col + 1, &blank);
                grid.end_history_line();
            }
            2 => {
                grid.clear(&blank);
            }
            3 => {
                let had_history = grid.scrollback_len() > 0;
                grid.clear_scrollback();
                self.history_cleared |= had_history;
            }
            _ => log::trace!("unhandled ED mode {mode}"),
        }
    }

    /// EL: erases part of the cursor's line. 0 erases from the cursor to the
    /// end, 1 from the start to the cursor, 2 the whole line.
    pub(crate) fn erase_in_line(&mut self, mode: u16) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let CursorState { line, col, .. } = self.cursor;
        let width = self.width();
        let row = self.grid_mut().edit_row(line);
        match mode {
            0 => {
                row.fill(col..width, &blank);
                row.set_wrapped(false);
            }
            1 => row.fill(0..col + 1, &blank),
            // Shells clear the line to draw their prompt again, which keeps
            // its mark.
            2 => {
                let mark = row.mark();
                row.reset(&blank);
                row.set_mark(mark);
            }
            _ => log::trace!("unhandled EL mode {mode}"),
        }
    }

    /// ECH: blanks characters from the cursor rightwards, without moving
    /// anything.
    pub(crate) fn erase_chars(&mut self, count: usize) {
        self.cursor.pending_wrap = false;
        let blank = self.blank();
        let CursorState { line, col, .. } = self.cursor;
        let end = col.saturating_add(count);
        self.grid_mut().edit_row(line).fill(col..end, &blank);
    }

    /// ICH: inserts blanks at the cursor, moving what's right of it
    /// towards the right margin, or the screen's edge.
    pub(crate) fn insert_chars(&mut self, count: usize) {
        self.cursor.pending_wrap = false;
        let Some(end) = self.edit_end() else { return };
        let blank = self.blank();
        let CursorState { line, col, .. } = self.cursor;
        // Editing, not printing: the top row no longer goes on from history.
        self.grid_mut().edit_row(line);
        self.insert_cells(line, col, end, count.min(end - col), &blank);
    }

    /// DCH: deletes characters at the cursor, as far as the right margin,
    /// or the screen's edge.
    pub(crate) fn delete_chars(&mut self, count: usize) {
        self.cursor.pending_wrap = false;
        let Some(end) = self.edit_end() else { return };
        let blank = self.blank();
        let CursorState { line, col, .. } = self.cursor;
        self.grid_mut().edit_row(line);
        self.delete_cells(line, col, end, count.min(end - col), &blank);
    }

    /// The column past the last that inserting or deleting characters at
    /// the cursor moves: the right margin's, or the screen's edge. `None`
    /// outside the margins, where nothing moves.
    fn edit_end(&self) -> Option<usize> {
        if !self.margins_on() {
            Some(self.width())
        } else if self.within_margins() {
            Some(self.margins.end)
        } else {
            None
        }
    }

    /// DECALN: fills the screen with `E`s, a test pattern for alignment.
    pub(crate) fn screen_alignment_test(&mut self) {
        self.scroll_region = 0..self.height();
        let (width, height) = (self.width(), self.height());
        let grid = self.grid_mut();
        for line in 0..height {
            let row = grid.edit_row(line);
            row.reset(&Style::DEFAULT);
            for col in 0..width {
                row.write(col, 'E', false, &Style::DEFAULT);
            }
        }
        self.move_to(0, 0);
    }

    // --- Modes ------------------------------------------------------------

    /// SM/RM: sets or resets an ANSI mode.
    pub(crate) fn set_ansi_mode(&mut self, mode: u16, on: bool) {
        match mode {
            4 => self.modes.set(Modes::INSERT, on),
            20 => self.modes.set(Modes::LINEFEED_NEWLINE, on),
            _ => log::trace!("unhandled ANSI mode {mode}"),
        }
    }

    /// DECSET/DECRST: sets or resets a DEC private mode.
    ///
    /// Mouse reporting and focus events are only recorded: clients do the
    /// reporting.
    pub(crate) fn set_dec_mode(&mut self, mode: u16, on: bool) {
        // As in xterm, one tracking mode and one encoding apply at a time,
        // and resetting any of them turns tracking or encoding off.
        if let Some(tracking) = MouseTracking::from_mode(mode) {
            self.mouse_tracking = if on { tracking } else { MouseTracking::Off };
            return;
        }
        if let Some(encoding) = MouseEncoding::from_mode(mode) {
            self.mouse_encoding = if on { encoding } else { MouseEncoding::Default };
            return;
        }
        match mode {
            1 => self.modes.set(Modes::APP_CURSOR_KEYS, on),
            1004 => self.modes.set(Modes::FOCUS_EVENTS, on),
            66 => self.modes.set(Modes::APP_KEYPAD, on),
            2004 => self.modes.set(Modes::BRACKETED_PASTE, on),
            2026 => self.modes.set(Modes::SYNCHRONIZED, on),
            // Only where the rows are rebuilt by clusters too.
            2027 if self.config.widths.graphemes => self.modes.set(Modes::GRAPHEMES, on),
            // Turned off, the margins are the screen's edges again.
            69 => {
                self.modes.set(Modes::LEFT_RIGHT_MARGINS, on);
                if !on {
                    self.margins = 0..self.width();
                }
            }
            // Told at once, as it asks, and then at each resize.
            2048 => {
                self.modes.set(Modes::RESIZE_REPORTS, on);
                if on {
                    self.report_size();
                }
            }
            6 => {
                self.modes.set(Modes::ORIGIN, on);
                self.move_to_origin(0, 0);
            }
            7 => {
                self.modes.set(Modes::AUTOWRAP, on);
                self.cursor.pending_wrap &= on;
            }
            25 => self.modes.set(Modes::CURSOR_VISIBLE, on),
            47 => self.switch_screen(on),
            1047 => {
                if !on && self.screen == Screen::Alternate {
                    self.grid_mut().clear(&Style::DEFAULT);
                }
                self.switch_screen(on);
            }
            1048 => {
                if on {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => {
                if on && self.screen == Screen::Primary {
                    self.save_cursor();
                    self.switch_screen(true);
                    self.grid_mut().clear(&Style::DEFAULT);
                } else if !on && self.screen == Screen::Alternate {
                    self.switch_screen(false);
                    self.restore_cursor();
                }
            }
            _ => log::trace!("unhandled DEC private mode {mode}"),
        }
    }

    /// DECRQM: whether mode `mode`, a DEC private mode if `dec`, is set,
    /// as DECRPM answers it: 1 for set, 2 for reset, 4 for one that can't
    /// be set, and 0 for a mode this terminal doesn't know. Programs ask before they rely on a mode, as
    /// for synchronized output.
    pub(crate) fn report_mode(&mut self, mode: u16, dec: bool) {
        let state = if dec {
            self.dec_mode(mode)
        } else {
            self.ansi_mode(mode)
        };
        let answer = match state {
            Some(true) => 1,
            Some(false) => 2,
            // A terminal laid out by the rules from before grapheme
            // clusters, as one an older build handed over, can't print by
            // them: it says so, rather than that it doesn't know the mode.
            None if dec && mode == 2027 => 4,
            None => 0,
        };
        let marker = if dec { "?" } else { "" };
        self.reply(format!("\x1b[{marker}{mode};{answer}$y").as_bytes());
    }

    /// Whether ANSI mode `mode` is set, if it's one this terminal knows.
    fn ansi_mode(&self, mode: u16) -> Option<bool> {
        let flag = match mode {
            4 => Modes::INSERT,
            20 => Modes::LINEFEED_NEWLINE,
            _ => return None,
        };
        Some(self.modes.contains(flag))
    }

    /// Whether DEC private mode `mode` is set, if it's one this terminal
    /// knows: one [`set_dec_mode`](Self::set_dec_mode) sets.
    fn dec_mode(&self, mode: u16) -> Option<bool> {
        if let Some(tracking) = MouseTracking::from_mode(mode) {
            return Some(self.mouse_tracking == tracking);
        }
        if let Some(encoding) = MouseEncoding::from_mode(mode) {
            return Some(self.mouse_encoding == encoding);
        }
        let flag = match mode {
            1 => Modes::APP_CURSOR_KEYS,
            6 => Modes::ORIGIN,
            7 => Modes::AUTOWRAP,
            25 => Modes::CURSOR_VISIBLE,
            66 => Modes::APP_KEYPAD,
            1004 => Modes::FOCUS_EVENTS,
            2004 => Modes::BRACKETED_PASTE,
            2026 => Modes::SYNCHRONIZED,
            2027 if self.config.widths.graphemes => Modes::GRAPHEMES,
            2048 => Modes::RESIZE_REPORTS,
            69 => Modes::LEFT_RIGHT_MARGINS,
            47 | 1047 | 1049 => return Some(self.screen == Screen::Alternate),
            _ => return None,
        };
        Some(self.modes.contains(flag))
    }

    fn switch_screen(&mut self, alternate: bool) {
        let screen = if alternate {
            Screen::Alternate
        } else {
            Screen::Primary
        };
        if screen == self.screen {
            return;
        }
        if alternate {
            let (width, height) = (self.width(), self.height());
            let widths = self.config.widths;
            self.alternate.get_or_insert_with(|| {
                let mut grid = Grid::new(width, height, 0, Compaction::Off);
                grid.set_widths(widths);
                grid
            });
            // A program that switches to it starts with no keyboard flags,
            // whatever one that ended without popping them left, and with
            // the window's own pointer.
            self.keyboard_alternate = keyboard::Stack::default();
            self.pointer_alternate = pointer::Stack::default();
        }
        self.screen = screen;
        // A viewer was showing the other screen.
        self.grid_mut().damage_all();
    }

    // --- Cursor save and restore, resets ----------------------------------

    /// DECSC: saves the cursor's position, pen, character sets and origin
    /// mode, for the current screen.
    pub(crate) fn save_cursor(&mut self) {
        let saved = SavedCursor {
            cursor: self.cursor,
            origin: self.modes.contains(Modes::ORIGIN),
        };
        *self.saved_cursor_mut() = saved;
    }

    /// DECRC: restores what [`save_cursor`](Self::save_cursor) saved, or the
    /// power-on state if nothing was saved.
    pub(crate) fn restore_cursor(&mut self) {
        let saved = *self.saved_cursor_mut();
        self.cursor = saved.cursor;
        self.modes.set(Modes::ORIGIN, saved.origin);
        // The saved position may predate a resize.
        self.cursor.line = self.cursor.line.min(self.height() - 1);
        self.cursor.col = self.cursor.col.min(self.width() - 1);
    }

    fn saved_cursor_mut(&mut self) -> &mut SavedCursor {
        match self.screen {
            Screen::Primary => &mut self.saved_primary,
            Screen::Alternate => &mut self.saved_alternate,
        }
    }

    /// DECSCUSR: the cursor's shape. 0 asks for the user's default; 1 and
    /// 2 are a block, 3 and 4 an underline, 5 and 6 a bar, each blinking or
    /// steady. Blinking isn't drawn, so the pairs are the same.
    pub(crate) fn set_cursor_shape(&mut self, style: u16) {
        self.cursor_shape = match style {
            0 => CursorShape::Default,
            1 | 2 => CursorShape::Block,
            3 | 4 => CursorShape::Underline,
            5 | 6 => CursorShape::Bar,
            _ => return,
        };
    }

    /// XTWINOPS (`CSI t`): `op` asks about the window, or changes it, of
    /// `which`, its second parameter. Only the title's stack and the size
    /// of the text in cells are this terminal's to answer; the window's
    /// place, and its size in pixels, are the window's own.
    pub(crate) fn window_op(&mut self, op: u16, which: Option<u16>) {
        // 0 is the icon's name and the title, 2 the title alone, and 1
        // the icon's name alone, which there's none of.
        let title = matches!(which.unwrap_or(0), 0 | 2);
        match op {
            // The text area's size in pixels, and a cell's, once a window
            // has said: until then, nothing, as nothing true can be said.
            14 => {
                if let Some([width, height]) = self.text_pixels() {
                    self.reply(format!("\x1b[4;{height};{width}t").as_bytes());
                }
            }
            16 => {
                if let Some([width, height]) = self.cell_pixels {
                    self.reply(format!("\x1b[6;{height};{width}t").as_bytes());
                }
            }
            18 => {
                let reply = format!("\x1b[8;{};{}t", self.height(), self.width());
                self.reply(reply.as_bytes());
            }
            22 if title => self.push_title(),
            23 if title => {
                if let Some(title) = self.titles.pop() {
                    self.title = title;
                }
            }
            _ => log::trace!("unhandled window operation {op}"),
        }
    }

    /// The text area's size in pixels, as `[width, height]`, if a cell's
    /// is known: the cells' times a cell's, at most what fits a `u16`.
    fn text_pixels(&self) -> Option<[u16; 2]> {
        let [width, height] = self.cell_pixels?;
        let times = |cells: u16, pixels: u16| {
            u16::try_from(u32::from(cells) * u32::from(pixels)).unwrap_or(u16::MAX)
        };
        Some([
            times(self.size.cols(), width),
            times(self.size.rows(), height),
        ])
    }

    /// Mode 2048's report of the terminal's size: `CSI 48 ; rows ; cols ;
    /// height ; width t`, its size in pixels 0 if it isn't known.
    pub(crate) fn report_size(&mut self) {
        let [width, height] = self.text_pixels().unwrap_or([0, 0]);
        let reply = format!(
            "\x1b[48;{};{};{height};{width}t",
            self.height(),
            self.width()
        );
        self.reply(reply.as_bytes());
    }

    /// Keeps the title to put back later, as vim does as it starts.
    fn push_title(&mut self) {
        if self.titles.len() == MOST_TITLES {
            self.titles.remove(0);
        }
        self.titles.push(shortened(&self.title));
    }

    /// The kitty keyboard protocol's flags on the screen shown.
    pub(crate) fn keyboard(&self) -> &keyboard::Stack {
        match self.screen {
            Screen::Primary => &self.keyboard_primary,
            Screen::Alternate => &self.keyboard_alternate,
        }
    }

    pub(crate) fn keyboard_mut(&mut self) -> &mut keyboard::Stack {
        match self.screen {
            Screen::Primary => &mut self.keyboard_primary,
            Screen::Alternate => &mut self.keyboard_alternate,
        }
    }

    /// OSC 22: the mouse pointer's shape, as `payload` asks (see
    /// [`crate::pointer`]); a question is answered `OSC 22 ; … ST`, of
    /// known names and digits alone, ended as it was.
    pub(crate) fn pointer_osc(&mut self, payload: &[u8], bell: bool) {
        let stack = match self.screen {
            Screen::Primary => &mut self.pointer_primary,
            Screen::Alternate => &mut self.pointer_alternate,
        };
        if let Some(answer) = stack.osc(payload) {
            let end = string_end(bell);
            self.reply(format!("\x1b]22;{answer}{end}").as_bytes());
        }
    }

    /// `CSI ? u`: the kitty keyboard protocol's flags in effect, as
    /// `CSI ? flags u`. fish asks this, then DA1, and takes an answer to
    /// DA1 alone to mean the protocol isn't there.
    pub(crate) fn report_keyboard(&mut self) {
        let reply = format!("\x1b[?{}u", self.keyboard().flags());
        self.reply(reply.as_bytes());
    }

    /// Answers `request`, a device control string that has ended, unless
    /// it was too long.
    pub(crate) fn answer_request(&mut self, request: &Request) {
        match request.finished() {
            Some((requests::Kind::Capabilities, names)) => {
                let answer = requests::capabilities(names);
                self.reply(&answer);
            }
            Some((requests::Kind::Setting, setting)) => self.answer_setting(setting),
            None => {}
        }
    }

    /// DECRQSS: how the setting `setting` names, by its sequence's final
    /// characters, is set: `DCS 1 $ r`, and the sequence that would set it
    /// so; or `DCS 0 $ r` for one this terminal doesn't say. Programs ask
    /// for the pen (`m`), the cursor's shape (` q`) and the scroll region
    /// (`r`).
    fn answer_setting(&mut self, setting: &[u8]) {
        let value = match setting {
            b"m" => Some(format!("{}m", sgr::describe(&self.cursor.pen))),
            // Steady, as blinking isn't drawn, but for the user's own.
            b" q" => {
                let style = match self.cursor_shape {
                    CursorShape::Default => 0,
                    CursorShape::Block => 2,
                    CursorShape::Underline => 4,
                    CursorShape::Bar => 6,
                };
                Some(format!("{style} q"))
            }
            b"r" => Some(format!(
                "{};{}r",
                self.scroll_region.start + 1,
                self.scroll_region.end
            )),
            b"s" => Some(format!("{};{}s", self.margins.start + 1, self.margins.end)),
            _ => None,
        };
        let reply = match value {
            Some(value) => format!("\x1bP1$r{value}\x1b\\"),
            None => "\x1bP0$r\x1b\\".to_owned(),
        };
        self.reply(reply.as_bytes());
    }

    /// DECKPAM and DECKPNM: the keypad sends application sequences, or
    /// numbers.
    pub(crate) fn set_app_keypad(&mut self, on: bool) {
        self.modes.set(Modes::APP_KEYPAD, on);
    }

    /// DECSTR: soft reset. Returns modes, pen and margins to their defaults
    /// without touching the screen.
    pub(crate) fn soft_reset(&mut self) {
        self.modes = Modes::default();
        self.scroll_region = 0..self.height();
        self.margins = 0..self.width();
        self.cursor.pen = Style::DEFAULT;
        self.cursor.charsets = Charsets::default();
        self.cursor.pending_wrap = false;
        self.saved_primary = SavedCursor::default();
        self.saved_alternate = SavedCursor::default();
    }

    /// RIS: full reset to the power-on state, including scrollback.
    pub(crate) fn full_reset(&mut self) {
        // What the program asked before the reset is still to be done.
        let replies = std::mem::take(&mut self.replies);
        let clipboard = std::mem::take(&mut self.clipboard);
        let ended = self.ended.take();
        let mut notifications = std::mem::take(&mut self.notifications);
        notifications.reset();
        let generation = self.primary.history_generation();
        let (_, next_link) = self.links.parts();
        // Numbers run out only after four billion commands; then they start
        // again, which the reset's new history tells windows of anyway,
        // and which counts as numbering them again.
        let next_block = self.blocks.next();
        let renumbered = self.blocks.renumbered() + u64::from(next_block.is_none());
        let next_block = next_block.unwrap_or(0);
        let secret = self.secret.take();
        // A program can't end regions by resetting the terminal (see
        // crate::regions).
        let regions = std::mem::take(&mut self.regions);
        // So do names that are the owner's.
        let kept = (self.names_kept).then(|| (std::mem::take(&mut self.title), self.cwd.take()));
        let (palette, cell_pixels) = (self.palette, self.cell_pixels);
        // Pictures' numbers are never reused, and what it held is let go.
        let mut graphics = std::mem::take(&mut self.graphics);
        graphics.reset();
        *self = Self::new(self.size, &self.config);
        self.graphics = graphics;
        self.palette = palette;
        self.cell_pixels = cell_pixels;
        // The shell, which knows it, outlives the reset.
        self.secret = secret;
        self.regions = regions;
        if let Some((title, cwd)) = kept {
            self.names_kept = true;
            self.title = title;
            self.cwd = cwd;
        }
        self.replies = replies;
        self.clipboard = clipboard;
        self.notifications = notifications;
        self.ended = ended;
        // Link IDs are never reused (see crate::links), nor are block
        // numbers, which windows know how blocks ended by.
        self.links = Links::from_parts([], next_link);
        self.blocks = Blocks::from_parts(next_block, std::collections::VecDeque::new())
            .renumbered_as(renumbered);
        // History is numbered from 0 again.
        self.primary.follow_history(generation);
    }

    fn resize(&mut self, size: Size) {
        if size == self.size {
            return;
        }
        let (width, height) = (usize::from(size.cols()), usize::from(size.rows()));
        let on_primary = self.screen == Screen::Primary;
        let autowrap = self.modes.contains(Modes::AUTOWRAP);
        // The primary screen's cursor is the live one, or the one saved when
        // a program switched to the alternate screen.
        let primary_cursor = if on_primary {
            &mut self.cursor
        } else {
            &mut self.saved_primary.cursor
        };
        (
            primary_cursor.line,
            primary_cursor.col,
            primary_cursor.pending_wrap,
        ) = self.primary.reflow(
            width,
            height,
            (primary_cursor.line, primary_cursor.col),
            primary_cursor.pending_wrap,
            autowrap,
        );
        // Without autowrap, nothing waits to wrap.
        primary_cursor.pending_wrap &= autowrap;
        if let Some(alternate) = &mut self.alternate {
            alternate.crop(width, height);
        }

        self.size = size;
        self.scroll_region = 0..height;
        self.margins = 0..width;
        self.tabs.resize(width);
        if !on_primary {
            self.cursor.pending_wrap = false;
        }
        for cursor in [
            &mut self.cursor,
            &mut self.saved_primary.cursor,
            &mut self.saved_alternate.cursor,
        ] {
            cursor.line = cursor.line.min(height - 1);
            cursor.col = cursor.col.min(width - 1);
        }
    }

    // --- Shell integration (see crate::blocks) ------------------------

    /// OSC 133: the shell marks where a prompt (`A`), typing (`B`) or a
    /// command's output (`C`) starts, or that the command finished (`D`),
    /// with `options`.
    pub(crate) fn shell_mark(&mut self, kind: &[u8], options: &[&[u8]]) {
        if self.screen == Screen::Alternate {
            return;
        }
        match kind {
            b"C" if self.carries_secret(options) => {
                // The shell's own next command: whatever a program left
                // open has ended.
                self.close_regions();
                self.trusted_command(options);
            }
            // A prompt while a command runs is the command's output, not
            // the shell's: a program printing marks of its own, or a
            // shell nested inside it, such as one over ssh. So is a
            // command's start: what runs is what started first.
            b"A" | b"C" if self.blocks.running() => {}
            b"A" | b"C" if self.blocks.run_out() => {
                self.renumber_blocks();
                self.shell_mark(kind, options);
            }
            b"A" => {
                // The program that said how far it had got has ended, and
                // so has one that asked for keys in kitty's encoding and
                // didn't pop it, so the shell gets Ctrl+C as ^C again.
                self.progress = None;
                self.keyboard_primary = keyboard::Stack::default();
                self.pointer_primary = pointer::Stack::default();
                let block = self.blocks.prompt();
                self.mark_cursor_row(block, false);
                // Forget the blocks no row is marked with any more.
                let oldest = self.primary.marks().map(|(_, mark)| mark.block).min();
                self.blocks.forget_before(oldest.unwrap_or(block));
            }
            b"B" => self.blocks.typing(),
            b"C" => {
                let block = (self.blocks).command(cmdline(options), self.cwd.clone(), false);
                self.mark_cursor_row(block, true);
            }
            b"D" => {
                let exit = options
                    .first()
                    .and_then(|status| std::str::from_utf8(status).ok()?.parse().ok());
                if let Some(block) = self.blocks.finished(exit) {
                    self.ended = Some((block, exit));
                }
            }
            _ => {}
        }
    }

    /// Whether `options`, a `C` mark's, carry the secret clackatoo's own
    /// shell integration was given for this terminal. Compared in time
    /// that doesn't depend on where they differ.
    fn carries_secret(&self, options: &[&[u8]]) -> bool {
        let Some(secret) = &self.secret else {
            return false;
        };
        (options.iter())
            .filter_map(|option| option.strip_prefix(b"clackatoo="))
            .any(|given| {
                given.len() == secret.len()
                    && (given.iter().zip(secret.iter())).fold(0, |differ, (a, b)| differ | (a ^ b))
                        == 0
            })
    }

    /// A `C` mark carrying the secret: clackatoo's own shell integration
    /// says a command has started. fish marks the start itself first, so
    /// a command already running is the one this says it is; otherwise
    /// it starts here.
    fn trusted_command(&mut self, options: &[&[u8]]) {
        let command = cmdline(options);
        if self.blocks.running() {
            self.blocks.trust_running(command);
            return;
        }
        if self.blocks.run_out() {
            self.renumber_blocks();
        }
        let block = self.blocks.command(command, self.cwd.clone(), true);
        self.mark_cursor_row(block, true);
    }

    /// Numbers blocks from 0 again, once they've run out, forgetting every
    /// mark: what windows know of blocks is by number, so history is
    /// renumbered too, which has them start again from a snapshot.
    fn renumber_blocks(&mut self) {
        self.primary.set_marks(&[]);
        self.blocks.start_again();
        let generation = self.primary.history_generation();
        self.primary.follow_history(generation);
    }

    fn mark_cursor_row(&mut self, block: u32, output: bool) {
        let line = self.cursor.line;
        let row = self.primary.row_mut(line);
        row.set_mark(Some(Mark::add(row.mark(), block, output)));
    }

    // --- Links (see crate::links) ------------------------------------------

    /// OSC 8: text printed from now on links to `uri`, with the `id` among
    /// `params`, or to nothing if `uri` is empty.
    pub(crate) fn set_link(&mut self, params: &[u8], uri: &[u8]) {
        let Some((uri, id)) = links::parse(params, uri) else {
            self.cursor.pen.link = None;
            return;
        };
        if self.links.wants_sweep() {
            let (live, scanned) = self.live_links();
            self.links.sweep(&live, scanned);
        }
        let uri = local_file_uri(&uri, self.config.local_host.as_deref());
        self.cursor.pen.link = self.links.intern(&uri, id.as_deref());
    }

    /// Every link a row or a pen may refer to, and how many styles were
    /// looked at to find them.
    fn live_links(&self) -> (HashSet<LinkId>, usize) {
        let pens = [
            &self.cursor,
            &self.saved_primary.cursor,
            &self.saved_alternate.cursor,
        ];
        let alternate = self.alternate.iter().flat_map(Grid::styles);
        let pens = pens.into_iter().map(|cursor| &cursor.pen);
        let mut scanned = 0;
        let live = (self.primary.styles().chain(alternate).chain(pens))
            .inspect(|_| scanned += 1)
            .filter_map(|style| style.link)
            .collect();
        (live, scanned)
    }

    /// The number of links the terminal holds.
    #[cfg(test)]
    pub(crate) fn link_count(&self) -> usize {
        self.links.len()
    }

    /// OSC 10, 11 and 12: each `?` among `asked` asks for a colour, the
    /// first the one `first` numbers and each after it the next, as xterm
    /// reads `OSC 10 ; ? ; ?`: 10 the text's, 11 the background's, 12 the
    /// cursor's. Each is answered as asked, `OSC 11 ; rgb:… ST`, ended as
    /// the question was. Setting them isn't supported.
    pub(crate) fn report_colours(&mut self, first: u16, asked: &[&[u8]], bell: bool) {
        let Some(palette) = self.palette else {
            return;
        };
        for (number, asked) in (first..=12).zip(asked) {
            if *asked != b"?" {
                continue;
            }
            let colour = match number {
                10 => palette.foreground,
                11 => palette.background,
                _ => palette.cursor,
            };
            let end = string_end(bell);
            let reply = format!("\x1b]{number};{}{end}", colours::x11(colour));
            self.reply(reply.as_bytes());
        }
    }

    /// OSC 4: each `index ; ?` pair among `asked` asks for an entry of
    /// the palette, answered as `OSC 4 ; index ; rgb:… ST`. Setting them
    /// isn't supported.
    pub(crate) fn report_palette(&mut self, asked: &[&[u8]], bell: bool) {
        let Some(palette) = self.palette else {
            return;
        };
        for pair in asked.chunks(2) {
            let [index, b"?"] = pair else {
                continue;
            };
            let Some(index) =
                (std::str::from_utf8(index).ok()).and_then(|index| index.parse().ok())
            else {
                continue;
            };
            let end = string_end(bell);
            let colour = colours::x11(palette.indexed(index));
            let reply = format!("\x1b]4;{index};{colour}{end}");
            self.reply(reply.as_bytes());
        }
    }

    /// OSC 9;4: how far the program's task has got, as `params` say.
    pub(crate) fn set_progress(&mut self, params: &[&[u8]]) {
        if let Some(progress) = TaskProgress::parse(params) {
            self.progress = progress;
        }
    }

    /// OSC 7: the shell's working directory, as a `file://` URL. A path
    /// longer than any directory's can be isn't one.
    pub(crate) fn set_cwd(&mut self, url: &[u8]) {
        // Each byte is at most three escaped, and a host comes first.
        if url.len() > 3 * LONGEST_PATH + 512 {
            return;
        }
        if let Some(path) = blocks::file_url_path(url)
            && path.len() <= LONGEST_PATH
            && self.cwd.as_deref() != Some(path.as_str())
        {
            self.cwd = Some(path.into());
        }
    }

    /// Queues `bytes`, one whole reply, unless [`MAX_REPLIES`] are
    /// waiting already: a program asking faster than it reads gets no
    /// more answers, and never part of one, until they're taken.
    ///
    /// Replies are typed into the program as if from the keyboard, so one
    /// that isn't [`safe_reply`] is never sent: it could only be made
    /// of what a program printed, which could have it type anything.
    pub(crate) fn reply(&mut self, bytes: &[u8]) {
        if !safe_reply(bytes) {
            debug_assert!(false, "an unsafe reply: {bytes:?}");
            log::error!(
                "not replying {:?}: it isn't one",
                String::from_utf8_lossy(bytes)
            );
            return;
        }
        if self.replies.len() + bytes.len() <= MAX_REPLIES {
            self.replies.extend_from_slice(bytes);
        }
    }
}

/// Whether `bytes` are made only as replies are: escape sequences (ESC
/// followed by `[`, `]`, `P` or `\`), BEL to end a string, and letters,
/// digits and the few marks replies hold. No other control, nothing
/// beyond ASCII, and never Enter: nothing a program could have printed to
/// be typed for it.
pub(crate) fn safe_reply(bytes: &[u8]) -> bool {
    let mut after_escape = false;
    bytes.iter().all(|&byte| {
        let ok = if after_escape {
            matches!(byte, b'[' | b']' | b'P' | b'_' | b'\\')
        } else {
            byte == 0x1B
                || byte == 0x07
                || byte.is_ascii_alphanumeric()
                || b";:?$+/=>|.-, ".contains(&byte)
        };
        after_escape = byte == 0x1B && !after_escape;
        ok
    }) && !after_escape
}

/// What ends an answer to an OSC string ended with BEL if `bell`, or
/// else with ST: as the question was, as xterm answers.
fn string_end(bell: bool) -> &'static str {
    if bell { "\x07" } else { "\x1b\\" }
}

/// `title`, or as much of it as a pushed title keeps: no more than
/// [`LONGEST_PUSHED_TITLE`] bytes, cut between characters.
fn shortened(title: &str) -> String {
    let mut end = title.len().min(LONGEST_PUSHED_TITLE);
    while !title.is_char_boundary(end) {
        end -= 1;
    }
    title[..end].to_owned()
}

/// The command a `C` mark's `options` say started, from its
/// `cmdline_url=`, unless it's longer than [`LONGEST_COMMAND`].
fn cmdline(options: &[&[u8]]) -> Option<String> {
    let url = (options.iter()).find_map(|option| option.strip_prefix(b"cmdline_url="))?;
    // Each byte is at most three escaped.
    if url.len() > 3 * LONGEST_COMMAND {
        return None;
    }
    Some(blocks::percent_decode(url)).filter(|command| command.len() <= LONGEST_COMMAND)
}
