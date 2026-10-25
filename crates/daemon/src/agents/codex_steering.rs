//! Backport the host-owned idle steering option to codex-acp 2.2.2.
//!
//! Upstream: agentclientprotocol/codex-acp#440 and #593. Its published
//! adapter otherwise starts an untracked prompt when a turn finishes during
//! steering. Keep the original bundle and dependencies intact; publish the
//! prepared executable only after checking the pinned bundle and every edit.
//! This runs on the install worker, never the daemon's event loop.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub(super) const VERSION: &str = "2.2.2+clackatoo.1";
const ORIGINAL_SHA256: &str = "6b1bb4e7e9caaf4aeca1042ae4aec2dc2433b0b0e4b1614b38a4d41699902aaa";
const DIST: &str = "codex-acp/node_modules/@agentclientprotocol/codex-acp/dist";
const PARSER: &str = include_str!("../../agents/codex/steering-parser.js");
const DELIVER: &str = include_str!("../../agents/codex/steering-deliver.js");

pub(super) fn program(dir: &Path) -> PathBuf {
    dir.join(DIST).join("clackatoo.js")
}

pub(super) fn install(dir: &Path) -> io::Result<()> {
    let original = fs::read_to_string(dir.join(DIST).join("index.js"))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, original.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest.as_ref() {
        write!(hex, "{byte:02x}").expect("writing to a String");
    }
    if hex != ORIGINAL_SHA256 {
        return Err(io::Error::other(
            "Codex adapter bundle differs from the pinned steering backport",
        ));
    }
    let prepared = patch(&original)?;
    let destination = program(dir);
    let staged = destination.with_extension("js.new");
    fs::write(&staged, prepared)?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
    fs::rename(staged, destination)
}

fn patch(source: &str) -> io::Result<String> {
    let source = replace_method(
        source,
        "  parseSessionSteerParams(params) {",
        "  createSessionConfigOptions(",
        PARSER,
    )?;
    let source = replace_method(
        &source,
        "  async performSteeringRequest(params) {",
        "  /**\n   * Rejects a steering prompt",
        DELIVER,
    )?;
    let identity = "title: \"Codex\",\n        version: package_default.version";
    if source.matches(identity).count() != 1 {
        return Err(io::Error::other("Codex adapter identity changed"));
    }
    Ok(source.replacen(
        identity,
        &format!("title: \"Codex\",\n        version: \"{VERSION}\""),
        1,
    ))
}

fn replace_method(source: &str, start: &str, end: &str, replacement: &str) -> io::Result<String> {
    if source.matches(start).count() != 1 || source.matches(end).count() != 1 {
        return Err(io::Error::other(
            "Codex steering backport no longer matches its source",
        ));
    }
    let from = source.find(start).expect("one match");
    let to = source[from..]
        .find(end)
        .ok_or_else(|| io::Error::other("Codex steering method has no end"))?
        + from;
    Ok(format!(
        "{}{}{}",
        &source[..from],
        replacement,
        &source[to..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unexpected_bundles_never_publish_a_prepared_adapter() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(DIST)).unwrap();
        fs::write(dir.path().join(DIST).join("index.js"), "changed").unwrap();
        assert!(install(dir.path()).is_err());
        assert!(!program(dir.path()).exists());
        assert!(patch("changed").is_err());
        assert!(replace_method("start start end", "start", "end", "replacement").is_err());
    }

    #[test]
    fn steering_backport_contract() {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("agents/codex/steering-test.mjs");
        let result = std::process::Command::new("node")
            .arg(script)
            .output()
            .expect("node for adapter regression tests");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    #[ignore = "checks the pinned installed npm bundle; set CLACKATOO_CODEX_BACKPORT_DIR"]
    fn prepare_the_pinned_adapter() {
        let dir =
            std::env::var_os("CLACKATOO_CODEX_BACKPORT_DIR").expect("adapter install directory");
        install(Path::new(&dir)).unwrap();
    }
}
