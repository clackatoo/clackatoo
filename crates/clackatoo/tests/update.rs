//! `clackatoo update` and `clackatoo --version`, run as real programs,
//! against fake releases.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

use clackatoo_update::fake::{Fake, Key, Releases};
use clackatoo_update::{SOURCE_VAR, verify};

const BINARY: &str = env!("CARGO_BIN_EXE_clackatoo");

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn version_says_the_release_the_commit_and_the_day_it_was_built() {
    let output = Command::new(BINARY).arg("--version").output().unwrap();
    assert!(output.status.success());
    let said = stdout(&output);
    // `clackatoo 2026.10.0 (1a2b3c4d5e 2026-10-13)`
    let rest = said
        .trim_end()
        .strip_prefix(&format!("clackatoo {} (", env!("CARGO_PKG_VERSION")))
        .unwrap_or_else(|| panic!("{said:?}"));
    let (hash, date) = rest.strip_suffix(')').unwrap().split_once(' ').unwrap();
    assert!(
        hash == "unknown" || (hash.len() == 10 && hash.bytes().all(|b| b.is_ascii_hexdigit())),
        "{hash:?}"
    );
    let parts: Vec<_> = date.split('-').collect();
    assert_eq!(
        parts.iter().map(|part| part.len()).collect::<Vec<_>>(),
        [4, 2, 2],
        "{date}"
    );
    assert!(
        parts.iter().all(|part| part.parse::<u32>().is_ok()),
        "{date}"
    );
    // The commit checked out, when git can say.
    let git = Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();
    if let Ok(git) = git
        && git.status.success()
        && std::env::var_os("CLACKATOO_GIT_HASH").is_none()
    {
        assert_eq!(hash, stdout(&git).trim());
    }
}

/// A copy of clackatoo installed in a directory of its own, to update:
/// a hard link to the one built, in cargo's scratch directory beside it, so
/// that it can always be linked.
///
/// Never a copy: this process would have the copy open for writing, and a
/// child another test starts meanwhile would inherit it until it runs its
/// program, and running the copy then fails with "Text file busy"
/// (ETXTBSY).
struct Installed {
    dir: tempfile::TempDir,
    runtime: tempfile::TempDir,
    /// Its `$XDG_DATA_HOME`, where the app launcher's entry goes.
    data: tempfile::TempDir,
}

impl Installed {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        std::fs::hard_link(BINARY, dir.path().join("clackatoo")).unwrap();
        Self {
            dir,
            // No daemon runs here.
            runtime: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
        }
    }

    fn program(&self) -> PathBuf {
        self.dir.path().join("clackatoo")
    }

    fn previous(&self) -> PathBuf {
        self.dir.path().join("clackatoo.previous")
    }

    fn update(&self, args: &[&str], releases: &Releases, key: &Key) -> Output {
        let mut all = vec!["update"];
        all.extend_from_slice(args);
        self.run(&self.program(), &all, releases, key, self.runtime.path())
    }

    /// Runs `program` with `args`, updating from `releases`, trusting
    /// `key`, with the daemon, if any, in `runtime`.
    fn run(
        &self,
        program: &Path,
        args: &[&str],
        releases: &Releases,
        key: &Key,
        runtime: &Path,
    ) -> Output {
        Command::new(program)
            .args(args)
            .env(SOURCE_VAR, releases.base())
            .env(verify::KEY_VAR, key.public_base64())
            .env("XDG_RUNTIME_DIR", runtime)
            .env("XDG_DATA_HOME", self.data.path())
            // The machines connected to recently, which the entry offers:
            // none.
            .env("XDG_STATE_HOME", self.data.path().join("state"))
            .output()
            .unwrap()
    }

    /// Where the app launcher's entry is.
    fn desktop_entry(&self) -> PathBuf {
        self.data.path().join("applications/clackatoo.desktop")
    }
}

/// Whether the program at `path` is the clackatoo that was built.
fn is_the_build(path: &Path) -> bool {
    std::fs::read(path).unwrap() == std::fs::read(BINARY).unwrap()
}

#[test]
fn update_installs_a_signed_release_and_rollback_goes_back() {
    let key = Key::new();
    let mut releases = Releases::new();
    releases.serve_over_http();
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let installed = Installed::new();

    let check = installed.update(&["--check"], &releases, &key);
    assert!(check.status.success(), "{check:?}");
    assert!(
        stdout(&check).contains("clackatoo 9999.1.0 is out"),
        "{check:?}"
    );
    assert!(is_the_build(&installed.program()));

    let update = installed.update(&[], &releases, &key);
    assert!(update.status.success(), "{update:?}");
    let said = stdout(&update);
    assert!(said.contains("installed clackatoo 9999.1.0"), "{said}");
    assert!(said.contains("no daemon is running"), "{said}");
    assert_eq!(
        std::fs::read(installed.program()).unwrap(),
        clackatoo_update::fake::program("9999.1.0")
    );
    assert!(is_the_build(&installed.previous()));

    // The build replaced rolls back to itself, even run by its new name.
    let rollback = installed.run(
        &installed.previous(),
        &["update", "--rollback"],
        &releases,
        &key,
        installed.runtime.path(),
    );
    assert!(rollback.status.success(), "{rollback:?}");
    assert!(
        stdout(&rollback).contains(&format!("back to {}", env!("CARGO_PKG_VERSION"))),
        "{rollback:?}"
    );
    assert!(is_the_build(&installed.program()));
    assert_eq!(
        std::fs::read(installed.previous()).unwrap(),
        clackatoo_update::fake::program("9999.1.0")
    );
}

#[test]
fn update_refuses_a_release_not_signed_with_the_release_key() {
    let key = Key::new();
    let stranger = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("9999.1.0", &stranger), true);
    let installed = Installed::new();

    let update = installed.update(&[], &releases, &key);
    assert!(!update.status.success());
    assert!(
        stderr(&update).contains("isn't signed with the release key"),
        "{update:?}"
    );
    assert!(is_the_build(&installed.program()));
    assert!(!installed.previous().exists());
}

#[test]
fn update_refuses_a_build_that_isnt_what_the_checksums_say() {
    let key = Key::new();
    let releases = Releases::new();
    let mut fake = Fake::new("9999.1.0", &key);
    fake.wrong_sum = true;
    releases.publish(&fake, true);
    let installed = Installed::new();

    let update = installed.update(&[], &releases, &key);
    assert!(!update.status.success());
    assert!(
        stderr(&update).contains("isn't what the release's checksums say"),
        "{update:?}"
    );
    assert!(is_the_build(&installed.program()));
}

#[test]
fn update_says_when_this_is_the_latest_and_goes_to_a_version_asked_for() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("9999.1.0", &key), false);
    releases.publish(&Fake::new(env!("CARGO_PKG_VERSION"), &key), true);
    let installed = Installed::new();

    let update = installed.update(&[], &releases, &key);
    assert!(update.status.success(), "{update:?}");
    assert!(stdout(&update).contains("is up to date"), "{update:?}");
    assert!(is_the_build(&installed.program()));

    let missing = installed.update(&["--to", "2026.9.0"], &releases, &key);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("there's no release 2026.9.0"),
        "{missing:?}"
    );

    let nightly = installed.update(&["--channel", "nightly"], &releases, &key);
    assert!(!nightly.status.success());
    assert!(
        stderr(&nightly).contains("nightly builds aren't published yet"),
        "{nightly:?}"
    );

    let to = installed.update(&["--to", "9999.1.0"], &releases, &key);
    assert!(to.status.success(), "{to:?}");
    assert!(
        stdout(&to).contains("installed clackatoo 9999.1.0"),
        "{to:?}"
    );
}

/// A temporary runtime directory with a short path, since socket paths are
/// limited to about 100 bytes. Stops the daemon in it when dropped.
struct Runtime(tempfile::TempDir, Option<Child>);

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = Command::new(BINARY)
            .arg("stop")
            .env("XDG_RUNTIME_DIR", self.0.path())
            .output();
        if let Some(mut daemon) = self.1.take() {
            let _ = daemon.wait();
        }
    }
}

#[test]
fn update_moves_the_daemon_onto_the_new_build() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let installed = Installed::new();
    let runtime = tempfile::Builder::new()
        .prefix("ck")
        .tempdir_in("/tmp")
        .unwrap();
    let daemon = Command::new(BINARY)
        .arg("daemon")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .spawn()
        .unwrap();
    let runtime = Runtime(runtime, Some(daemon));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(Command::new(BINARY)
        .arg("list")
        .env("XDG_RUNTIME_DIR", runtime.0.path()))
    .output()
    .unwrap()
    .status
    .success()
    {
        assert!(Instant::now() < deadline, "the daemon never answered");
        std::thread::sleep(Duration::from_millis(10));
    }

    let update = installed.run(
        &installed.program(),
        &["update"],
        &releases,
        &key,
        runtime.0.path(),
    );
    assert!(update.status.success(), "{update:?}");
    // The fake build says it was asked to upgrade the daemon: the new
    // build does it, not the old one updating.
    assert!(
        stdout(&update).contains("clackatoo 9999.1.0 ran upgrade"),
        "{update:?}"
    );
}

#[test]
fn update_refreshes_the_launchers_entry_with_the_new_build_if_clackatoo_wrote_it() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let installed = Installed::new();
    let install = installed.run(
        &installed.program(),
        &["desktop", "install"],
        &releases,
        &key,
        installed.runtime.path(),
    );
    assert!(install.status.success(), "{install:?}");
    // As a build at another path would have written it.
    let entry = std::fs::read_to_string(installed.desktop_entry()).unwrap();
    let program = std::fs::canonicalize(installed.program()).unwrap();
    let elsewhere = entry.replace(program.to_str().unwrap(), "/old/clackatoo");
    assert_ne!(elsewhere, entry);
    std::fs::write(installed.desktop_entry(), elsewhere).unwrap();

    // The new build is the one asked, as it has the new icon.
    let update = installed.update(&[], &releases, &key);
    assert!(update.status.success(), "{update:?}");
    assert!(
        stdout(&update).contains("clackatoo 9999.1.0 ran desktop refresh"),
        "{update:?}"
    );

    // Going back, this build is asked, and the entry starts it again.
    let rollback = installed.run(
        &installed.previous(),
        &["update", "--rollback"],
        &releases,
        &key,
        installed.runtime.path(),
    );
    assert!(rollback.status.success(), "{rollback:?}");
    assert!(
        stdout(&rollback).contains("the app launcher's entry now starts"),
        "{rollback:?}"
    );
    assert_eq!(
        std::fs::read_to_string(installed.desktop_entry()).unwrap(),
        entry
    );
}

#[test]
fn update_leaves_a_launchers_entry_clackatoo_didnt_write() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let installed = Installed::new();
    let handmade = "[Desktop Entry]\nType=Application\nName=clackatoo\nExec=/src/clackatoo\n";
    std::fs::create_dir_all(installed.desktop_entry().parent().unwrap()).unwrap();
    std::fs::write(installed.desktop_entry(), handmade).unwrap();

    let update = installed.update(&[], &releases, &key);
    assert!(update.status.success(), "{update:?}");
    assert!(!stdout(&update).contains("ran desktop"), "{update:?}");
    assert_eq!(
        std::fs::read_to_string(installed.desktop_entry()).unwrap(),
        handmade
    );
}
