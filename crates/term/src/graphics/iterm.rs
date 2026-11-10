//! iTerm2's inline images: `OSC 1337 ; File = args : base64 ST`, as
//! `imgcat` prints them, where `args` are `key=value` pairs separated by
//! `;`. A file that isn't `inline=1` is a download, which isn't done.
//! Nor is iTerm2's later `MultipartFile`, which sends a file in parts.

/// How big an image is to show, on one side, as `width` or `height` say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Side {
    /// As big as the image is.
    #[default]
    Auto,
    /// This many cells.
    Cells(u32),
    /// This many pixels.
    Pixels(u32),
    /// This much of the terminal, in hundredths.
    Percent(u32),
}

impl Side {
    fn parse(value: &[u8]) -> Option<Self> {
        if value == b"auto" {
            return Some(Self::Auto);
        }
        let (digits, make): (&[u8], fn(u32) -> Self) = match value {
            [digits @ .., b'p', b'x'] => (digits, Self::Pixels),
            [digits @ .., b'%'] => (digits, Self::Percent),
            digits => (digits, Self::Cells),
        };
        if digits.is_empty() || digits.len() > 9 || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let number = (digits.iter()).fold(0u32, |sum, digit| sum * 10 + u32::from(digit - b'0'));
        Some(make(number))
    }
}

/// What an inline image's arguments ask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Args {
    pub(crate) width: Side,
    pub(crate) height: Side,
    /// Whether, given both sides, the image keeps its proportions, as it
    /// does unless `preserveAspectRatio=0`.
    pub(crate) stretch: bool,
    /// `doNotMoveCursor=1`: the cursor stays where it was.
    pub(crate) stay: bool,
}

/// An inline image's arguments and where its data, base64, starts in
/// `string`, `OSC 1337`'s string after `1337;`; `None` for anything
/// else, a download among them, or arguments that don't make sense.
pub(crate) fn parse(string: &[u8]) -> Option<(Args, usize)> {
    let rest = string.strip_prefix(b"File=")?;
    // The data follows the first `:`: the name, base64, holds none.
    let colon = rest.iter().position(|&byte| byte == b':')?;
    let mut args = Args::default();
    let mut inline = false;
    for pair in rest[..colon].split(|&byte| byte == b';') {
        let (key, value) = match pair.iter().position(|&byte| byte == b'=') {
            Some(equals) => (&pair[..equals], &pair[equals + 1..]),
            None => (pair, &b""[..]),
        };
        match key {
            b"width" => args.width = Side::parse(value)?,
            b"height" => args.height = Side::parse(value)?,
            b"preserveAspectRatio" => args.stretch = value == b"0",
            b"inline" => inline = value == b"1",
            b"doNotMoveCursor" => args.stay = value == b"1",
            _ => {}
        }
    }
    inline.then_some((args, b"File=".len() + colon + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inline_images_arguments_are_read() {
        let string = b"File=name=YS5wbmc=;width=10;height=50%;inline=1:AAAA";
        let (args, data) = parse(string).unwrap();
        assert_eq!(args.width, Side::Cells(10));
        assert_eq!(args.height, Side::Percent(50));
        assert!(!args.stretch && !args.stay);
        assert_eq!(&string[data..], b"AAAA");
        let (args, _) = parse(b"File=inline=1;width=20px;preserveAspectRatio=0:AA").unwrap();
        assert_eq!((args.width, args.stretch), (Side::Pixels(20), true));
    }

    #[test]
    fn downloads_and_nonsense_are_left_alone() {
        assert!(parse(b"File=name=YQ==:AAAA").is_none());
        assert!(parse(b"File=inline=1;width=x:AAAA").is_none());
        assert!(parse(b"File=inline=1").is_none());
        assert!(parse(b"SetMark").is_none());
    }
}
