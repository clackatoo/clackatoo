//! `scripts/release`, run in a repository of its own made for the test.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The script, from the workspace's root.
fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/release")
}

/// A git repository holding a workspace of one crate, at version 0.1.0,
/// with git's own config kept out, so nothing of the user's applies.
struct Repo(tempfile::TempDir);

impl Repo {
    fn new() -> Self {
        let repo = Self(tempfile::tempdir().unwrap());
        repo.git(&["init", "--quiet", "--initial-branch=main"]);
        repo.write(
            "Cargo.toml",
            "[workspace]\nresolver = \"3\"\nmembers = [\"one\"]\n\n[workspace.package]\n\
             version = \"0.1.0\"\nedition = \"2024\"\n\n[workspace.dependencies]\n",
        );
        repo.write(
            "one/Cargo.toml",
            "[package]\nname = \"one\"\nversion.workspace = true\nedition.workspace = true\n",
        );
        repo.write("one/src/lib.rs", "");
        let lock = Command::new(cargo())
            .args(["generate-lockfile", "--offline", "--quiet"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(lock.status.success(), "{lock:?}");
        repo.git(&["add", "."]);
        repo.commit("chore: start", "");
        repo
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.path().join(path)).unwrap()
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(self.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("CARGO", cargo());
        command
    }

    fn git(&self, args: &[&str]) -> String {
        let output = self.command("git").args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Commits a change with this message.
    fn commit(&self, subject: &str, body: &str) {
        let count = self
            .git(&["rev-list", "--all", "--count"])
            .trim()
            .parse()
            .unwrap_or(0);
        self.write("one/src/lib.rs", &format!("// {count}\n"));
        self.git(&["add", "."]);
        let mut args = vec!["commit", "--quiet", "-m", subject];
        if !body.is_empty() {
            args.extend(["-m", body]);
        }
        self.git(&args);
    }

    /// Runs the script with `args`, on `date`.
    fn release(&self, date: &str, args: &[&str]) -> Output {
        self.command("bash")
            .arg(script())
            .args(args)
            .env("RELEASE_DATE", date)
            .output()
            .unwrap()
    }
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

#[test]
fn a_release_bumps_the_version_writes_the_changelog_commits_and_tags() {
    let repo = Repo::new();
    repo.commit("feat(gui): a button", "");
    repo.commit("fix: the button's colour", "");
    repo.commit("test: the button", "");
    repo.commit("perf: drawing the button", "");
    repo.commit("not a conventional commit", "");
    repo.commit("feat!: buttons are round", "");
    repo.commit(
        "refactor: tidy",
        "BREAKING CHANGE: the config's [button] is gone",
    );

    let dry = repo.release("2026-10-13", &["--dry-run"]);
    assert!(dry.status.success(), "{dry:?}");
    assert!(String::from_utf8_lossy(&dry.stdout).contains("would release v2026.10.0"));
    assert!(repo.git(&["tag"]).is_empty());
    assert!(repo.read("Cargo.toml").contains("version = \"0.1.0\""));

    let made = repo.release("2026-10-13", &[]);
    assert!(made.status.success(), "{made:?}");
    assert!(String::from_utf8_lossy(&made.stdout).contains("git push origin HEAD v2026.10.0"));

    assert!(
        repo.read("Cargo.toml")
            .contains("[workspace.package]\nversion = \"2026.10.0\"")
    );
    assert!(
        repo.read("Cargo.lock")
            .contains("name = \"one\"\nversion = \"2026.10.0\"")
    );
    let changelog = repo.read("CHANGELOG.md");
    assert!(
        changelog.starts_with("# Changelog\n\n## 2026.10.0 (2026-10-13)\n"),
        "{changelog}"
    );
    let section = |name: &str| {
        let start = changelog
            .find(&format!("### {name}\n"))
            .unwrap_or_else(|| panic!("{name}: {changelog}"));
        let rest = &changelog[start..];
        rest[4..]
            .find("\n### ")
            .map_or(rest, |end| &rest[..end + 4])
            .to_owned()
    };
    let breaking = section("Breaking changes");
    assert!(breaking.contains("- buttons are round ("), "{breaking}");
    assert!(breaking.contains("- tidy ("), "{breaking}");
    let features = section("Features");
    assert!(features.contains("- **gui:** a button ("), "{features}");
    assert!(features.contains("- buttons are round ("), "{features}");
    assert!(section("Fixes").contains("- the button's colour ("));
    assert!(section("Speed").contains("- drawing the button ("));
    for left_out in ["the button\n", "not a conventional", "start"] {
        assert!(!changelog.contains(left_out), "{left_out:?}: {changelog}");
    }

    assert_eq!(
        repo.git(&["log", "-1", "--format=%s"]).trim(),
        "chore(release): v2026.10.0"
    );
    assert_eq!(repo.git(&["tag"]).trim(), "v2026.10.0");
    assert_eq!(repo.git(&["cat-file", "-t", "v2026.10.0"]).trim(), "tag");
    assert!(repo.git(&["status", "--porcelain"]).is_empty());

    // The next that month counts on, with only what came since.
    repo.commit("fix: round buttons are clickable at the edge", "");
    let next = repo.release("2026-10-20", &[]);
    assert!(next.status.success(), "{next:?}");
    let changelog = repo.read("CHANGELOG.md");
    assert!(
        changelog.starts_with(
            "# Changelog\n\n## 2026.10.1 (2026-10-20)\n\n### Fixes\n\n- round buttons"
        ),
        "{changelog}"
    );
    let older = changelog.find("## 2026.10.0").unwrap();
    assert!(!changelog[..older].contains("a button"), "{changelog}");
    assert!(
        changelog[older..].contains("- **gui:** a button ("),
        "{changelog}"
    );

    // A new month starts again from 0, and a release with nothing to
    // note says so.
    repo.commit("chore: dependencies", "");
    let november = repo.release("2026-11-02", &[]);
    assert!(november.status.success(), "{november:?}");
    assert!(repo.git(&["tag"]).contains("v2026.11.0"));
    assert!(
        repo.read("CHANGELOG.md")
            .contains("## 2026.11.0 (2026-11-02)\n\nNothing to note")
    );
}

#[test]
fn a_release_is_refused_with_changes_not_committed_or_a_version_tagged_already() {
    let repo = Repo::new();
    repo.commit("feat: a", "");
    repo.write("one/src/lib.rs", "// not committed\n");
    let dirty = repo.release("2026-10-13", &[]);
    assert!(!dirty.status.success());
    assert!(String::from_utf8_lossy(&dirty.stderr).contains("not committed"));
    repo.git(&["checkout", "--", "."]);

    assert!(repo.release("2026-10-13", &["2026.10.5"]).status.success());
    assert!(repo.git(&["tag"]).contains("v2026.10.5"));
    // Counting on from the highest this month.
    repo.commit("fix: b", "");
    assert!(repo.release("2026-10-14", &[]).status.success());
    assert!(repo.git(&["tag"]).contains("v2026.10.6"));

    let again = repo.release("2026-10-14", &["2026.10.6"]);
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("tagged already"));
    let wrong = repo.release("2026-10-14", &["2026.10"]);
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("isn't a version"));
}

#[test]
fn a_release_commits_the_protocol_fixture_once_per_protocol_version() {
    let repo = Repo::new();
    // A protocol crate whose fixture writer, as the real one does, writes
    // the fixture if it isn't there, and fails if it's there and differs.
    repo.write(
        "Cargo.toml",
        "[workspace]\nresolver = \"3\"\nmembers = [\"one\", \"crates/protocol\"]\n\n\
         [workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [workspace.dependencies]\n",
    );
    repo.write(
        "crates/protocol/Cargo.toml",
        "[package]\nname = \"clackatoo-protocol\"\nversion.workspace = true\n\
         edition.workspace = true\n",
    );
    repo.write("crates/protocol/src/lib.rs", "");
    repo.write(".gitignore", "/target\n");
    repo.write(
        "crates/protocol/examples/fixture.rs",
        r#"fn main() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let path = dir.join("protocol-v1.bin");
    match std::fs::read(&path) {
        Ok(there) if there == b"one" => {}
        Ok(_) => std::process::exit(1),
        Err(_) => {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, b"one").unwrap();
        }
    }
    println!("{}", path.display());
}
"#,
    );
    let lock = repo
        .command(cargo())
        .args(["generate-lockfile", "--offline", "--quiet"])
        .output()
        .unwrap();
    assert!(lock.status.success(), "{lock:?}");
    repo.git(&["add", "."]);
    repo.commit("feat: a protocol", "");

    repo.write("crates/protocol/fixtures/stray", "not the release's");
    let made = repo.release("2026-10-13", &[]);
    assert!(made.status.success(), "{made:?}");
    let committed = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
    assert!(
        committed.contains("crates/protocol/fixtures/protocol-v1.bin"),
        "{committed}"
    );
    // Only what the writer named: not a file left beside it.
    assert!(!committed.contains("stray"), "{committed}");
    assert!(repo.git(&["status", "--porcelain"]).contains("stray"));
    std::fs::remove_file(repo.path().join("crates/protocol/fixtures/stray")).unwrap();

    // The next release at the same protocol version leaves it as it is.
    repo.commit("fix: something", "");
    let next = repo.release("2026-10-14", &[]);
    assert!(next.status.success(), "{next:?}");
    let committed = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
    assert!(!committed.contains("fixtures"), "{committed}");
    assert_eq!(repo.read("crates/protocol/fixtures/protocol-v1.bin"), "one");

    // One whose protocol encodes otherwise, unbumped, is stopped.
    repo.write("crates/protocol/fixtures/protocol-v1.bin", "two");
    repo.git(&["add", "."]);
    repo.commit("feat: a changed protocol", "");
    let refused = repo.release("2026-10-15", &[]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("fixture couldn't be written"),
        "{refused:?}"
    );
    assert!(!repo.git(&["tag"]).contains("v2026.10.2"));
}
