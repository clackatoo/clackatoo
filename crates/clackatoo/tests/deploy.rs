//! `scripts/deploy`, run in a repository of its own made for the test, with
//! `gh`, `cargo` and the build it makes standing in for the real ones.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The script, from the workspace's root.
fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/deploy")
}

/// A repository at a release tag, and programs standing in for `gh`,
/// which says the release is published, and `cargo`, whose build writes
/// down what it's asked to do in `log`, failing `desktop` if `refresh`
/// is to fail.
struct Setup {
    dir: tempfile::TempDir,
}

impl Setup {
    fn new(refresh: Result<(), ()>) -> Self {
        let setup = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(setup.repo()).unwrap();
        std::fs::create_dir_all(setup.bin()).unwrap();
        setup.git(&["init", "--quiet", "--initial-branch=main"]);
        std::fs::write(setup.repo().join("README"), "clackatoo\n").unwrap();
        setup.git(&["add", "."]);
        setup.git(&["commit", "--quiet", "-m", "chore(release): v2026.10.1"]);
        setup.git(&["tag", "v2026.10.1"]);
        let desktop = match refresh {
            Ok(()) => "exit 0",
            Err(()) => "exit 1",
        };
        let build = format!(
            "#!/bin/sh\necho \"$*\" >> \"$DEPLOY_LOG\"\n[ \"$1\" = desktop ] && {desktop}\nexit 0\n"
        );
        // Written by a child, as programs written in a test's own process
        // can be held open by children other tests start.
        let programs = [
            (
                "gh",
                "#!/bin/sh\necho clackatoo-x86_64-linux.gz\n".to_owned(),
            ),
            (
                "cargo",
                format!(
                    "#!/bin/sh\nmkdir -p target/release\ncat > target/release/clackatoo <<'EOF'\n\
                     {build}EOF\nchmod +x target/release/clackatoo\n"
                ),
            ),
        ];
        for (name, text) in programs {
            let path = setup.bin().join(name);
            let written = Command::new("sh")
                .args(["-c", "cat > \"$1\" && chmod +x \"$1\"", "sh"])
                .arg(&path)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child.stdin.take().unwrap().write_all(text.as_bytes())?;
                    child.wait()
                })
                .unwrap();
            assert!(written.success());
        }
        setup
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    fn log(&self) -> PathBuf {
        self.dir.path().join("log")
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let path = format!(
            "{}:{}",
            self.bin().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(program);
        command
            .current_dir(self.repo())
            .env("PATH", path)
            .env("DEPLOY_LOG", self.log())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com");
        command
    }

    fn git(&self, args: &[&str]) {
        let output = self.command("git").args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    fn deploy(&self) -> Output {
        self.command("bash").arg(script()).output().unwrap()
    }

    /// What the build was asked to do, a line each.
    fn asked(&self) -> String {
        std::fs::read_to_string(self.log()).unwrap_or_default()
    }
}

#[test]
fn the_launchers_entry_is_rewritten_after_the_upgrade() {
    let setup = Setup::new(Ok(()));
    let output = setup.deploy();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(setup.asked(), "upgrade\ndesktop refresh\n");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn an_entry_not_rewritten_doesnt_fail_a_deploy_that_upgraded() {
    let setup = Setup::new(Err(()));
    let output = setup.deploy();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(setup.asked(), "upgrade\ndesktop refresh\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr,
        "deploy: upgraded to v2026.10.1, but the app launcher's entry wasn't rewritten\n"
    );
}
