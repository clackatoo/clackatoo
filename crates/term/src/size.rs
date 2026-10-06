//! Terminal dimensions.

/// A terminal's size in cells.
///
/// Both dimensions are between 1 and [`Size::MAX`]. The cap stops a client
/// asking for a terminal that would need gigabytes of grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Size {
    cols: u16,
    rows: u16,
}

impl Size {
    /// The largest number of columns or rows a terminal may have. That's
    /// enough for an 8K display with a 2-pixel-wide font.
    pub const MAX: u16 = 4096;

    /// A size of `cols` × `rows`.
    ///
    /// # Errors
    ///
    /// If either dimension is 0 or above [`Size::MAX`].
    pub fn new(cols: u16, rows: u16) -> Result<Self, InvalidSize> {
        let valid = 1..=Self::MAX;
        if valid.contains(&cols) && valid.contains(&rows) {
            Ok(Self { cols, rows })
        } else {
            Err(InvalidSize { cols, rows })
        }
    }

    /// Columns.
    #[must_use]
    pub const fn cols(self) -> u16 {
        self.cols
    }

    /// Rows.
    #[must_use]
    pub const fn rows(self) -> u16 {
        self.rows
    }
}

impl std::fmt::Display for Size {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.cols, self.rows)
    }
}

/// A requested terminal size was out of range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("terminal size {cols}x{rows} is invalid: both dimensions must be between 1 and {max}", max = Size::MAX)]
pub struct InvalidSize {
    /// The requested columns.
    pub cols: u16,
    /// The requested rows.
    pub rows: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_must_be_in_range() {
        assert!(Size::new(80, 24).is_ok());
        assert!(Size::new(1, Size::MAX).is_ok());
        assert!(Size::new(0, 24).is_err());
        assert!(Size::new(80, Size::MAX + 1).is_err());
    }
}
