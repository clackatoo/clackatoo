//! Terminals' images (CLACKA-239, part 2): what their programs send with
//! kitty's graphics protocol, sixel and iTerm2's inline images, kept in
//! the one image store every pane's images are (see `crate::images`),
//! as the terminal's own.
//!
//! A terminal waits while its image is kept: its output is held, and its
//! PTY not read, until the store says what came of it, which may take a
//! moment for a big image, and as long as reading a file may (see
//! `Images::keep`). Then the terminal goes on, and its PTY is read again.
//!
//! The images a terminal's screen shows are pinned, so it's those it
//! shows least lately that go when it holds more than the limit for a
//! terminal's images; those it no longer shows or keeps it lets go of.
//!
//! What's being sent counts too: an image sent in parts, or a sixel or
//! iTerm2 image's data, may take only the room the limit for all images
//! leaves, with what the store and the other terminals hold, and one
//! that's still being sent after [`SENDING_WAIT`] is given up on.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use clackatoo_protocol::{ImageFrom, ImageId, ImageInfo, TerminalId};
use clackatoo_term::ImageKept;

use super::{Daemon, Flood};
use crate::images::{Source, Why, error_of};

/// How long an image may be in the sending, in parts, before it's given
/// up on, as one whose last part never comes.
pub(super) const SENDING_WAIT: Duration = Duration::from_secs(30);

impl Daemon {
    /// Takes in what terminal `id`'s output did to its images: those to
    /// keep, those let go of, and those its screen shows now.
    pub(super) fn terminal_images(&mut self, id: TerminalId) {
        let limits = self.images.limits();
        let kept = self.images.bytes(None);
        let Some(terminal) = self
            .terminals
            .get_mut(&id)
            .and_then(|slot| slot.terminal.pty())
        else {
            return;
        };
        terminal.set_image_limit(limits.per_owner);
        // What's being sent may take what room all images leave.
        let others = self.pictures.sending_besides(id);
        let room = (limits.total.saturating_sub(kept)).saturating_sub(others);
        terminal.set_upload_room(usize::try_from(room).unwrap_or(usize::MAX));
        let sending = terminal.sending_images() as u64;
        let parts = terminal.sending_parts();
        self.pictures
            .note_sending(id, sending, parts, Instant::now());
        let requests = terminal.take_image_requests();
        let released = terminal.take_released_images();
        let shown = (terminal.emulator().has_images() || self.pictures.pins(id).is_some())
            .then(|| terminal.emulator().shown_images());
        let owner = ImageFrom::Terminal(id);
        let mut refused = Vec::new();
        for request in requests {
            let file = crate::images::in_file(&request.source);
            let why = Why::Terminal {
                terminal: id,
                token: request.token,
                file,
            };
            if let Err(why) = self
                .images
                .keep(owner, why, Source::Terminal(request.source))
            {
                refused.push((request.token, error_of(&why, file)));
            }
        }
        for key in released {
            self.images.let_go(owner, ImageId(key));
        }
        if let Some(shown) = shown {
            self.pin_terminal_images(id, shown.into_iter().map(ImageId).collect());
        }
        for (token, error) in refused {
            let _ = self.terminal_image_kept(id, token, Err(error));
        }
    }

    /// Pins the images terminal `id`'s screen shows, `shown`, and unpins
    /// those it no longer does.
    fn pin_terminal_images(&mut self, id: TerminalId, shown: HashSet<ImageId>) {
        let pinned = self.pictures.take_pins(id);
        for &image in shown.difference(&pinned) {
            self.images.pin(image);
        }
        for &image in pinned.difference(&shown) {
            self.images.unpin(image);
        }
        self.pictures.set_pins(id, shown);
    }

    /// Gives up on the images still being sent past [`SENDING_WAIT`].
    pub(super) fn abandon_stale_sending(&mut self) {
        for id in self.pictures.stale_sending(Instant::now()) {
            if let Some(terminal) =
                (self.terminals.get_mut(&id)).and_then(|slot| slot.terminal.pty())
            {
                terminal.abandon_sending();
            }
            self.pictures.note_sending(id, 0, false, Instant::now());
        }
    }

    /// The store kept an image terminal `terminal` asked for with `token`,
    /// or didn't, as `kept` says: in a file, if `file`. One the terminal
    /// no longer waits for, as it gave up on it, is let go.
    pub(super) fn terminal_image_stored(
        &mut self,
        terminal: TerminalId,
        token: u64,
        file: bool,
        kept: Result<ImageInfo, String>,
    ) {
        let image = kept.as_ref().ok().map(|info| info.id);
        let kept = kept
            .map(|info| ImageKept {
                key: info.id.0,
                width: info.width,
                height: info.height,
            })
            .map_err(|why| {
                log::debug!("terminal {terminal}: an image wasn't kept: {why}");
                error_of(&why, file)
            });
        if !self.terminal_image_kept(terminal, token, kept)
            && let Some(image) = image
        {
            self.images.let_go(ImageFrom::Terminal(terminal), image);
        }
    }

    /// Tells terminal `id` what came of its image `token`: it goes on with
    /// its output, and its PTY is read again once it waits no more. Says
    /// whether it waited for it.
    fn terminal_image_kept(
        &mut self,
        id: TerminalId,
        token: u64,
        kept: Result<ImageKept, clackatoo_term::ImageError>,
    ) -> bool {
        let Some(terminal) = self
            .terminals
            .get_mut(&id)
            .and_then(|slot| slot.terminal.pty())
        else {
            return false;
        };
        if !terminal.image_kept(token, kept) {
            return false;
        }
        let waiting = terminal.waiting_for_image();
        self.update_pty_interest(id);
        self.output_taken(id);
        if !waiting {
            self.queue_flood(Flood::Terminal(id));
        }
        true
    }

    /// Has every terminal waiting for an image go on as if it couldn't be
    /// kept, as before a handover.
    pub(super) fn give_up_terminal_images(&mut self) {
        let waiting: Vec<TerminalId> = (self.terminals.iter_mut())
            .filter_map(|(&id, slot)| Some((id, slot.terminal.pty()?)))
            .filter(|(_, terminal)| terminal.waiting_for_image())
            .map(|(id, _)| id)
            .collect();
        for id in waiting {
            if let Some(terminal) = self
                .terminals
                .get_mut(&id)
                .and_then(|slot| slot.terminal.pty())
            {
                terminal.give_up_images();
            }
            self.output_taken(id);
        }
    }
}
