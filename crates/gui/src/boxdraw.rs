//! Box-drawing and block characters, drawn to fill their cells exactly.
//!
//! Fonts draw these to fit the font's own line height, so lines don't meet
//! across rows once a cell is taller (and rarely line up across columns
//! either). Drawing them here makes borders in htop, tmux or editors join
//! seamlessly at any size.

/// How heavily one arm of a box-drawing character is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Weight {
    None,
    Light,
    Heavy,
    Double,
}

/// The arms of U+2500 to U+257F, one per character, as up, right, down
/// and left: `.` none, `l` light, `h` heavy, `d` double. Dashed lines, arcs
/// and diagonals are drawn separately and marked `-`, `(` and `x`.
const ARMS: [&str; 128] = [
    // U+2500
    ".l.l", ".h.h", "l.l.", "h.h.", "-", "-", "-", "-", "-", "-", "-", "-", ".ll.", ".hl.", ".lh.",
    ".hh.", // U+2510
    "..ll", "..lh", "..hl", "..hh", "ll..", "lh..", "hl..", "hh..", "l..l", "l..h", "h..l", "h..h",
    "lll.", "lhl.", "hll.", "llh.", // U+2520
    "hlh.", "hhl.", "lhh.", "hhh.", "l.ll", "l.lh", "h.ll", "l.hl", "h.hl", "h.lh", "l.hh", "h.hh",
    ".lll", ".llh", ".hll", ".hlh", // U+2530
    ".lhl", ".lhh", ".hhl", ".hhh", "ll.l", "ll.h", "lh.l", "lh.h", "hl.l", "hl.h", "hh.l", "hh.h",
    "llll", "lllh", "lhll", "lhlh", // U+2540
    "hlll", "llhl", "hlhl", "hllh", "hhll", "llhh", "lhhl", "hhlh", "lhhh", "hlhh", "hhhl", "hhhh",
    "-", "-", "-", "-", // U+2550
    ".d.d", "d.d.", ".dl.", ".ld.", ".dd.", "..ld", "..dl", "..dd", "ld..", "dl..", "dd..", "l..d",
    "d..l", "d..d", "ldl.", "dld.", // U+2560
    "ddd.", "l.ld", "d.dl", "d.dd", ".dld", ".ldl", ".ddd", "ld.d", "dl.l", "dd.d", "ldld", "dldl",
    "dddd", "(", "(", "(", // U+2570
    "(", "x", "x", "x", "...l", "l...", ".l..", "..l.", "...h", "h...", ".h..", "..h.", ".h.l",
    "l.h.", ".l.h", "h.l.",
];

/// Whether `ch` is drawn here rather than by a font.
pub(crate) fn is_drawn(ch: char) -> bool {
    match ch {
        '\u{2500}'..='\u{257F}' => ARMS[ch as usize - 0x2500] != "x",
        '\u{2580}'..='\u{259F}' => true,
        _ => false,
    }
}

/// `ch` as a coverage mask filling a `width` × `height` cell, with light
/// lines `stroke` pixels thick. `None` if it isn't drawn here.
pub(crate) fn rasterize(ch: char, width: u32, height: u32, stroke: u32) -> Option<Vec<u8>> {
    let mut canvas = Canvas::new(width, height);
    let light = stroke.max(1);
    match ch {
        '\u{2500}'..='\u{257F}' => {
            let code = ch as u32 - 0x2500;
            match ARMS[code as usize] {
                "x" => return None,
                "-" => canvas.dashes(code, light),
                "(" => canvas.arc(code, light),
                arms => {
                    let mut weights = [Weight::None; 4];
                    for (weight, symbol) in weights.iter_mut().zip(arms.bytes()) {
                        *weight = match symbol {
                            b'l' => Weight::Light,
                            b'h' => Weight::Heavy,
                            b'd' => Weight::Double,
                            _ => Weight::None,
                        };
                    }
                    canvas.lines(weights, light);
                }
            }
        }
        '\u{2580}'..='\u{259F}' => canvas.block(ch),
        _ => return None,
    }
    Some(canvas.pixels)
}

/// A coverage mask being drawn.
struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width * height) as usize],
        }
    }

    /// Fills the pixels from `(x0, y0)` up to `(x1, y1)` with `alpha`,
    /// clipped to the canvas.
    fn fill(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, alpha: u8) {
        for y in y0.min(self.height)..y1.min(self.height) {
            let row = (y * self.width) as usize;
            for x in x0.min(self.width)..x1.min(self.width) {
                self.pixels[row + x as usize] = alpha;
            }
        }
    }

    /// The span across a line's axis that a band of `thickness` centred in
    /// `size` covers.
    fn band(size: u32, thickness: u32) -> (u32, u32) {
        let start = size.saturating_sub(thickness) / 2;
        (start, start + thickness)
    }

    /// How thick a line of `weight` is. Doubles are two light lines with a
    /// light line's gap between.
    fn thickness(weight: Weight, light: u32) -> u32 {
        match weight {
            Weight::None => 0,
            Weight::Light => light,
            Weight::Heavy => light * 2,
            Weight::Double => light * 3,
        }
    }

    /// Draws arms out from the centre: up, right, down, left. Each arm runs
    /// from its edge to the far side of the arms crossing it, so joins are
    /// filled whatever the weights.
    fn lines(&mut self, [up, right, down, left]: [Weight; 4], light: u32) {
        let vertical = Self::thickness(up, light).max(Self::thickness(down, light));
        let horizontal = Self::thickness(left, light).max(Self::thickness(right, light));
        // Where the crossing arms run, or the arm's own width for a lone arm.
        let (v0, v1) = Self::band(self.width, if vertical == 0 { light } else { vertical });
        let (h0, h1) = Self::band(
            self.height,
            if horizontal == 0 { light } else { horizontal },
        );
        let (w, h) = (self.width, self.height);
        let has = |weight| weight != Weight::None;
        // A double arm's two lines meet the crossing arms' lines: the one
        // on the side of a crossing arm stops at that arm's nearer line,
        // the other runs on to the far one, as in ╔ and ╬.
        let near_top = h0 + light;
        let near_bottom = h1.saturating_sub(light);
        let near_left = v0 + light;
        let near_right = v1.saturating_sub(light);
        let up_ends = [
            if has(left) { near_top } else { h1 },
            if has(right) { near_top } else { h1 },
        ];
        let down_starts = [
            if has(left) { near_bottom } else { h0 },
            if has(right) { near_bottom } else { h0 },
        ];
        let left_ends = [
            if has(up) { near_left } else { v1 },
            if has(down) { near_left } else { v1 },
        ];
        let right_starts = [
            if has(up) { near_right } else { v0 },
            if has(down) { near_right } else { v0 },
        ];
        self.stroke_vertical(up, light, [(0, up_ends[0]), (0, up_ends[1])], (0, h1));
        self.stroke_vertical(
            down,
            light,
            [(down_starts[0], h), (down_starts[1], h)],
            (h0, h),
        );
        self.stroke_horizontal(left, light, [(0, left_ends[0]), (0, left_ends[1])], (0, v1));
        self.stroke_horizontal(
            right,
            light,
            [(right_starts[0], w), (right_starts[1], w)],
            (v0, w),
        );
    }

    /// A vertical arm over `along`, or for a double arm, its left and right
    /// lines over `doubles`.
    fn stroke_vertical(
        &mut self,
        weight: Weight,
        light: u32,
        doubles: [(u32, u32); 2],
        (y0, y1): (u32, u32),
    ) {
        let (x0, x1) = Self::band(self.width, Self::thickness(weight, light));
        match weight {
            Weight::None => {}
            Weight::Double => {
                self.fill(x0, doubles[0].0, x0 + light, doubles[0].1, 255);
                self.fill(x1 - light, doubles[1].0, x1, doubles[1].1, 255);
            }
            _ => self.fill(x0, y0, x1, y1, 255),
        }
    }

    /// A horizontal arm over `along`, or for a double arm, its top and
    /// bottom lines over `doubles`.
    fn stroke_horizontal(
        &mut self,
        weight: Weight,
        light: u32,
        doubles: [(u32, u32); 2],
        (x0, x1): (u32, u32),
    ) {
        let (y0, y1) = Self::band(self.height, Self::thickness(weight, light));
        match weight {
            Weight::None => {}
            Weight::Double => {
                self.fill(doubles[0].0, y0, doubles[0].1, y0 + light, 255);
                self.fill(doubles[1].0, y1 - light, doubles[1].1, y1, 255);
            }
            _ => self.fill(x0, y0, x1, y1, 255),
        }
    }

    /// Dashed lines: U+2504 to U+250B (three and four dashes) and U+254C to
    /// U+254F (two).
    fn dashes(&mut self, code: u32, light: u32) {
        let (dashes, offset) = match code {
            0x04..=0x07 => (3, 0x04),
            0x08..=0x0B => (4, 0x08),
            _ => (2, 0x4C),
        };
        let index = code - offset;
        let heavy = index % 2 == 1;
        let vertical = index >= 2;
        let thickness = if heavy { light * 2 } else { light };
        let length = if vertical { self.height } else { self.width };
        let slot = length / dashes;
        for dash in 0..dashes {
            // Each dash fills most of its slot, with the gap split around it.
            let start = dash * slot + slot / 4;
            let end = start + slot / 2 + slot % 2;
            if vertical {
                let (x0, x1) = Self::band(self.width, thickness);
                self.fill(x0, start, x1, end, 255);
            } else {
                let (y0, y1) = Self::band(self.height, thickness);
                self.fill(start, y0, end, y1, 255);
            }
        }
    }

    /// The rounded corners U+256D to U+2570: a quarter ellipse joining the
    /// middles of two edges, smoothed by sampling each pixel 16 times.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn arc(&mut self, code: u32, light: u32) {
        let (w, h) = (self.width as f32, self.height as f32);
        let half = light as f32 / 2.0;
        // The middle of a straight line through the cell, so arcs meet the
        // lines beside them.
        let x_mid = Self::band(self.width, light).0 as f32 + half;
        let y_mid = Self::band(self.height, light).0 as f32 + half;
        // The ellipse's centre is the corner the arc curves around.
        let (cx, cy) = match code {
            0x6D => (w, h),
            0x6E => (0.0, h),
            0x6F => (0.0, 0.0),
            _ => (w, 0.0),
        };
        let rx = (cx - x_mid).abs();
        let ry = (cy - y_mid).abs();
        for y in 0..self.height {
            for x in 0..self.width {
                let mut covered = 0u32;
                for sample in 0..16 {
                    let px = x as f32 + (sample % 4) as f32 / 4.0 + 0.125 - cx;
                    let py = y as f32 + (sample / 4) as f32 / 4.0 + 0.125 - cy;
                    // The ellipse's equation, divided by its gradient's
                    // length, approximates the distance from it.
                    let f = (px / rx).powi(2) + (py / ry).powi(2) - 1.0;
                    let gradient =
                        2.0 * ((px / (rx * rx)).powi(2) + (py / (ry * ry)).powi(2)).sqrt();
                    if gradient > 0.0 && (f / gradient).abs() <= half {
                        covered += 1;
                    }
                }
                self.pixels[(y * self.width + x) as usize] = (covered * 255 / 16) as u8;
            }
        }
    }

    /// The block elements U+2580 to U+259F.
    fn block(&mut self, ch: char) {
        let (w, h) = (self.width, self.height);
        let eighths = |size: u32, n: u32| (size * n + 4) / 8;
        let (hw, hh) = (w / 2, h / 2);
        match ch {
            '▀' => self.fill(0, 0, w, hh, 255),
            '\u{2581}'..='\u{2588}' => {
                let n = ch as u32 - 0x2580;
                self.fill(0, h - eighths(h, n), w, h, 255);
            }
            '\u{2589}'..='\u{258F}' => {
                let n = 8 - (ch as u32 - 0x2588);
                self.fill(0, 0, eighths(w, n), h, 255);
            }
            '▐' => self.fill(hw, 0, w, h, 255),
            '░' => self.fill(0, 0, w, h, 64),
            '▒' => self.fill(0, 0, w, h, 128),
            '▓' => self.fill(0, 0, w, h, 192),
            '▔' => self.fill(0, 0, w, eighths(h, 1), 255),
            '▕' => self.fill(w - eighths(w, 1), 0, w, h, 255),
            _ => {
                // Quadrants, as upper left, upper right, lower left, lower
                // right.
                let [ul, ur, ll, lr] = match ch {
                    '▖' => [false, false, true, false],
                    '▗' => [false, false, false, true],
                    '▘' => [true, false, false, false],
                    '▙' => [true, false, true, true],
                    '▚' => [true, false, false, true],
                    '▛' => [true, true, true, false],
                    '▜' => [true, true, false, true],
                    '▝' => [false, true, false, false],
                    '▞' => [false, true, true, false],
                    _ => [false, true, true, true],
                };
                for (on, x0, y0, x1, y1) in [
                    (ul, 0, 0, hw, hh),
                    (ur, hw, 0, w, hh),
                    (ll, 0, hh, hw, h),
                    (lr, hw, hh, w, h),
                ] {
                    if on {
                        self.fill(x0, y0, x1, y1, 255);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
// Counting a few hundred pixels needs no faster crate.
#[allow(clippy::naive_bytecount)]
mod tests {
    use super::*;

    fn mask(ch: char) -> Vec<u8> {
        rasterize(ch, 10, 20, 1).expect("drawn here")
    }

    fn at(mask: &[u8], x: u32, y: u32) -> u8 {
        mask[(y * 10 + x) as usize]
    }

    #[test]
    fn lines_reach_the_edges_they_join() {
        let cross = mask('┼');
        // The middle of every edge is covered, so neighbours connect.
        assert_eq!(at(&cross, 4, 0), 255);
        assert_eq!(at(&cross, 4, 19), 255);
        assert_eq!(at(&cross, 0, 9), 255);
        assert_eq!(at(&cross, 9, 9), 255);
        // Corners stay empty.
        assert_eq!(at(&cross, 0, 0), 0);

        let corner = mask('┌');
        assert_eq!(at(&corner, 4, 19), 255);
        assert_eq!(at(&corner, 9, 9), 255);
        assert_eq!(at(&corner, 4, 0), 0);
        assert_eq!(at(&corner, 0, 9), 0);
    }

    #[test]
    fn heavy_lines_are_thicker() {
        let light = mask('─').iter().filter(|&&a| a > 0).count();
        let heavy = mask('━').iter().filter(|&&a| a > 0).count();
        assert_eq!(heavy, light * 2);
    }

    #[test]
    fn arcs_meet_the_lines_beside_them() {
        let arc = mask('╭');
        let line = mask('│');
        // Where the arc leaves the bottom edge, a vertical line is drawn.
        let bottom = |m: &[u8]| (0..10).map(|x| at(m, x, 19) > 128).collect::<Vec<_>>();
        assert_eq!(bottom(&arc), bottom(&line));
    }

    #[test]
    fn blocks_fill_their_fraction() {
        let lower = mask('▄');
        assert_eq!(lower.iter().filter(|&&a| a == u8::MAX).count(), 100);
        assert_eq!(at(&lower, 0, 19), 255);
        assert_eq!(at(&lower, 0, 0), 0);
        assert!(mask('█').iter().all(|&a| a == 255));
    }

    #[test]
    fn diagonals_are_left_to_the_font() {
        assert!(!is_drawn('╱'));
        assert!(rasterize('╳', 10, 20, 1).is_none());
        assert!(is_drawn('─') && is_drawn('▒'));
    }
}
