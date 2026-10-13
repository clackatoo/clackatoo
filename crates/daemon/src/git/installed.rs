//! Finding the git clackatoo runs, once, the first time it's needed.
//!
//! git is run by its absolute path, found through only the absolute
//! directories of `$PATH`, and is given that `$PATH` too: an empty or
//! relative entry would find a `git`, or a `git-lfs` for the shell git
//! runs a filter in, in whatever folder git is run in, which is a work
//! tree anyone could have filled.
//!
//! It must be git 2.38 or newer: older ones find a git directory by
//! itself whatever they're told (`safe.bareRepository` is 2.38's), such
//! as one committed inside a clone, with a config of its own. And it
//! must take settings from `GIT_CONFIG_COUNT` (2.31's), as filter drivers
//! are turned off with (see [`super::filters`]): a git that ignored them
//! would run them all. One that doesn't pass is never run; that's logged
//! once, and explorers say so. git installed or upgraded later is seen
//! when the daemon next starts.
//!
//! Git before 2.44 doesn't know `GIT_NO_LAZY_FETCH`, which stops it
//! fetching an object missing from a partial clone. That's still safe:
//! an empty `GIT_ALLOW_PROTOCOL` refuses every way of fetching, so the
//! commands a repository names to reach a remote or log in never run.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The oldest git clackatoo runs.
pub(crate) const OLDEST: (u32, u32) = (2, 38);

/// The git to run.
#[derive(Debug)]
pub(crate) struct Installed {
    /// Its absolute path.
    pub(crate) program: PathBuf,
    /// The `$PATH` it's given: the daemon's, without its relative entries.
    pub(crate) path: OsString,
}

/// The git to run, or why there's none, found the first time it's asked.
pub(crate) fn installed() -> Result<&'static Installed, &'static str> {
    static FOUND: OnceLock<Result<Installed, String>> = OnceLock::new();
    let found = FOUND.get_or_init(|| {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let found = find(&path);
        match &found {
            Ok(installed) => log::info!("git is {}", installed.program.display()),
            Err(why) => log::warn!("{why}"),
        }
        found
    });
    found.as_ref().map_err(String::as_str)
}

/// The git found through `path`, a `$PATH`, if it'll do, or why not.
fn find(path: &OsStr) -> Result<Installed, String> {
    let path = absolute_only(path);
    let program = find_program("git", &path)
        .ok_or_else(|| "git isn't installed, so there's no git status".to_owned())?;
    let installed = Installed { program, path };
    let said = run(&installed, &["--version"], &[])
        .ok_or_else(|| "git didn't say its version, so it isn't run".to_owned())?;
    let said = String::from_utf8_lossy(&said);
    if version(&said).is_none_or(|version| version < OLDEST) {
        let (major, minor) = OLDEST;
        return Err(format!(
            "git {major}.{minor} or newer is needed for git status, not {}",
            said.trim().trim_start_matches("git version ")
        ));
    }
    // The settings that turn filter drivers off are given this way.
    let probed = run(
        &installed,
        &["config", "--show-scope", "--get", "clackatoo.probe"],
        &[
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_CONFIG_KEY_0", "clackatoo.probe"),
            ("GIT_CONFIG_VALUE_0", "1"),
        ],
    );
    if probed.as_deref() != Some(b"command\t1\n") {
        return Err("git ignores settings given in its environment, so it isn't run".to_owned());
    }
    Ok(installed)
}

/// What `installed` printed when run with `args` and `env`, outside any
/// repository, if it succeeded.
fn run(installed: &Installed, args: &[&str], env: &[(&str, &str)]) -> Option<Vec<u8>> {
    let mut command = Command::new(&installed.program);
    command
        .args(args)
        // As `git()` runs it: a `GIT_CONFIG` in the daemon's environment
        // would make the probe read that file alone, and fail.
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .envs(env.iter().copied())
        .env("PATH", &installed.path)
        .current_dir("/")
        .stdin(Stdio::null());
    super::output_within(&mut command, super::PATIENCE)
}

/// `path`, a `$PATH`, with only its absolute directories.
fn absolute_only(path: &OsStr) -> OsString {
    let kept: Vec<&[u8]> = (path.as_bytes().split(|&byte| byte == b':'))
        .filter(|dir| dir.starts_with(b"/"))
        .collect();
    OsString::from(OsStr::from_bytes(&kept.join(&b':')))
}

/// The first file called `name` that may be run in the directories of
/// `path`, a `$PATH`.
fn find_program(name: &str, path: &OsStr) -> Option<PathBuf> {
    (path.as_bytes().split(|&byte| byte == b':'))
        .filter(|dir| dir.starts_with(b"/"))
        .map(|dir| Path::new(OsStr::from_bytes(dir)).join(name))
        .find(|program| {
            std::fs::metadata(program)
                .is_ok_and(|found| found.is_file() && found.permissions().mode() & 0o111 != 0)
        })
}

/// The major and minor version in what `git --version` printed: "git
/// version 2.53.0", or "git version 2.39.3 (Apple Git-146)".
fn version(said: &str) -> Option<(u32, u32)> {
    let number = said.trim().strip_prefix("git version ")?;
    let mut parts = number.split(|ch: char| !ch.is_ascii_digit());
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_as_git_prints_them() {
        assert_eq!(version("git version 2.53.0\n"), Some((2, 53)));
        assert_eq!(version("git version 2.44.0.windows.1"), Some((2, 44)));
        assert_eq!(version("git version 2.39.3 (Apple Git-146)"), Some((2, 39)));
        assert_eq!(version("git version 3.0"), Some((3, 0)));
        assert_eq!(version("hub version 2.53.0"), None);
        assert!(version("git version 10.1.0").unwrap() >= OLDEST);
    }

    #[test]
    fn git_2_38_is_the_oldest_run() {
        assert!(version("git version 2.37.7").unwrap() < OLDEST);
        assert!(version("git version 2.38.0").unwrap() >= OLDEST);
        // Ubuntu 24.04's.
        assert!(version("git version 2.43.0").unwrap() >= OLDEST);
    }

    #[test]
    fn only_absolute_directories_are_searched() {
        assert_eq!(
            absolute_only(OsStr::new(":/usr/bin:.:bin::/bin:")),
            OsStr::new("/usr/bin:/bin")
        );
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("tool");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        // Not one that can't be run.
        let path = OsString::from(dir.path());
        assert_eq!(find_program("tool", &path), None);
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_program("tool", &path), Some(program));
        // Nor one in a relative directory, wherever it's run.
        assert_eq!(find_program("tool", OsStr::new(".:tool")), None);
    }

    /// A stand-in for git, in `dir`, that runs `script`.
    fn stand_in(dir: &Path, script: &str) -> OsString {
        let program = dir.join("git");
        std::fs::write(&program, format!("#!/bin/sh\n{script}")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        OsString::from(dir)
    }

    #[test]
    fn an_old_git_or_one_that_ignores_its_environment_is_never_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = stand_in(dir.path(), "echo 'git version 2.37.1'\n");
        let why = find(&path).unwrap_err();
        assert_eq!(
            why,
            "git 2.38 or newer is needed for git status, not 2.37.1"
        );
        // New enough, but as git 2.30 was: GIT_CONFIG_COUNT is ignored.
        let script = "[ \"$1\" = --version ] && echo 'git version 2.53.0' && exit 0\nexit 1\n";
        let path = stand_in(dir.path(), script);
        assert!(find(&path).unwrap_err().contains("ignores settings"));
        // One that takes them is run.
        let script = "[ \"$1\" = --version ] && echo 'git version 2.53.0' && exit 0\n\
                      [ \"$GIT_CONFIG_KEY_0\" = clackatoo.probe ] && printf 'command\\t1\\n'\n";
        let path = stand_in(dir.path(), script);
        let installed = find(&path).unwrap();
        assert_eq!(installed.program, dir.path().join("git"));
        assert!(
            find(OsStr::new(""))
                .unwrap_err()
                .contains("isn't installed")
        );
    }

    #[test]
    fn the_git_here_is_run() {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let installed = find(&path).expect("git 2.38 or newer is installed");
        assert!(installed.program.is_absolute());
    }
}
