//! clackatoo's releases: finding them, checking they're signed with the
//! release key, and installing them in place of the running build.
//!
//! Releases are made by CI from tags in the public repository on GitHub,
//! and listed by its API ([`Source`]). Each has a gzipped build per
//! machine, and checksums signed with minisign ([`verify`]). A build is
//! installed beside the old one and renamed over it ([`install`]), and the
//! old one kept for going back. The window looks for new releases once a
//! day ([`check`]), but only `clackatoo update` installs them.

#![forbid(unsafe_code)]

pub mod check;
#[cfg(feature = "fake")]
pub mod fake;
pub mod install;
mod source;
pub mod verify;
mod version;

use std::path::PathBuf;

pub use minisign_verify::PublicKey;
pub use source::{Asset, Channel, GITHUB, Release, SOURCE_VAR, Source};
pub use version::{CURRENT, NotAVersion, Version};

/// Why an update failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("couldn't fetch {0}")]
    Fetch(String, #[source] Box<ureq::Error>),
    #[error("{0} isn't there")]
    NotFound(String),
    #[error("{0} is too big to be a release")]
    TooBig(String),
    #[error("{0} is neither an http(s):// nor a file:// URL")]
    BadUrl(String),
    #[error("couldn't read {}", .0.display())]
    Read(PathBuf, #[source] std::io::Error),
    #[error("couldn't write {}", .0.display())]
    Write(PathBuf, #[source] std::io::Error),
    #[error("{0} doesn't describe a release")]
    BadRelease(String, #[source] serde_json::Error),
    #[error("the latest release is tagged {0:?}, which isn't a version")]
    BadTag(String),
    #[error("there's no release {0}")]
    NoSuchRelease(Version),
    #[error("nightly builds aren't published yet")]
    NoNightlies,
    #[error("release {0} has no {1}")]
    MissingAsset(Version, String),
    #[error("this build of clackatoo carries no release key, so it can't check releases")]
    NoReleaseKey,
    #[error("release {0} isn't signed with the release key, so it wasn't installed")]
    BadSignature(Version),
    #[error("{0} isn't listed in the release's checksums")]
    NotListed(String),
    #[error("{0} isn't what the release's checksums say, so it wasn't installed")]
    BadChecksum(String),
    #[error("couldn't unpack the download")]
    Unpack(#[source] std::io::Error),
    #[error("the new build won't run on this machine: {0}")]
    WontRun(String),
    #[error("the new build should be {expected}, but says {said:?}")]
    OtherVersion { expected: Version, said: String },
    #[error("there's no previous build at {}", .0.display())]
    NoPrevious(PathBuf),
}
