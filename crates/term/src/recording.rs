//! Recordings of terminal output, for tests and benchmarks.
//!
//! A recording is everything a program wrote to its terminal, plus the size
//! the terminal had. Replaying it through a [`Terminal`](crate::Terminal) of
//! that size rebuilds the same screen, byte for byte, every time.
//!
//! The format is a single text header line followed by the raw bytes:
//!
//! ```text
//! clackatoo-recording v1 cols=80 rows=24
//! <raw output bytes…>
//! ```
//!
//! The header is text so a recording can be identified with `head -1`.

use crate::size::{InvalidSize, Size};

const MAGIC: &str = "clackatoo-recording v1";

/// A parsed recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recording<'a> {
    /// The terminal's size while the output was recorded.
    pub size: Size,
    /// Everything the program wrote.
    pub output: &'a [u8],
}

impl<'a> Recording<'a> {
    /// Parses a recording file's contents.
    ///
    /// # Errors
    ///
    /// If the header is missing, malformed or from an unknown version.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, RecordingError> {
        let newline = bytes
            .iter()
            .position(|&b| b == b'\n')
            .ok_or(RecordingError::MissingHeader)?;
        let header =
            std::str::from_utf8(&bytes[..newline]).map_err(|_| RecordingError::MissingHeader)?;
        let size = parse_header(header)?;
        Ok(Self {
            size,
            output: &bytes[newline + 1..],
        })
    }
}

/// The header line, including its newline, that starts a recording of a
/// terminal of `size`.
#[must_use]
pub fn header(size: Size) -> String {
    format!("{MAGIC} cols={} rows={}\n", size.cols(), size.rows())
}

fn parse_header(header: &str) -> Result<Size, RecordingError> {
    let fields = header
        .strip_prefix(MAGIC)
        .ok_or(RecordingError::MissingHeader)?;
    let mut cols = None;
    let mut rows = None;
    let malformed = || RecordingError::MalformedHeader(header.to_owned());
    for field in fields.split_whitespace() {
        let (key, value) = field.split_once('=').ok_or_else(malformed)?;
        let slot = match key {
            "cols" => &mut cols,
            "rows" => &mut rows,
            // Unknown fields are allowed, so v1 readers can skip fields added later.
            _ => continue,
        };
        *slot = Some(value.parse::<u16>().map_err(|_| malformed())?);
    }
    match (cols, rows) {
        (Some(cols), Some(rows)) => Ok(Size::new(cols, rows)?),
        _ => Err(malformed()),
    }
}

/// A recording could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RecordingError {
    /// The data doesn't start with a `clackatoo-recording v1` header line.
    #[error("not a clackatoo recording: missing `{MAGIC}` header line")]
    MissingHeader,
    /// The header line is there, but its fields can't be parsed.
    #[error("malformed recording header: {0:?}")]
    MalformedHeader(String),
    /// The header's size is out of range.
    #[error(transparent)]
    InvalidSize(#[from] InvalidSize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_header() {
        let size = Size::new(132, 43).unwrap();
        let file = [header(size).as_bytes(), b"\x1b[1mhi\n"].concat();
        let recording = Recording::parse(&file).unwrap();
        assert_eq!(recording.size, size);
        assert_eq!(recording.output, b"\x1b[1mhi\n");
    }

    #[test]
    fn unknown_fields_are_skipped() {
        let recording =
            Recording::parse(b"clackatoo-recording v1 rows=2 shell=fish cols=3\nx").unwrap();
        assert_eq!(recording.size, Size::new(3, 2).unwrap());
    }

    #[test]
    fn bad_headers_are_rejected() {
        assert_eq!(
            Recording::parse(b"no header"),
            Err(RecordingError::MissingHeader)
        );
        assert_eq!(
            Recording::parse(b"something else\n"),
            Err(RecordingError::MissingHeader)
        );
        assert!(matches!(
            Recording::parse(b"clackatoo-recording v1 cols=80\n"),
            Err(RecordingError::MalformedHeader(_))
        ));
        assert!(matches!(
            Recording::parse(b"clackatoo-recording v1 cols=0 rows=24\n"),
            Err(RecordingError::InvalidSize(_))
        ));
    }
}
