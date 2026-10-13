//! Builds the web app, `crates/web`, into the program: compiles it to
//! WebAssembly, runs wasm-bindgen over it, and writes `web_app.rs` in
//! `OUT_DIR`, which embeds its files, gzipped where that's smaller, each
//! with a hash of itself to cache it by.
//!
//! It needs the `wasm32-unknown-unknown` target and wasm-bindgen-cli at
//! exactly the version of the `wasm-bindgen` crate in `Cargo.lock`:
//!
//! ```sh
//! rustup target add wasm32-unknown-unknown
//! cargo install wasm-bindgen-cli --version 0.2.129 --locked
//! ```
//!
//! Without them, the program is built without the app, saying so in a
//! warning, and the daemon serves a page saying so in its place: building
//! clackatoo never needs them. `CLACKATOO_WEB_APP=required` makes a build
//! without them fail instead, as releases do; `off` doesn't look for them.
//! `CLACKATOO_WASM_BINDGEN` names the wasm-bindgen program, if it isn't
//! `wasm-bindgen` on `PATH`.
//!
//! The app is compiled by a cargo of its own, in `target/web`, apart from
//! this build's target directory, whose lock it would otherwise wait on.
//! It's a member of the workspace, so `Cargo.lock` pins what it's built
//! with. The app is built again whenever it or the crates it shares with
//! the daemon change: the protocol, the terminal and the text crates.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The app's own files, beside what wasm-bindgen makes, with their types.
const STATIC: &[(&str, &str)] = &[
    ("index.html", "text/html; charset=utf-8"),
    ("main.js", "text/javascript; charset=utf-8"),
    ("sw.js", "text/javascript; charset=utf-8"),
    ("style.css", "text/css; charset=utf-8"),
    ("manifest.webmanifest", "application/manifest+json"),
    ("icon-180.png", "image/png"),
    ("icon-192.png", "image/png"),
    ("icon-512.png", "image/png"),
];

/// What wasm-bindgen makes, with their types.
const BINDGEN: &[(&str, &str)] = &[
    ("app.js", "text/javascript; charset=utf-8"),
    ("app_bg.wasm", "application/wasm"),
];

/// Builds the app, and writes `web_app.rs`.
pub(crate) fn build() {
    for var in ["CLACKATOO_WEB_APP", "CLACKATOO_WASM_BINDGEN"] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo says"));
    let root = manifest.join("../..");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo says"));
    let mode = std::env::var("CLACKATOO_WEB_APP").unwrap_or_default();
    let files = if mode == "off" {
        Vec::new()
    } else {
        for watched in [
            "crates/web/src",
            "crates/web/static",
            "crates/web/Cargo.toml",
            "crates/protocol/src",
            "crates/term/src",
            "crates/text/src",
            "Cargo.lock",
        ] {
            println!("cargo::rerun-if-changed={}", root.join(watched).display());
        }
        match compile(&root, &out) {
            Ok(files) => files,
            Err(why) if mode == "required" => panic!("the web app couldn't be built: {why}"),
            Err(why) => {
                println!("cargo::warning=built without the web app: {why}");
                Vec::new()
            }
        }
    };
    write_assets(&out.join("web_app.rs"), &files);
}

/// One of the app's files, built.
struct File {
    /// Where it's served.
    path: String,
    content_type: &'static str,
    /// Where it is, and gzipped, if that's smaller.
    source: PathBuf,
    gzip: Option<PathBuf>,
    etag: String,
}

/// Compiles the app into `out/web`, returning its files.
fn compile(root: &Path, out: &Path) -> Result<Vec<File>, String> {
    let bindgen =
        std::env::var_os("CLACKATOO_WASM_BINDGEN").unwrap_or_else(|| "wasm-bindgen".into());
    let wanted = locked_version(&root.join("Cargo.lock"), "wasm-bindgen")
        .ok_or("Cargo.lock has no wasm-bindgen")?;
    let install = format!(
        "rustup target add wasm32-unknown-unknown; \
         cargo install wasm-bindgen-cli --version {wanted} --locked"
    );
    let version = Command::new(&bindgen)
        .arg("--version")
        .output()
        .map_err(|_| format!("there's no {} to run: {install}", bindgen.display()))?;
    let version = String::from_utf8_lossy(&version.stdout);
    let version = version.split_whitespace().nth(1).unwrap_or_default();
    if version != wanted {
        return Err(format!(
            "{} is {version}, and the wasm-bindgen crate is {wanted}: {install}",
            bindgen.display()
        ));
    }
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let sysroot = Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .map_err(|err| format!("rustc couldn't be asked for its sysroot: {err}"))?;
    let sysroot = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim());
    if !sysroot.join("lib/rustlib/wasm32-unknown-unknown").exists() {
        return Err(format!(
            "the wasm32-unknown-unknown target isn't installed: {install}"
        ));
    }

    // An optimised build for an optimised program, and a quicker one for
    // the rest.
    let profile = match std::env::var("PROFILE").as_deref() {
        Ok("release") => "web",
        _ => "web-dev",
    };
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| root.join("target"), PathBuf::from)
        .join("web");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .current_dir(root)
        .args(["build", "--locked", "-p", "clackatoo-web"])
        .args(["--target", "wasm32-unknown-unknown", "--profile", profile])
        .arg("--target-dir")
        .arg(&target_dir);
    scrub(&mut command);
    let status = command
        .status()
        .map_err(|err| format!("cargo couldn't be run for it: {err}"))?;
    if !status.success() {
        return Err("compiling it to WebAssembly failed".into());
    }
    let wasm = target_dir.join(format!(
        "wasm32-unknown-unknown/{profile}/clackatoo_web.wasm"
    ));
    let web = out.join("web");
    let status = Command::new(&bindgen)
        .args(["--target", "web", "--no-typescript", "--out-name", "app"])
        .arg("--out-dir")
        .arg(&web)
        .arg(&wasm)
        .status()
        .map_err(|err| format!("wasm-bindgen couldn't be run: {err}"))?;
    if !status.success() {
        return Err("wasm-bindgen failed".into());
    }

    collect(root, out, &web)
}

/// The app's files: its own, and what wasm-bindgen made in `web`, each
/// gzipped into `out` where that's smaller.
fn collect(root: &Path, out: &Path, web: &Path) -> Result<Vec<File>, String> {
    let statics = root.join("crates/web/static");
    let sources = (STATIC
        .iter()
        .map(|&(name, kind)| (statics.join(name), name, kind)))
    .chain(
        BINDGEN
            .iter()
            .map(|&(name, kind)| (web.join(name), name, kind)),
    );
    let mut files = Vec::new();
    for (source, name, content_type) in sources {
        let body = std::fs::read(&source)
            .map_err(|err| format!("{} couldn't be read: {err}", source.display()))?;
        let gzip = gzip(&body).filter(|gzipped| gzipped.len() < body.len() * 9 / 10);
        let gzip = match gzip {
            Some(gzipped) => {
                let path = out.join(format!("{name}.gz"));
                std::fs::write(&path, gzipped).map_err(|err| err.to_string())?;
                Some(path)
            }
            None => None,
        };
        files.push(File {
            path: format!("/{name}"),
            content_type,
            source,
            gzip,
            etag: format!("{:016x}", fnv1a(&body)),
        });
    }
    Ok(files)
}

/// Takes from `command` what cargo tells a build script that's about this
/// build, not the app's: flags for this target, clippy standing in for
/// rustc, and the like.
fn scrub(command: &mut Command) {
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        let ours = name.starts_with("CARGO_CFG_")
            || name.starts_with("CARGO_FEATURE_")
            || name.starts_with("CARGO_PKG_")
            || name.starts_with("CARGO_MANIFEST_")
            || name.starts_with("DEP_")
            || [
                "CARGO_ENCODED_RUSTFLAGS",
                "RUSTFLAGS",
                "CARGO_TARGET_DIR",
                "CARGO_BUILD_TARGET",
                "CARGO_PRIMARY_PACKAGE",
                "RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER",
                "TARGET",
                "HOST",
                "PROFILE",
                "OUT_DIR",
                "OPT_LEVEL",
                "DEBUG",
                "NUM_JOBS",
            ]
            .contains(&name.as_ref());
        if ours {
            command.env_remove(OsString::from(name.as_ref()));
        }
    }
}

/// The version `Cargo.lock` at `path` has of the crate `name`.
fn locked_version(path: &Path, name: &str) -> Option<String> {
    let lock = std::fs::read_to_string(path).ok()?;
    let mut lines = lock.lines();
    let named = format!("name = \"{name}\"");
    lines.find(|line| *line == named)?;
    let version = lines.next()?.strip_prefix("version = \"")?;
    Some(version.trim_end_matches('"').to_owned())
}

/// `bytes`, gzipped as tightly as it goes.
fn gzip(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(bytes).ok()?;
    encoder.finish().ok()
}

/// FNV-1a, 64 bits: what tells one version of a file from another, for
/// browsers' caches. Not for anything an attacker chooses.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Writes the Rust that embeds `files`, as `ASSETS`.
fn write_assets(path: &Path, files: &[File]) {
    let mut rust = String::from(
        "/// The web app's files, built into the program: none if it was built\n\
         /// without them.\n\
         pub(crate) static ASSETS: &[clackatoo_daemon::web::Asset] = &[\n",
    );
    for file in files {
        let gzip = file.gzip.as_ref().map_or_else(
            || "None".to_owned(),
            |gzip| format!("Some(include_bytes!({:?}))", gzip.display().to_string()),
        );
        let _ = writeln!(
            rust,
            "    clackatoo_daemon::web::Asset {{ path: {:?}, content_type: {:?}, \
             body: include_bytes!({:?}), gzip: {gzip}, etag: {:?} }},",
            file.path,
            file.content_type,
            file.source.display().to_string(),
            file.etag,
        );
    }
    rust.push_str("];\n");
    // Rewritten only when it changes, so the program isn't rebuilt for
    // nothing.
    if std::fs::read_to_string(path).ok().as_deref() != Some(rust.as_str()) {
        std::fs::write(path, rust).expect("OUT_DIR is writable");
    }
}
