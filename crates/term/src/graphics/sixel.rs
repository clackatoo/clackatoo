//! Sixel images (`DCS P1 ; P2 ; P3 q … ST`), as DEC's terminals drew
//! them and img2sixel, chafa and notcurses print them: columns of six
//! pixels, one character each, painted in a colour from a table of
//! registers a program fills.
//!
//! The emulator only collects a sixel's data (see the module above); it's
//! decoded where the terminal's owner decodes images, off the thread the
//! terminal lives on, by [`decode`]: in two walks, the first measuring the
//! image, the second painting it, so nothing is allocated before its size
//! is known to be within bounds.
//!
//! Pixels nothing paints are left see-through, whatever `P2` says, so the
//! terminal's background shows behind them. The pixel aspect ratio
//! (`P1`) is taken as 1:1, as every encoder in use sends.

/// The most colour registers kept: a register numbered past them is the
/// one this many below, as xterm takes it.
pub const MOST_REGISTERS: usize = 1024;

/// The longest side a sixel image may have, in pixels.
pub const MOST_SIDE: u32 = 16384;

/// How many times over an image's pixels its data may paint them: a
/// program going back over its columns (`$`) paints some again, as
/// encoders do for each colour, but data that's cheap to send and paints
/// the same pixels over and over is refused before it's decoded.
pub const MOST_COATS: u64 = 16;

/// A sixel image, decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// Its width in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
    /// Its pixels, four bytes each, red, green, blue and alpha, row by row
    /// from the top.
    pub rgba: Vec<u8>,
}

/// Why a sixel image couldn't be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// It paints nothing, and says no size.
    Empty,
    /// It's bigger than [`MOST_SIDE`] on a side, or has more pixels than
    /// were allowed, or paints them more than [`MOST_COATS`] times over.
    TooBig,
}

/// The 16 colours a VT340's registers start with, as xterm has them.
const VT340: [[u8; 3]; 16] = [
    [0, 0, 0],
    [51, 51, 204],
    [204, 36, 36],
    [51, 204, 51],
    [204, 51, 204],
    [51, 204, 204],
    [204, 204, 51],
    [120, 120, 120],
    [69, 69, 69],
    [87, 87, 153],
    [153, 69, 69],
    [87, 153, 87],
    [153, 87, 153],
    [87, 153, 153],
    [153, 153, 87],
    [204, 204, 204],
];

/// A step of a sixel image's data.
enum Step {
    /// Six pixels of a column, low bit at the top, `count` columns of them.
    Sixel { bits: u8, count: u32 },
    /// Back to the left of the band.
    Return,
    /// Down to the next band of six rows.
    Next,
    /// A colour register chosen, and set if `set` says how.
    Colour {
        register: usize,
        set: Option<[u8; 3]>,
    },
    /// The raster attributes: the image's size, as the encoder says.
    Raster { width: u32, height: u32 },
}

/// Walks `data`, a sixel image's data after its `q`, step by step.
fn walk(data: &[u8], mut each: impl FnMut(Step)) {
    let mut i = 0;
    while let Some(&byte) = data.get(i) {
        i += 1;
        match byte {
            b'?'..=b'~' => each(Step::Sixel {
                bits: byte - b'?',
                count: 1,
            }),
            b'!' => {
                let (count, used) = numbers::<1>(&data[i..]);
                i += used;
                if let Some(&sixel @ b'?'..=b'~') = data.get(i) {
                    i += 1;
                    each(Step::Sixel {
                        bits: sixel - b'?',
                        count: count[0],
                    });
                }
            }
            b'$' => each(Step::Return),
            b'-' => each(Step::Next),
            b'#' => {
                let (params, used) = numbers::<5>(&data[i..]);
                i += used;
                let register = params[0] as usize % MOST_REGISTERS;
                let set = match params[1] {
                    1 => Some(hls(params[2], params[3], params[4])),
                    2 => Some([
                        percent_of(params[2]),
                        percent_of(params[3]),
                        percent_of(params[4]),
                    ]),
                    _ => None,
                };
                each(Step::Colour { register, set });
            }
            b'"' => {
                let (params, used) = numbers::<4>(&data[i..]);
                i += used;
                each(Step::Raster {
                    width: params[2],
                    height: params[3],
                });
            }
            _ => {}
        }
    }
}

/// Up to `N` numbers separated by `;` at the start of `data`, saturating,
/// 0 for those left out, and how many bytes they took.
fn numbers<const N: usize>(data: &[u8]) -> ([u32; N], usize) {
    let mut values = [0u32; N];
    let mut at = 0;
    let mut used = 0;
    for &byte in data {
        match byte {
            b'0'..=b'9' => {
                if let Some(value) = values.get_mut(at) {
                    *value = value
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0'));
                }
            }
            b';' => at += 1,
            _ => break,
        }
        used += 1;
    }
    (values, used)
}

/// A percentage, 0 to 100, as a byte.
fn percent_of(percent: u32) -> u8 {
    u8::try_from(percent.min(100) * 255 / 100).unwrap_or(u8::MAX)
}

/// DEC's hue, lightness and saturation as red, green and blue: DEC's hue
/// puts blue at 0°, red at 120° and green at 240°.
fn hls(hue: u32, lightness: u32, saturation: u32) -> [u8; 3] {
    if saturation == 0 {
        return [percent_of(lightness); 3];
    }
    let hue = f64::from((hue % 360 + 240) % 360) / 360.0;
    let lightness = f64::from(lightness.min(100)) / 100.0;
    let saturation = f64::from(saturation.min(100)) / 100.0;
    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    let channel = |t: f64| {
        let t = t.rem_euclid(1.0);
        let value = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let byte = (value * 255.0).round().clamp(0.0, 255.0) as u8;
        byte
    };
    [
        channel(hue + 1.0 / 3.0),
        channel(hue),
        channel(hue - 1.0 / 3.0),
    ]
}

/// The size `data` paints, or says it is in its raster attributes,
/// whichever is bigger, in pixels, each side no more than [`MOST_SIDE`]:
/// `None` past that.
#[must_use]
pub fn measure(data: &[u8]) -> Option<(u32, u32)> {
    extent(data).map(|(width, height, _)| (width, height))
}

/// As [`measure`], with how many pixels painting it would paint.
fn extent(data: &[u8]) -> Option<(u32, u32, u64)> {
    let (mut x, mut band, mut width, mut height) = (0u32, 0u32, 0u32, 0u32);
    let mut painted = 0u64;
    let mut too_big = false;
    walk(data, |step| match step {
        Step::Sixel { bits, count } => {
            let ink = u64::from(bits.count_ones());
            painted = painted.saturating_add(u64::from(count.min(MOST_SIDE + 1)) * ink);
            x = x.saturating_add(count);
            width = width.max(x);
            if bits != 0 {
                let bottom = 6 - bits.leading_zeros().saturating_sub(2);
                height = height.max(band.saturating_mul(6).saturating_add(bottom));
            }
            too_big |= x > MOST_SIDE;
        }
        Step::Return => x = 0,
        Step::Next => {
            x = 0;
            band = band.saturating_add(1);
            too_big |= band.saturating_mul(6) > MOST_SIDE;
        }
        Step::Colour { .. } => {}
        Step::Raster {
            width: raster_width,
            height: raster_height,
        } => {
            width = width.max(raster_width);
            height = height.max(raster_height);
        }
    });
    (!too_big && width <= MOST_SIDE && height <= MOST_SIDE).then_some((width, height, painted))
}

/// Decodes `data`, a sixel image's data after its `q`, into pixels, if it
/// has no more than `most_pixels`.
///
/// # Errors
///
/// If it's empty, or too big.
pub fn decode(data: &[u8], most_pixels: u64) -> Result<Decoded, Refused> {
    let (width, height, painted) = extent(data).ok_or(Refused::TooBig)?;
    if width == 0 || height == 0 {
        return Err(Refused::Empty);
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > most_pixels || painted > pixels * MOST_COATS {
        return Err(Refused::TooBig);
    }
    let (w, h) = (width as usize, height as usize);
    let mut rgba = vec![0u8; w * h * 4];
    let mut registers = vec![[0u8; 3]; MOST_REGISTERS];
    registers[..VT340.len()].copy_from_slice(&VT340);
    let (mut x, mut band, mut colour) = (0usize, 0usize, VT340[0]);
    walk(data, |step| match step {
        Step::Sixel { bits, count } => {
            let end = x.saturating_add(count as usize).min(w);
            for bit in 0..6 {
                let y = band * 6 + bit;
                if bits >> bit & 1 == 0 || y >= h {
                    continue;
                }
                for pixel in rgba[(y * w + x.min(w)) * 4..(y * w + end) * 4].chunks_exact_mut(4) {
                    pixel.copy_from_slice(&[colour[0], colour[1], colour[2], u8::MAX]);
                }
            }
            x = x.saturating_add(count as usize);
        }
        Step::Return => x = 0,
        Step::Next => {
            x = 0;
            band += 1;
        }
        Step::Colour { register, set } => {
            if let Some(set) = set {
                registers[register] = set;
            }
            colour = registers[register];
        }
        Step::Raster { .. } => {}
    });
    Ok(Decoded {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sixel_paints_six_rows_of_a_column_in_the_chosen_colour() {
        // Register 1 red, then a full column twice, then the top pixel.
        let image = decode(b"#1;2;100;0;0#1~~@", 1 << 20).unwrap();
        assert_eq!((image.width, image.height), (3, 6));
        assert_eq!(&image.rgba[..4], &[255, 0, 0, 255]);
        // The third column has its top pixel only.
        assert_eq!(&image.rgba[8..12], &[255, 0, 0, 255]);
        assert_eq!(&image.rgba[(3 + 2) * 4..(3 + 2) * 4 + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn repeats_bands_and_raster_attributes_size_the_image() {
        assert_eq!(measure(b"!10~-!3~"), Some((10, 12)));
        assert_eq!(measure(b"\"1;1;40;30!3~"), Some((40, 30)));
        assert_eq!(measure(b"@"), Some((1, 1)));
        assert_eq!(measure(b""), Some((0, 0)));
        assert_eq!(decode(b"", 100), Err(Refused::Empty));
    }

    #[test]
    fn a_sixel_too_big_is_refused_before_anything_is_allocated() {
        assert_eq!(measure(b"!99999~"), None);
        assert_eq!(decode(b"!4000~", 1000), Err(Refused::TooBig));
        let bands = "-".repeat(3000);
        assert_eq!(measure(format!("~{bands}~").as_bytes()), None);
        // Numbers saturate rather than overflow.
        assert_eq!(measure(b"!99999999999999999999~"), None);
        assert_eq!(
            decode(b"#99999999999;2;1;1;1~", 100).map(|i| i.width),
            Ok(1)
        );
    }

    #[test]
    fn data_painting_the_same_pixels_over_and_over_is_refused() {
        // A full band, painted again once per colour, as encoders do.
        let coats = |times: usize| "#1!100~$".repeat(times);
        assert!(decode(coats(4).as_bytes(), 1 << 20).is_ok());
        let over = coats(usize::try_from(MOST_COATS).unwrap() + 1);
        assert_eq!(decode(over.as_bytes(), 1 << 20), Err(Refused::TooBig));
        // However wide the repeat says, and however often.
        let wide = "!16384~$".repeat(4000);
        assert_eq!(decode(wide.as_bytes(), 1 << 30), Err(Refused::TooBig));
    }

    #[test]
    fn hls_colours_are_decs() {
        // DEC's hue 120 is red, 240 green, 0 blue.
        assert_eq!(hls(120, 50, 100), [255, 0, 0]);
        assert_eq!(hls(240, 50, 100), [0, 255, 0]);
        assert_eq!(hls(0, 50, 100), [0, 0, 255]);
        assert_eq!(hls(0, 100, 0), [255, 255, 255]);
    }
}
