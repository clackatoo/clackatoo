//! The numeric parameters of CSI and DCS sequences.

use std::fmt;

/// The most parameters (counting subparameters) a sequence keeps. Beyond
/// this, the sequence is flagged as ignored.
pub(crate) const MAX_PARAMS: usize = 32;

/// A sequence's parameters, such as the `1;38:5:208` in `CSI 1;38:5:208 m`.
///
/// Each parameter is a group: its value followed by any colon-separated
/// subparameters. Values saturate at `u16::MAX`, and a missing value is 0.
#[derive(Clone, Default)]
pub(crate) struct Params {
    /// For the first value of each group, how many values the group has.
    group_lens: [u8; MAX_PARAMS],
    values: [u16; MAX_PARAMS],
    /// How many subparameters the group being collected has so far.
    current_subparams: u8,
    len: usize,
}

impl Params {
    /// The number of values, counting subparameters.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// The groups, each a value followed by its subparameters.
    pub(crate) fn iter(&self) -> ParamsIter<'_> {
        ParamsIter {
            params: self,
            index: 0,
        }
    }

    pub(super) fn is_full(&self) -> bool {
        self.len == MAX_PARAMS
    }

    pub(super) fn clear(&mut self) {
        self.current_subparams = 0;
        self.len = 0;
    }

    /// Ends the current group with `value`.
    pub(super) fn push(&mut self, value: u16) {
        self.group_lens[self.len - usize::from(self.current_subparams)] =
            self.current_subparams + 1;
        self.values[self.len] = value;
        self.current_subparams = 0;
        self.len += 1;
    }

    /// Adds `value` to the current group, which continues with a
    /// subparameter.
    pub(super) fn extend(&mut self, value: u16) {
        self.group_lens[self.len - usize::from(self.current_subparams)] =
            self.current_subparams + 1;
        self.values[self.len] = value;
        self.current_subparams += 1;
        self.len += 1;
    }
}

impl<'a> IntoIterator for &'a Params {
    type IntoIter = ParamsIter<'a>;
    type Item = &'a [u16];

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// The groups of a [`Params`].
#[derive(Clone, Debug)]
pub(crate) struct ParamsIter<'a> {
    params: &'a Params,
    index: usize,
}

impl<'a> Iterator for ParamsIter<'a> {
    type Item = &'a [u16];

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.params.len {
            return None;
        }
        let len = usize::from(self.params.group_lens[self.index]);
        let group = &self.params.values[self.index..self.index + len];
        self.index += len;
        Some(group)
    }
}

impl fmt::Debug for Params {
    /// Written the way the sequence spells them, like `[1;38:5:208]`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[")?;
        for (i, group) in self.iter().enumerate() {
            if i > 0 {
                write!(f, ";")?;
            }
            for (j, value) in group.iter().enumerate() {
                if j > 0 {
                    write!(f, ":")?;
                }
                write!(f, "{value}")?;
            }
        }
        write!(f, "]")
    }
}
