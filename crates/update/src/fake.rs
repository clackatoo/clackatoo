//! Fake releases, for tests: a directory laid out as GitHub's API is,
//! signed with a key made for the test, and read as files or served over
//! HTTP. Only built with the `fake` feature, which only tests turn on.
//!
//! Everything here panics rather than failing: it's for tests.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;

use ring::digest::{SHA256, digest};

use crate::verify::{self, SIGNATURE, SUMS};
use crate::{PublicKey, Source};

/// A key to sign fake releases with, made afresh.
#[derive(Debug)]
pub struct Key {
    pair: minisign::KeyPair,
}

impl Default for Key {
    fn default() -> Self {
        Self::new()
    }
}

impl Key {
    /// # Panics
    ///
    /// If no key can be made.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pair: minisign::KeyPair::generate_unencrypted_keypair().unwrap(),
        }
    }

    /// The public half, as the base64 line of minisign's `.pub` file, as
    /// `$CLACKATOO_UPDATE_KEY` takes it.
    #[must_use]
    pub fn public_base64(&self) -> String {
        self.pair.pk.to_base64()
    }

    /// The public half.
    ///
    /// # Panics
    ///
    /// Never: the key is minisign's own.
    #[must_use]
    pub fn public(&self) -> PublicKey {
        PublicKey::from_base64(&self.public_base64()).unwrap()
    }

    /// A minisign signature of `bytes`, as the `.minisig` file holds it,
    /// with `trusted_comment`, as CI's `minisign -t` gives it.
    ///
    /// # Panics
    ///
    /// If signing fails.
    #[must_use]
    pub fn sign(&self, bytes: &[u8], trusted_comment: &str) -> String {
        let pk = Some(&self.pair.pk);
        minisign::sign(pk, &self.pair.sk, bytes, Some(trusted_comment), None)
            .unwrap()
            .into_string()
    }
}

/// A fake program that says it's clackatoo `version` when asked
/// `--version`, and that it ran when given anything else, as a shell
/// script.
#[must_use]
pub fn program(version: &str) -> Vec<u8> {
    format!(
        "#!/bin/sh\ncase \"$1\" in\n--version) echo \"clackatoo {version} (fake 2026-10-13)\";;\n*) \
         echo \"clackatoo {version} ran $*\";;\nesac\n"
    )
    .into_bytes()
}

/// `bytes`, gzipped, as releases hold their builds.
///
/// # Panics
///
/// Never: it's all in memory.
#[must_use]
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// The SHA-256 of `bytes`, in hex, as `sha256sum` writes it.
#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    (digest(&SHA256, bytes).as_ref().iter()).fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// What goes into a fake release, each part spoilable.
#[derive(Debug)]
pub struct Fake<'a> {
    pub version: &'a str,
    /// The program, before it's gzipped.
    pub program: Vec<u8>,
    /// What the checksums are signed with.
    pub key: &'a Key,
    /// Lists a wrong hash for the build in the checksums.
    pub wrong_sum: bool,
    /// Leaves this machine's build out.
    pub no_build: bool,
    /// Signs it as this other version's release, as an old release served
    /// under a new tag would be.
    pub signed_as: Option<&'a str>,
}

impl<'a> Fake<'a> {
    /// A good release of `version`, signed with `key`.
    #[must_use]
    pub fn new(version: &'a str, key: &'a Key) -> Self {
        Self {
            version,
            program: program(version),
            key,
            wrong_sum: false,
            no_build: false,
            signed_as: None,
        }
    }
}

/// A directory of fake releases, and the base URL its files are named
/// under: `file://` it, or an HTTP server's.
#[derive(Debug)]
pub struct Releases {
    dir: tempfile::TempDir,
    base: String,
}

impl Default for Releases {
    fn default() -> Self {
        Self::new()
    }
}

impl Releases {
    /// An empty directory of releases, read as files.
    ///
    /// # Panics
    ///
    /// If it can't be made.
    #[must_use]
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = format!("file://{}", dir.path().display());
        Self { dir, base }
    }

    /// The directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Where to find them, as `$CLACKATOO_UPDATE_SOURCE` takes it.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    #[must_use]
    pub fn source(&self) -> Source {
        Source::new(&self.base)
    }

    /// Serves the releases over HTTP from now on, on a port of their own,
    /// until the test ends. Releases published before still name their
    /// files as before.
    ///
    /// # Panics
    ///
    /// If there's no port to be had.
    pub fn serve_over_http(&mut self) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        self.base = format!("http://{}", listener.local_addr().unwrap());
        let dir = self.path().to_owned();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let Ok(reading) = stream.try_clone() else {
                    return;
                };
                let mut reader = BufReader::new(reading);
                let mut request = String::new();
                let _ = reader.read_line(&mut request);
                let mut user_agent = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(agent) = line.to_ascii_lowercase().strip_prefix("user-agent:") {
                        agent.trim().clone_into(&mut user_agent);
                    }
                }
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                // GitHub's API refuses requests with no User-Agent.
                let body = if user_agent.starts_with("clackatoo/") {
                    std::fs::read(dir.join(path.trim_start_matches('/'))).ok()
                } else {
                    None
                };
                let (status, body) = match body {
                    Some(body) => ("200 OK", body),
                    None => ("404 Not Found", b"{\"message\":\"Not Found\"}".to_vec()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
    }

    /// Publishes `fake`, as the latest release too if `latest`.
    ///
    /// # Panics
    ///
    /// If its files can't be written.
    pub fn publish(&self, fake: &Fake<'_>, latest: bool) {
        let tag = format!("v{}", fake.version);
        let assets = self.path().join("assets").join(&tag);
        std::fs::create_dir_all(&assets).unwrap();
        let name = verify::build_name();
        let packed = gzip(&fake.program);
        let hash = if fake.wrong_sum {
            sha256(b"something else")
        } else {
            sha256(&packed)
        };
        let sums = format!(
            "{hash}  {name}\n{}  clackatoo-elsewhere-linux.gz\n",
            sha256(b"")
        );
        std::fs::write(assets.join(&name), &packed).unwrap();
        std::fs::write(assets.join(SUMS), &sums).unwrap();
        let signed_as = fake.signed_as.unwrap_or(fake.version);
        let comment = format!("clackatoo v{signed_as}");
        std::fs::write(
            assets.join(SIGNATURE),
            fake.key.sign(sums.as_bytes(), &comment),
        )
        .unwrap();
        let url = |file: &str| format!("{}/assets/{tag}/{file}", self.base);
        let mut listed = vec![SUMS, SIGNATURE];
        if !fake.no_build {
            listed.push(&name);
        }
        let assets: Vec<_> = (listed.iter())
            .map(|file| serde_json::json!({ "name": file, "browser_download_url": url(file) }))
            .collect();
        let json = serde_json::json!({
            "tag_name": tag,
            "body": format!("## {}\n\n- fake", fake.version),
            "assets": assets,
        })
        .to_string();
        let releases = self.path().join("releases");
        std::fs::create_dir_all(releases.join("tags")).unwrap();
        std::fs::write(releases.join("tags").join(&tag), &json).unwrap();
        if latest {
            std::fs::write(releases.join("latest"), &json).unwrap();
        }
    }
}
