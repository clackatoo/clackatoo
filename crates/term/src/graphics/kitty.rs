//! The kitty graphics protocol's commands: `APC G keys ; payload ST`,
//! where `keys` is `key=value` pairs separated by commas, each key one
//! letter and each value a number or a letter, and the payload base64.
//!
//! Only reading a command is here; what each does is the emulator's (see
//! the module above).

/// The most bytes a command's keys may take: kitty's longest command
/// takes well under a hundred.
pub(crate) const LONGEST_KEYS: usize = 1024;

/// One command's keys, as kitty defaults them.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "kitty's keys, one field each"
)]
pub(crate) struct Control {
    /// `a`: what to do: `t` transmit, `T` transmit and show, `q` ask
    /// whether it could be, `p` show one transmitted, `d` delete, and
    /// `f`, `a` and `c` for animation, which isn't done.
    pub(crate) action: u8,
    /// `q`: 1 says no `OK`, 2 nothing at all.
    pub(crate) quiet: u32,
    /// `f`: 24 RGB, 32 RGBA, 100 PNG.
    pub(crate) format: u32,
    /// `t`: `d` in the payload, `f` a file, `t` a temporary file, `s`
    /// shared memory.
    pub(crate) medium: u8,
    /// `s`, `v`: the pixels' width and height, for RGB and RGBA.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// `S`, `O`: how many bytes to read of a file, from where.
    pub(crate) size: u32,
    pub(crate) offset: u32,
    /// `o=z`: the data is compressed with zlib.
    pub(crate) compressed: bool,
    /// `m=1`: more of the data comes in the next command.
    pub(crate) more: bool,
    /// `i`: the image's ID, as the program numbers it.
    pub(crate) id: u32,
    /// `I`: a number the program knows the image by, whose ID the
    /// terminal picks and says.
    pub(crate) number: u32,
    /// `p`: the placement's ID.
    pub(crate) placement: u32,
    /// `x`, `y`, `w`, `h`: the part of the image shown, in its pixels;
    /// or, deleting, the cell or column (`x`) and row (`y`).
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) w: u32,
    pub(crate) h: u32,
    /// `X`, `Y`: how far into its first cell the image starts, in pixels.
    pub(crate) cell_x: u32,
    pub(crate) cell_y: u32,
    /// `c`, `r`: the columns and rows it's shown over.
    pub(crate) cols: u32,
    pub(crate) rows: u32,
    /// `C=1`: the cursor stays where it is.
    pub(crate) stay: bool,
    /// `U=1`: a virtual placement, for Unicode placeholders.
    pub(crate) virtual_: bool,
    /// `z`: above (0 and up) or below the text, and other images.
    pub(crate) z: i32,
    /// `d`: what a delete deletes.
    pub(crate) delete: u8,
    /// `P`, `Q`: a parent placement, which isn't done.
    pub(crate) parent: bool,
    /// It has no key but `i`, `I` and `p`, as an answer has: an answer
    /// echoed back, as by a program that doesn't read it, is never a
    /// command, or it would answer itself for ever.
    pub(crate) answer_like: bool,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            action: b't',
            quiet: 0,
            format: 32,
            medium: b'd',
            width: 0,
            height: 0,
            size: 0,
            offset: 0,
            compressed: false,
            more: false,
            id: 0,
            number: 0,
            placement: 0,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            cell_x: 0,
            cell_y: 0,
            cols: 0,
            rows: 0,
            stay: false,
            virtual_: false,
            z: 0,
            delete: b'a',
            parent: false,
            answer_like: true,
        }
    }
}

impl Control {
    /// The keys `keys` gives, the `G` before them taken off already; or
    /// `None` if they're no command's: a key that isn't one letter, or a
    /// value that's neither a number nor a letter.
    pub(crate) fn parse(keys: &[u8]) -> Option<Self> {
        let mut control = Self::default();
        for pair in keys.split(|&byte| byte == b',') {
            if pair.is_empty() {
                continue;
            }
            let [key, b'=', value @ ..] = pair else {
                return None;
            };
            let letter = match value {
                [letter] if letter.is_ascii_alphabetic() => Some(*letter),
                _ => None,
            };
            let number = || -> Option<i64> {
                let (negative, digits) = match value {
                    [b'-', digits @ ..] => (true, digits),
                    digits => (false, digits),
                };
                if digits.is_empty() || digits.len() > 11 || !digits.iter().all(u8::is_ascii_digit)
                {
                    return None;
                }
                let value =
                    (digits.iter()).fold(0i64, |sum, digit| sum * 10 + i64::from(digit - b'0'));
                Some(if negative { -value } else { value })
            };
            let unsigned = || number().and_then(|value| u32::try_from(value).ok());
            control.answer_like &= matches!(key, b'i' | b'I' | b'p');
            match key {
                b'a' => control.action = letter?,
                b't' => control.medium = letter?,
                b'd' => control.delete = letter?,
                b'o' => control.compressed = letter? == b'z',
                b'q' => control.quiet = unsigned()?,
                b'f' => control.format = unsigned()?,
                b's' => control.width = unsigned()?,
                b'v' => control.height = unsigned()?,
                b'S' => control.size = unsigned()?,
                b'O' => control.offset = unsigned()?,
                b'm' => control.more = unsigned()? == 1,
                b'i' => control.id = unsigned()?,
                b'I' => control.number = unsigned()?,
                b'p' => control.placement = unsigned()?,
                b'x' => control.x = unsigned()?,
                b'y' => control.y = unsigned()?,
                b'w' => control.w = unsigned()?,
                b'h' => control.h = unsigned()?,
                b'X' => control.cell_x = unsigned()?,
                b'Y' => control.cell_y = unsigned()?,
                b'c' => control.cols = unsigned()?,
                b'r' => control.rows = unsigned()?,
                b'C' => control.stay = unsigned()? == 1,
                b'U' => control.virtual_ = unsigned()? == 1,
                b'z' => control.z = i32::try_from(number()?).ok()?,
                b'P' | b'Q' => control.parent |= unsigned()? != 0,
                // Other keys are animation's, or newer than this.
                _ => {}
            }
        }
        Some(control)
    }

    /// Whether an answer is to be sent: one is asked for by an ID or a
    /// number, and `q` doesn't keep it quiet, `OK` if `ok`, else an
    /// error.
    pub(crate) fn answers(&self, ok: bool) -> bool {
        (self.id != 0 || self.number != 0) && if ok { self.quiet == 0 } else { self.quiet < 2 }
    }
}

/// A fixed error an answer says: never anything a program sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    /// A command that doesn't make sense, or isn't done.
    Invalid,
    /// No image of that ID or number.
    NoImage,
    /// A file that couldn't be read: the same for every reason, so a
    /// program can't find out what files there are.
    Unreadable,
    /// The data isn't an image that can be shown.
    BadData,
    /// The image is bigger than the limit for a terminal's images.
    TooBig,
    /// Too much is being done with images just now.
    Busy,
}

impl Error {
    /// What the answer says.
    pub(crate) fn words(self) -> &'static str {
        match self {
            Self::Invalid => "EINVAL:the command is not one that can be done",
            Self::NoImage => "ENOENT:no such image",
            Self::Unreadable => "EBADF:Failed to read image file",
            Self::BadData => "ENODATA:the data is not an image that can be shown",
            Self::TooBig => "EFBIG:the image is over the limit for images",
            Self::Busy => "EBUSY:too many images at once, try again",
        }
    }
}

/// The answer to a command whose keys were `control`, as kitty sends it:
/// the IDs it had, parsed, and `words`, which are fixed.
pub(crate) fn answer(control: &Control, id: u32, words: &str) -> String {
    use std::fmt::Write as _;
    let mut answer = format!("\x1b_Gi={id}");
    if control.number != 0 {
        let _ = write!(answer, ",I={}", control.number);
    }
    if control.placement != 0 {
        let _ = write!(answer, ",p={}", control.placement);
    }
    answer.push(';');
    answer.push_str(words);
    answer.push_str("\x1b\\");
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_read_as_kitty_reads_them() {
        let control = Control::parse(b"a=T,f=100,i=31,q=2,z=-5,C=1,m=1").unwrap();
        assert_eq!(control.action, b'T');
        assert_eq!(control.format, 100);
        assert_eq!(control.id, 31);
        assert_eq!(control.quiet, 2);
        assert_eq!(control.z, -5);
        assert!(control.stay && control.more);
        assert_eq!(Control::parse(b"").unwrap(), Control::default());
        assert_eq!(Control::parse(b",a=p,").unwrap().action, b'p');
    }

    #[test]
    fn keys_that_arent_a_commands_are_refused() {
        assert!(Control::parse(b"a=Tx").is_none());
        assert!(Control::parse(b"ab=T").is_none());
        assert!(Control::parse(b"i=-1").is_none());
        assert!(Control::parse(b"i=99999999999").is_none());
        assert!(Control::parse(b"z=99999999999").is_none());
        assert!(Control::parse(b"a=\x1b").is_none());
    }
}
