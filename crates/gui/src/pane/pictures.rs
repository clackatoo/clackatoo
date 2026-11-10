//! What a terminal's rows in view show of pictures (CLACKA-239): the
//! pieces of those its program placed, which its rows hold, and what
//! kitty's Unicode placeholders in its text show. The images themselves
//! come through the window's one image layer (see `crate::images`).

use clackatoo_term::graphics::{self, Slice};

use super::{Document, Pane};

impl Pane {
    /// Whether its screen shows a picture its program sent, or a
    /// placeholder may.
    pub(crate) fn shows_pictures(&self) -> bool {
        self.screen.as_ref().is_some_and(|screen| {
            !screen.virtuals.is_empty() || screen.rows.iter().any(|row| !row.pieces().is_empty())
        })
    }

    /// The parts of pictures the `height` rows in view show, each with
    /// its row in view, in cells `cell` pixels big.
    pub(crate) fn picture_slices(&self, height: usize, cell: [f32; 2]) -> Vec<(usize, Slice)> {
        let Some(screen) = &self.screen else {
            return Vec::new();
        };
        let top = self.view_top();
        let document = Document(self);
        let mut slices = Vec::new();
        for at in 0..height {
            let Some(row) = document.line(top + at as u64) else {
                continue;
            };
            for &piece in row.pieces() {
                if let Some(slice) =
                    (screen.pictures.get(&piece.picture)).and_then(|picture| picture.slice(piece))
                {
                    slices.push((at, slice));
                }
            }
            if screen.virtuals.is_empty() || !graphics::has_placeholders(row) {
                continue;
            }
            for (col, said) in graphics::placeholders(row) {
                let placement = (screen.virtuals.iter()).find(|placement| {
                    placement.id == said.image
                        && (said.placement == 0 || placement.placement == said.placement)
                });
                if let Some(mut slice) =
                    placement.and_then(|placement| placement.slice(said.row, said.col, cell))
                {
                    #[allow(clippy::cast_precision_loss, reason = "columns are few")]
                    let col = col as f32;
                    slice.rect[0] += col;
                    slices.push((at, slice));
                }
            }
        }
        slices
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{TerminalId, View, ViewUpdate};
    use clackatoo_term::{Config, ImageKept, Size, Terminal};

    use super::*;

    /// A 12 × 5 terminal, its cells 10 by 20 pixels, after `output`, the
    /// image it sends kept as `kept`.
    fn terminal(output: &[u8], kept: ImageKept) -> Terminal {
        let mut terminal = Terminal::new(Size::new(12, 5).unwrap(), &Config::default());
        terminal.set_cell_pixels(Some([10, 20]));
        let used = terminal.advance_until_waiting(output);
        for request in terminal.take_image_requests() {
            terminal.image_kept(request.token, Ok(kept));
        }
        terminal.advance(&output[used..]);
        terminal
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "halves and wholes, exactly")]
    fn a_picture_and_a_placeholder_show_in_the_rows_in_view() {
        let kept = ImageKept {
            key: [3; 16],
            width: 20,
            height: 40,
        };
        // A picture two columns and two rows big from the second column,
        // then a virtual placement and a placeholder on the third row.
        let output = "x\x1b_Ga=T,f=100,i=1,q=2;AAAA\x1b\\\r\n\
                      \x1b_Ga=p,U=1,i=1,c=1,r=1,q=2\x1b\\\
                      \x1b[38;5;1m\u{10EEEE}\u{0305}\u{0305}";
        let terminal = terminal(output.as_bytes(), kept);
        let mut pane = Pane::new(TerminalId(1));
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal)));
        let slices = pane.picture_slices(5, [10.0, 20.0]);
        let shown: Vec<(usize, [f32; 4])> = (slices.iter())
            .map(|(at, slice)| (*at, slice.rect))
            .collect();
        assert_eq!(
            shown,
            [
                (0, [1.0, 0.0, 2.0, 1.0]),
                (1, [1.0, 0.0, 2.0, 1.0]),
                // 20 by 40 fitted to one cell, 10 by 20: all of it.
                (2, [0.0, 0.0, 1.0, 1.0]),
            ]
        );
        assert_eq!(slices[1].1.crop, [0.0, 20.0, 20.0, 20.0]);
        assert!(slices.iter().all(|(_, slice)| slice.image == [3; 16]));
    }
}
