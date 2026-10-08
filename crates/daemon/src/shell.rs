//! Shell integration: when the daemon starts the user's own shell, it gets
//! the shell to mark its prompts and commands (see `clackatoo_term`'s
//! blocks), unless the shell does that by itself.
//!
//! - fish 4 marks them by itself.
//! - bash runs [`BASH`] as its startup file, in place of `~/.bashrc`,
//!   which the script reads first.
//! - Other shells, zsh among them, aren't marked yet.
//!
//! The daemon writes the scripts into its runtime directory when it
//! starts, so a shell always gets the running build's.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// bash's script.
const BASH: &str = include_str!("shell/bash.bash");

/// Writes the scripts into `dir`, replacing any an older build wrote, and
/// returns `dir`.
pub(crate) fn install(dir: PathBuf) -> io::Result<PathBuf> {
    std::fs::create_dir_all(&dir)?;
    // Whole or not at all: a shell may be starting while it's replaced.
    let path = dir.join("bash.bash");
    let partial = dir.join(format!("bash.bash.{}", std::process::id()));
    std::fs::write(&partial, BASH)?;
    std::fs::rename(&partial, &path)?;
    Ok(dir)
}

/// The arguments that start `shell` with its integration from `dir`, if
/// it needs any and has one.
pub(crate) fn args(shell: &OsStr, dir: &Path) -> Vec<String> {
    let name = Path::new(shell).file_name();
    match (name.and_then(OsStr::to_str), dir.join("bash.bash").to_str()) {
        (Some("bash"), Some(script)) => vec!["--rcfile".to_owned(), script.to_owned()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn os_args(shell: &str, dir: &Path) -> Vec<OsString> {
        args(OsStr::new(shell), dir)
            .into_iter()
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn only_bash_is_given_a_script() {
        let dir = tempfile::tempdir().unwrap();
        let installed = install(dir.path().join("shell")).unwrap();
        let script = installed.join("bash.bash");
        assert_eq!(std::fs::read_to_string(&script).unwrap(), BASH);
        assert_eq!(
            os_args("/usr/bin/bash", &installed),
            ["--rcfile".into(), script.into_os_string()]
        );
        assert!(os_args("/usr/bin/fish", &installed).is_empty());
        assert!(os_args("/bin/sh", &installed).is_empty());
    }
}
