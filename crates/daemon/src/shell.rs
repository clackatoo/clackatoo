//! Shell integration: when the daemon starts the user's own shell, it gets
//! the shell to mark its prompts and commands (see `clackatoo_term`'s
//! blocks), unless the shell does that by itself.
//!
//! - fish 4 marks them by itself.
//! - bash runs [`BASH`] as its startup file, in place of `~/.bashrc`,
//!   which the script reads first.
//! - zsh is started with `ZDOTDIR` pointing at a directory holding
//!   [`ZSHENV`] as its `.zshenv`, which puts `ZDOTDIR` back as it was
//!   (kept for it in `CLACKATOO_ZSH_ZDOTDIR`), reads the user's own
//!   `.zshenv`, and loads [`ZSH`]. zsh then reads the user's other startup
//!   files as usual. This is how kitty and Ghostty do it. It's bypassed
//!   if `/etc/zsh/zshenv` sets `ZDOTDIR` itself.
//! - Other shells aren't marked.
//!
//! The daemon writes the scripts into its runtime directory when it
//! starts, so a shell always gets the running build's.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

/// bash's script.
const BASH: &str = include_str!("shell/bash.bash");

/// zsh's `.zshenv`, which loads [`ZSH`].
const ZSHENV: &str = include_str!("shell/zshenv.zsh");

/// zsh's script.
const ZSH: &str = include_str!("shell/clackatoo.zsh");

/// Where `ZDOTDIR` is kept for zsh's script to put back, when it was set.
const KEPT_ZDOTDIR: &str = "CLACKATOO_ZSH_ZDOTDIR";

/// How to start a shell with its integration.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Integration {
    /// Arguments to start it with.
    pub(crate) args: Vec<String>,
    /// Variables to set in its environment.
    pub(crate) env: Vec<(&'static str, OsString)>,
}

/// Writes the scripts into `dir`, replacing any an older build wrote, and
/// returns `dir`.
pub(crate) fn install(dir: PathBuf) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir.join("zsh"))?;
    write(&dir, "bash.bash", BASH)?;
    write(&dir, "zsh/.zshenv", ZSHENV)?;
    write(&dir, "zsh/clackatoo.zsh", ZSH)?;
    Ok(dir)
}

/// Writes `text` into `dir`'s file `name`, whole or not at all, as a
/// shell may be starting while it's replaced.
fn write(dir: &Path, name: &str, text: &str) -> io::Result<()> {
    let path = dir.join(name);
    let partial = dir.join(format!("{name}.{}", std::process::id()));
    std::fs::write(&partial, text)?;
    std::fs::rename(&partial, &path)
}

/// How to start `shell` with its integration from `dir`, if it needs any
/// and has one. `zdotdir` is `ZDOTDIR` as the shell would otherwise get
/// it.
pub(crate) fn integration(shell: &OsStr, dir: &Path, zdotdir: Option<&OsStr>) -> Integration {
    let name = Path::new(shell).file_name().and_then(OsStr::to_str);
    match name {
        Some("bash") => match dir.join("bash.bash").into_os_string().into_string() {
            Ok(script) => Integration {
                args: vec!["--rcfile".to_owned(), script],
                env: Vec::new(),
            },
            Err(_) => Integration::default(),
        },
        Some("zsh") => {
            let mut env = vec![("ZDOTDIR", dir.join("zsh").into_os_string())];
            if let Some(zdotdir) = zdotdir {
                env.push((KEPT_ZDOTDIR, zdotdir.to_owned()));
            }
            Integration {
                args: Vec::new(),
                env,
            }
        }
        _ => Integration::default(),
    }
}

/// Whether `shell`, started by the daemon with its integration from
/// `dir`, if the daemon has it, marks its prompts and commands: fish by
/// itself, bash and zsh by their scripts.
pub(crate) fn marks(shell: &OsStr, dir: Option<&Path>) -> bool {
    let name = Path::new(shell).file_name().and_then(OsStr::to_str);
    match name {
        Some("fish") => true,
        Some("bash" | "zsh") => dir.is_some(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    #[test]
    fn fish_marks_by_itself_and_bash_and_zsh_with_their_scripts() {
        let dir = Some(Path::new("/run/shell"));
        assert!(marks(OsStr::new("/usr/bin/fish"), None));
        assert!(marks(OsStr::new("/bin/zsh"), dir));
        assert!(marks(OsStr::new("bash"), dir));
        assert!(!marks(OsStr::new("/bin/bash"), None));
        assert!(!marks(OsStr::new("/bin/sh"), dir));
    }

    use super::*;

    fn os_args(shell: &str, dir: &Path) -> Vec<OsString> {
        integration(OsStr::new(shell), dir, None)
            .args
            .into_iter()
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn bash_is_given_a_script_and_zsh_a_zdotdir() {
        let dir = tempfile::tempdir().unwrap();
        let installed = install(dir.path().join("shell")).unwrap();
        let script = installed.join("bash.bash");
        assert_eq!(std::fs::read_to_string(&script).unwrap(), BASH);
        let zsh = installed.join("zsh");
        assert_eq!(
            std::fs::read_to_string(zsh.join(".zshenv")).unwrap(),
            ZSHENV
        );
        assert_eq!(
            std::fs::read_to_string(zsh.join("clackatoo.zsh")).unwrap(),
            ZSH
        );

        assert_eq!(
            os_args("/usr/bin/bash", &installed),
            ["--rcfile".into(), script.into_os_string()]
        );
        assert_eq!(
            integration(OsStr::new("/usr/bin/zsh"), &installed, None),
            Integration {
                args: Vec::new(),
                env: vec![("ZDOTDIR", zsh.clone().into_os_string())],
            }
        );
        // A ZDOTDIR of the user's own is kept for the script to put back.
        assert_eq!(
            integration(OsStr::new("zsh"), &installed, Some(OsStr::new("/z"))).env,
            [
                ("ZDOTDIR", zsh.into_os_string()),
                (KEPT_ZDOTDIR, "/z".into())
            ]
        );
        assert_eq!(
            integration(OsStr::new("/usr/bin/fish"), &installed, None),
            Integration::default()
        );
        assert!(os_args("/bin/sh", &installed).is_empty());
    }

    #[test]
    fn installing_again_replaces_the_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("shell");
        std::fs::create_dir_all(shell.join("zsh")).unwrap();
        std::fs::write(shell.join("zsh/.zshenv"), "old").unwrap();
        install(shell.clone()).unwrap();
        assert_eq!(
            std::fs::read_to_string(shell.join("zsh/.zshenv")).unwrap(),
            ZSHENV
        );
        let left: Vec<_> = std::fs::read_dir(shell.join("zsh")).unwrap().collect();
        assert_eq!(left.len(), 2, "no partial files are left");
    }
}
