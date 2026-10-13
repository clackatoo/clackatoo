//! Checking a release is ours before it's installed.
//!
//! Each release has a file of checksums, `SHA256SUMS`, as `sha256sum`
//! writes it, and that file's minisign signature, `SHA256SUMS.minisig`,
//! made in CI with the release key. Every build carries the key's public
//! half, so a download is installed only if the signature is the key's
//! and the download's SHA-256 is the one the signed file lists. One
//! signature covers every file, and anyone can check by hand:
//!
//! ```text
//! minisign -Vm SHA256SUMS -p release-key.pub && sha256sum -c --ignore-missing SHA256SUMS
//! ```

use std::io::Read;

use minisign_verify::{PublicKey, Signature};
use ring::digest::{SHA256, digest};

use crate::source::{MAX_DOWNLOAD, fetch};
use crate::{Error, Release, Version};

/// The release key's public half, as minisign writes it. Until the real
/// one is made, this file says there's none, and updates are refused.
const RELEASE_KEY: &str = include_str!("../release-key.pub");

/// The environment variable that replaces the release key in tests, as
/// the base64 line of a minisign public key. Only debug builds read it:
/// a release build trusts its own key and no other.
pub const KEY_VAR: &str = "CLACKATOO_UPDATE_KEY";

/// The file of checksums each release has.
pub const SUMS: &str = "SHA256SUMS";

/// Its signature.
pub const SIGNATURE: &str = "SHA256SUMS.minisig";

/// The key releases are signed with: this build's own, or in a debug
/// build, `$CLACKATOO_UPDATE_KEY`'s.
///
/// # Errors
///
/// [`Error::NoReleaseKey`] if this build carries none.
pub fn release_key() -> Result<PublicKey, Error> {
    if cfg!(debug_assertions)
        && let Ok(key) = std::env::var(KEY_VAR)
    {
        return PublicKey::from_base64(key.trim()).map_err(|_| Error::NoReleaseKey);
    }
    PublicKey::decode(RELEASE_KEY).map_err(|_| Error::NoReleaseKey)
}

/// The name of the build of clackatoo for this machine in each release,
/// gzipped: `clackatoo-x86_64-linux.gz`.
#[must_use]
pub fn build_name() -> String {
    format!("clackatoo-{}-linux.gz", std::env::consts::ARCH)
}

/// Downloads this machine's build from `release`, and checks it's signed
/// with `key`. Returns the program, unpacked.
///
/// # Errors
///
/// If a file can't be fetched, the release has no build for this machine,
/// the signature isn't `key`'s ([`Error::BadSignature`]), or the download
/// isn't what the checksums say ([`Error::BadChecksum`]).
pub fn download(release: &Release, key: &PublicKey) -> Result<Vec<u8>, Error> {
    let name = build_name();
    let build_url = release.asset(&name)?;
    let sums = fetch(release.asset(SUMS)?, 1 << 20)?;
    let signature = fetch(release.asset(SIGNATURE)?, 1 << 16)?;
    check_signature(&sums, &signature, key, release.version)?;
    let packed = fetch(build_url, MAX_DOWNLOAD)?;
    check_sum(&sums, &name, &packed)?;
    unpack(&packed)
}

/// Checks `signature` is `key`'s, over `sums`.
fn check_signature(
    sums: &[u8],
    signature: &[u8],
    key: &PublicKey,
    version: Version,
) -> Result<(), Error> {
    let signature = std::str::from_utf8(signature)
        .ok()
        .and_then(|text| Signature::decode(text).ok())
        .ok_or(Error::BadSignature(version))?;
    // Only prehashed signatures, as minisign has made by default since
    // 0.10.
    (key.verify(sums, &signature, false)).map_err(|_| Error::BadSignature(version))
}

/// Checks `bytes`, the file called `name`, has the SHA-256 `sums` lists
/// for it.
fn check_sum(sums: &[u8], name: &str, bytes: &[u8]) -> Result<(), Error> {
    let sums = String::from_utf8_lossy(sums);
    // sha256sum writes the hash, a space, then a space for text or `*` for
    // binary, then the name.
    let listed = sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(' ')?;
        let file = file.strip_prefix([' ', '*']).unwrap_or(file);
        (file == name).then_some(hash)
    });
    let Some(listed) = listed else {
        return Err(Error::NotListed(name.to_owned()));
    };
    let actual = digest(&SHA256, bytes);
    let matches = listed.len() == actual.as_ref().len() * 2
        && (actual.as_ref().iter())
            .zip(listed.as_bytes().chunks(2))
            .all(|(&byte, hex)| {
                std::str::from_utf8(hex)
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                    == Some(byte)
            });
    if matches {
        Ok(())
    } else {
        Err(Error::BadChecksum(name.to_owned()))
    }
}

/// Unzips a download.
fn unpack(packed: &[u8]) -> Result<Vec<u8>, Error> {
    let mut program = Vec::new();
    flate2::read::GzDecoder::new(packed)
        .take(MAX_DOWNLOAD * 4)
        .read_to_end(&mut program)
        .map_err(Error::Unpack)?;
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_are_read_as_sha256sum_writes_them() {
        let hash = "b5bb9d8014a0f9b1d61e21e796d78dccdf1352f23cd32812f4850b878ae4944c";
        for line in [format!("{hash}  foo"), format!("{hash} *foo")] {
            assert!(check_sum(line.as_bytes(), "foo", b"foo\n").is_ok());
            assert!(matches!(
                check_sum(line.as_bytes(), "foo", b"bar\n"),
                Err(Error::BadChecksum(_))
            ));
            assert!(matches!(
                check_sum(line.as_bytes(), "bar", b"foo\n"),
                Err(Error::NotListed(_))
            ));
        }
    }

    #[test]
    fn the_release_key_is_minisigns_or_none() {
        // Until the real key is made, the file holds none, and updates are
        // refused, not checked against nothing.
        match PublicKey::decode(RELEASE_KEY) {
            Ok(_) => assert!(RELEASE_KEY.starts_with("untrusted comment: ")),
            Err(_) => assert!(RELEASE_KEY.contains("no release key")),
        }
    }
}
