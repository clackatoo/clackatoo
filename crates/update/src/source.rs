//! Where releases come from: GitHub's API for the public repository, or
//! anywhere else that answers the same way, such as a directory of fake
//! releases in a test.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::{Error, Version};

/// The repository's releases in GitHub's API. Public, so no token is
/// needed.
pub const GITHUB: &str = "https://api.github.com/repos/clackatoo/clackatoo";

/// The environment variable that points `clackatoo update` at other
/// releases: a URL, or a directory, laid out as GitHub's API is.
pub const SOURCE_VAR: &str = "CLACKATOO_UPDATE_SOURCE";

/// The most a release's description, checksums or signature may take.
const MAX_SMALL: u64 = 4 << 20;

/// The most a download of clackatoo itself may take.
pub(crate) const MAX_DOWNLOAD: u64 = 512 << 20;

/// Which releases to follow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    /// Releases tagged from main, each a version.
    #[default]
    Stable,
    /// A build of main each night. Not published yet: GitHub's latest
    /// release leaves prereleases out, so nightlies will be prereleases
    /// tagged `nightly`, beside the stable ones.
    Nightly,
}

/// Where releases are listed: the base of an API laid out as GitHub's is,
/// which answers `releases/latest` and `releases/tags/TAG` with a release
/// in GitHub's JSON. An `http://`, `https://` or `file://` URL, or a
/// directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    base: String,
}

impl Default for Source {
    fn default() -> Self {
        Self::new(GITHUB)
    }
}

impl Source {
    /// The releases at `base`: a URL, or a directory's path.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let base = base.trim_end_matches('/');
        let base = if base.starts_with('/') {
            format!("file://{base}")
        } else {
            base.to_owned()
        };
        Self { base }
    }

    /// The releases `$CLACKATOO_UPDATE_SOURCE` names, or else GitHub's.
    #[must_use]
    pub fn from_env() -> Self {
        std::env::var(SOURCE_VAR)
            .ok()
            .filter(|base| !base.is_empty())
            .map_or_else(Self::default, |base| Self::new(&base))
    }

    /// The newest release on `channel`.
    ///
    /// # Errors
    ///
    /// If it can't be fetched or read, or the channel isn't published.
    pub fn latest(&self, channel: Channel) -> Result<Release, Error> {
        match channel {
            Channel::Stable => self.release("releases/latest"),
            Channel::Nightly => Err(Error::NoNightlies),
        }
    }

    /// The release of `version`.
    ///
    /// # Errors
    ///
    /// [`Error::NoSuchRelease`] if there's none, or if it can't be fetched
    /// or read.
    pub fn version(&self, version: Version) -> Result<Release, Error> {
        match self.release(&format!("releases/tags/{}", version.tag())) {
            Err(Error::NotFound(_)) => Err(Error::NoSuchRelease(version)),
            result => result,
        }
    }

    fn release(&self, path: &str) -> Result<Release, Error> {
        let url = format!("{}/{path}", self.base);
        let json = fetch(&url, MAX_SMALL)?;
        let release: GithubRelease =
            serde_json::from_slice(&json).map_err(|err| Error::BadRelease(url.clone(), err))?;
        let version = (release.tag_name.parse()).map_err(|_| Error::BadTag(release.tag_name))?;
        Ok(Release {
            version,
            notes: release.body.unwrap_or_default(),
            assets: (release.assets.into_iter())
                .map(|asset| Asset {
                    name: asset.name,
                    url: asset.browser_download_url,
                })
                .collect(),
        })
    }
}

/// A release, as its source lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// What changed, in Markdown.
    pub notes: String,
    /// The files it's made of.
    pub assets: Vec<Asset>,
}

impl Release {
    /// The URL of the file called `name`.
    pub(crate) fn asset(&self, name: &str) -> Result<&str, Error> {
        (self.assets.iter())
            .find(|asset| asset.name == name)
            .map(|asset| asset.url.as_str())
            .ok_or_else(|| Error::MissingAsset(self.version, name.to_owned()))
    }
}

/// One of a release's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

/// A release, as GitHub's API describes it: only what's needed.
#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// What's at `url`, at most `limit` bytes of it.
pub(crate) fn fetch(url: &str, limit: u64) -> Result<Vec<u8>, Error> {
    if let Some(path) = url.strip_prefix("file://") {
        return read_file(PathBuf::from(path), limit, url);
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(Error::BadUrl(url.to_owned()));
    }
    let config = ureq::Agent::config_builder()
        .user_agent(concat!("clackatoo/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        // A slow connection takes a while over a big download, but one
        // that stalls altogether gives up.
        .timeout_recv_body(Some(Duration::from_mins(10)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = match agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(404)) => return Err(Error::NotFound(url.to_owned())),
        Err(err) => return Err(Error::Fetch(url.to_owned(), Box::new(err))),
    };
    (response.body_mut().with_config().limit(limit).read_to_vec())
        .map_err(|err| Error::Fetch(url.to_owned(), Box::new(err)))
}

fn read_file(path: PathBuf, limit: u64, url: &str) -> Result<Vec<u8>, Error> {
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::NotFound(url.to_owned()));
        }
        Err(err) => return Err(Error::Read(path, err)),
    };
    let mut bytes = Vec::new();
    (file.take(limit + 1).read_to_end(&mut bytes)).map_err(|err| Error::Read(path, err))?;
    if bytes.len() as u64 > limit {
        return Err(Error::TooBig(url.to_owned()));
    }
    Ok(bytes)
}
