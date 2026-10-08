//! Installing a [known](crate::known) language: downloading its grammar
//! and queries, checking them, and writing them as a [pack](crate::pack).

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

/// A language clackatoo knows where to get.
#[derive(Debug)]
pub struct Known {
    /// Its name, which its pack's directory is called.
    pub name: &'static str,
    /// The grammar's name, which its WebAssembly exports a function for.
    pub grammar: &'static str,
    /// Where it's from: the grammar's repository and version.
    pub source: &'static str,
    /// The endings of the names of files it's for.
    pub extensions: &'static [&'static str],
    /// The names of files it's for.
    pub filenames: &'static [&'static str],
    /// The grammar, compiled to WebAssembly.
    pub wasm: Download,
    /// The highlight queries, in the order they go in `highlights.scm`.
    pub highlights: &'static [Download],
}

/// A file to download, and what it must be.
#[derive(Debug)]
pub struct Download {
    /// Where it is.
    pub url: &'static str,
    /// Its SHA-256, in hex.
    pub sha256: &'static str,
}

/// Why a language couldn't be installed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// curl, which downloads the files, couldn't be run.
    #[error("curl, which downloads languages, couldn't be run: {0}")]
    NoCurl(io::Error),
    /// A file couldn't be downloaded.
    #[error("{url}: {reason}")]
    Download {
        /// The file.
        url: &'static str,
        /// What curl said.
        reason: String,
    },
    /// A file downloaded isn't the one tried with clackatoo.
    #[error("{url} has changed since it was tried with clackatoo: it's not installed")]
    Changed {
        /// The file.
        url: &'static str,
    },
    /// The pack couldn't be written.
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// The known language called `name`.
#[must_use]
pub fn known(name: &str) -> Option<&'static Known> {
    crate::known::KNOWN.iter().find(|known| known.name == name)
}

/// Downloads `known` and writes it as a pack in `dir`, replacing any
/// that's there. The pack appears whole or not at all: it's written beside
/// where it goes, then moved there.
///
/// # Errors
///
/// If a file can't be downloaded, isn't what it should be, or can't be
/// written.
pub fn install(known: &Known, dir: &Path) -> Result<(), InstallError> {
    let wasm = download(&known.wasm)?;
    let mut highlights = Vec::new();
    for query in known.highlights {
        highlights.extend(download(query)?);
        highlights.push(b'\n');
    }
    let manifest = manifest(known);

    fs::create_dir_all(dir)?;
    let pack = dir.join(known.name);
    if pack.is_dir() {
        // Already there, and found: each file is replaced as a whole, the
        // manifest last.
        write(&pack, "grammar.wasm", &wasm)?;
        write(&pack, "highlights.scm", &highlights)?;
        return write(&pack, "language.toml", manifest.as_bytes());
    }
    // Hidden, so it isn't taken for a pack while it's being written.
    let partial = dir.join(format!(".{}.{}", known.name, std::process::id()));
    fs::create_dir(&partial)?;
    fs::write(partial.join("grammar.wasm"), &wasm)?;
    fs::write(partial.join("highlights.scm"), &highlights)?;
    fs::write(partial.join("language.toml"), manifest)?;
    fs::rename(&partial, &pack)?;
    Ok(())
}

/// The pack's `language.toml`.
fn manifest(known: &Known) -> String {
    let list = |names: &[&str]| {
        let quoted: Vec<String> = names.iter().map(|name| format!("{name:?}")).collect();
        format!("[{}]", quoted.join(", "))
    };
    format!(
        "# Installed by `clackatoo language install`, from {}.\n\
         grammar = {:?}\n\
         extensions = {}\n\
         filenames = {}\n",
        known.source,
        known.grammar,
        list(known.extensions),
        list(known.filenames),
    )
}

/// Writes `name` in `dir`, replacing it whole.
fn write(dir: &Path, name: &str, contents: &[u8]) -> Result<(), InstallError> {
    let partial = dir.join(format!(".{name}.{}", std::process::id()));
    fs::write(&partial, contents)?;
    fs::rename(&partial, dir.join(name))?;
    Ok(())
}

/// Downloads `file` with curl, and checks it.
fn download(file: &Download) -> Result<Vec<u8>, InstallError> {
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args(["--max-time", "300", file.url])
        .output()
        .map_err(InstallError::NoCurl)?;
    if !output.status.success() {
        return Err(InstallError::Download {
            url: file.url,
            reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    if sha256(&output.stdout) != file.sha256 {
        return Err(InstallError::Changed { url: file.url });
    }
    Ok(output.stdout)
}

fn sha256(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::Pack;

    #[test]
    fn known_languages_have_distinct_names_and_files() {
        let all = crate::known::KNOWN;
        for (index, language) in all.iter().enumerate() {
            assert!(all[..index].iter().all(|other| other.name != language.name));
            assert_eq!(language.wasm.sha256.len(), 64, "{}", language.name);
            assert!(!language.highlights.is_empty(), "{}", language.name);
        }
        assert_eq!(known("rust").map(|rust| rust.extensions), Some(&["rs"][..]));
    }

    #[test]
    fn a_manifest_reads_back_as_its_pack() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("bash");
        fs::create_dir(&pack).unwrap();
        let bash = known("bash").unwrap();
        fs::write(pack.join("language.toml"), manifest(bash)).unwrap();
        let read = Pack::read(&pack).unwrap();
        assert_eq!(read.grammar, "bash");
        assert_eq!(read.extensions, ["sh", "bash"]);
        assert!(read.filenames.contains(&".bashrc".to_owned()));
    }

    #[test]
    fn sha256_is_in_hex() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
