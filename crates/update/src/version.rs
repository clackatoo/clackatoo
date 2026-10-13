//! Release versions, and which is newer.
//!
//! clackatoo's versions are calendar versions, `YEAR.MONTH.N`: the year and
//! month a release was made in, and how many releases came before it that
//! month, from 0. So `2026.10.0` is October 2026's first release, and
//! `2026.10.3` its fourth. They're three plain numbers, as Cargo needs, and
//! compare number by number, never as text.

use std::fmt;
use std::str::FromStr;

/// The version of this build: the workspace's, which every crate shares.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A release's version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version([u64; 3]);

impl Version {
    /// The version of this build.
    ///
    /// # Panics
    ///
    /// Never: the workspace's version is checked by a test.
    #[must_use]
    pub fn current() -> Self {
        CURRENT
            .parse()
            .expect("the workspace's version is three numbers")
    }

    /// The git tag a release of this version has: `v` and the version.
    #[must_use]
    pub fn tag(self) -> String {
        format!("v{self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c] = self.0;
        write!(f, "{a}.{b}.{c}")
    }
}

/// Text that isn't a version.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a version, such as 2026.10.0")]
pub struct NotAVersion(String);

impl FromStr for Version {
    type Err = NotAVersion;

    /// Reads `2026.10.0`, or a tag, `v2026.10.0`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let wrong = || NotAVersion(text.to_owned());
        let digits = text.strip_prefix('v').unwrap_or(text);
        let mut parts = digits.split('.');
        let mut numbers = [0; 3];
        for number in &mut numbers {
            let part = parts.next().ok_or_else(wrong)?;
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(wrong());
            }
            *number = part.parse().map_err(|_| wrong())?;
        }
        if parts.next().is_some() {
            return Err(wrong());
        }
        Ok(Self(numbers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        text.parse().unwrap()
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(version("2026.10.10") > version("2026.10.9"));
        assert!(version("2026.11.0") > version("2026.10.31"));
        assert!(version("2027.1.0") > version("2026.12.4"));
        assert!(version("2026.10.0") > version("0.1.0"));
        assert_eq!(version("v2026.10.2"), version("2026.10.2"));
    }

    #[test]
    fn only_three_numbers_are_a_version() {
        for text in [
            "",
            "2026",
            "2026.10",
            "2026.10.1.2",
            "2026.10.x",
            "2026.-1.0",
            "2026..1",
        ] {
            assert!(text.parse::<Version>().is_err(), "{text:?}");
        }
        assert_eq!(version("2026.10.3").to_string(), "2026.10.3");
        assert_eq!(version("2026.10.3").tag(), "v2026.10.3");
    }

    #[test]
    fn this_build_has_a_version() {
        assert_eq!(Version::current().to_string(), CURRENT);
    }
}
