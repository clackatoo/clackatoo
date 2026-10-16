//! Shell integration: when the daemon starts the user's own shell, it gets
//! the shell to mark its prompts and commands (see `clackatoo_term`'s
//! blocks), unless the shell does that by itself.
//!
//! - fish 4 marks them by itself, and runs [`FISH`] once its startup
//!   files have run (`fish -C 'source FISH'`), which marks each command
//!   again with the terminal's secret (see below).
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
//!
//! ## The secret
//!
//! Any program a shell runs can print OSC 133 marks, saying any command
//! is running. So that a command offered to run again after a restart is
//! one the shell really ran, each terminal whose shell gets its
//! integration has a [`Secret`] of its own, which the integration adds to
//! its `C` mark (`clackatoo=SECRET`), and `clackatoo_term` trusts only
//! a mark carrying it.
//!
//! The shell is given it in a file only the user can read (0600, in the
//! runtime directory), named in `CLACKATOO_SECRET_FILE`. The integration
//! reads it first thing, removes the file, unsets the variable, and
//! keeps the secret in a variable of its own that isn't exported. The
//! daemon removes the file too once the shell has marked anything, and
//! when the terminal goes, in case the integration never ran.
//!
//! Not the secret itself in the environment, unset by the integration:
//! the environment a program started with stays readable in
//! `/proc/PID/environ` for its whole life, by anything running as the
//! user, unset or not, and whatever the startup files start before the
//! integration runs would inherit it. A file holds it only until the
//! shell has read it, and the variable names a file that's gone.
//!
//! The marks are read and dropped by the terminal, so the secret never
//! reaches its text. A recording (`NewTerminal::record_to`) keeps every
//! byte the program wrote, the marks with it: the secret it holds is good
//! only while that terminal lives.

use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use ring::rand::{SecureRandom, SystemRandom};

/// bash's script.
const BASH: &str = include_str!("shell/bash.bash");

/// zsh's `.zshenv`, which loads [`ZSH`].
const ZSHENV: &str = include_str!("shell/zshenv.zsh");

/// zsh's script.
const ZSH: &str = include_str!("shell/clackatoo.zsh");

/// fish's script.
const FISH: &str = include_str!("shell/clackatoo.fish");

/// Names the file holding the terminal's secret, for the integration.
const SECRET_FILE: &str = "CLACKATOO_SECRET_FILE";

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

impl Integration {
    /// Whether there's nothing to it: the shell has no integration.
    pub(crate) fn is_empty(&self) -> bool {
        self.args.is_empty() && self.env.is_empty()
    }

    /// Tells the shell its secret is in `file`.
    pub(crate) fn tell_secret(&mut self, file: &Path) {
        self.env.push((SECRET_FILE, file.into()));
    }
}

/// Writes the scripts into `dir`, replacing any an older build wrote, and
/// returns `dir`. A daemon starting `afresh` removes the secrets' files
/// a daemon that crashed left there; one taking over from an upgrade
/// keeps them, as they're its terminals'.
pub(crate) fn install(dir: PathBuf, afresh: bool) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir.join("zsh"))?;
    if afresh {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with("secret.") {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    write(&dir, "bash.bash", BASH)?;
    write(&dir, "zsh/.zshenv", ZSHENV)?;
    write(&dir, "zsh/clackatoo.zsh", ZSH)?;
    write(&dir, "clackatoo.fish", FISH)?;
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
    match name(shell) {
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
        Some("fish") => match dir.join("clackatoo.fish").to_str() {
            Some(script) => Integration {
                args: vec!["-C".to_owned(), format!("source {}", fish_quoted(script))],
                env: Vec::new(),
            },
            None => Integration::default(),
        },
        _ => Integration::default(),
    }
}

/// `shell`'s name, without its directory.
fn name(shell: &OsStr) -> Option<&str> {
    Path::new(shell).file_name().and_then(OsStr::to_str)
}

/// `text` in fish's single quotes, inside which only `\'` and `\\` mean
/// anything.
fn fish_quoted(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}

/// Whether `shell` has an integration that adds the terminal's secret to
/// its marks: bash, zsh and fish.
pub(crate) fn takes_secret(shell: &OsStr) -> bool {
    matches!(name(shell), Some("bash" | "zsh" | "fish"))
}

/// A terminal's secret, which its shell's integration adds to its `C`
/// marks (see the [module docs](self)): 16 random bytes, in hex.
#[derive(Debug)]
pub(crate) struct Secret {
    pub(crate) text: String,
    /// The file the shell reads it from, until it's removed.
    pub(crate) file: Option<SecretFile>,
}

impl Secret {
    /// A new secret for terminal `id`, written in `dir` for its shell to
    /// read.
    pub(crate) fn new(dir: &Path, id: u32) -> io::Result<Self> {
        let mut bytes = [0; 16];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| io::Error::other("no random numbers to make a secret of"))?;
        let text = bytes.iter().fold(String::new(), |mut text, byte| {
            use std::fmt::Write as _;
            write!(text, "{byte:02x}").expect("a String takes anything written");
            text
        });
        let path = dir.join(format!("secret.{id}"));
        let open = || {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
                .open(&path)
        };
        // One left by a daemon that crashed is no one's now.
        let mut written = match open() {
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                std::fs::remove_file(&path)?;
                open()?
            }
            opened => opened?,
        };
        let file = SecretFile(path);
        writeln!(written, "{text}")?;
        Ok(Self {
            text,
            file: Some(file),
        })
    }
}

/// The file a shell reads its terminal's secret from, removed when this is
/// dropped, if the shell hasn't removed it itself.
#[derive(Debug)]
pub(crate) struct SecretFile(PathBuf);

impl SecretFile {
    /// The file, kept, for the next build to remove.
    pub(crate) fn keep(mut self) -> PathBuf {
        std::mem::take(&mut self.0)
    }

    /// A file the last build handed over.
    pub(crate) fn kept(path: PathBuf) -> Self {
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for SecretFile {
    fn drop(&mut self) {
        // Kept for the next build.
        if self.0.as_os_str().is_empty() {
            return;
        }
        if let Err(err) = std::fs::remove_file(&self.0)
            && err.kind() != io::ErrorKind::NotFound
        {
            log::warn!("{}: removing it failed: {err}", self.0.display());
        }
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
        let installed = install(dir.path().join("shell"), true).unwrap();
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
        assert!(os_args("/bin/sh", &installed).is_empty());
    }

    #[test]
    fn fish_is_given_its_script_to_run_after_its_startup_files() {
        let dir = tempfile::tempdir().unwrap();
        let installed = install(dir.path().join("it's \\ here"), true).unwrap();
        let script = installed.join("clackatoo.fish");
        assert_eq!(std::fs::read_to_string(&script).unwrap(), FISH);
        let quoted = format!("source '{}/it\\'s \\\\ here/clackatoo.fish'", {
            dir.path().display()
        });
        assert_eq!(
            os_args("/usr/bin/fish", &installed),
            [OsString::from("-C"), quoted.into()]
        );
    }

    #[test]
    fn each_shell_with_an_integration_is_told_where_its_secret_is() {
        let dir = tempfile::tempdir().unwrap();
        let installed = install(dir.path().join("shell"), true).unwrap();
        let secret = installed.join("secret.1");
        for shell in ["bash", "/bin/zsh", "fish"] {
            assert!(takes_secret(OsStr::new(shell)));
            let mut integration = integration(OsStr::new(shell), &installed, None);
            assert!(!integration.is_empty());
            integration.tell_secret(&secret);
            let told = (SECRET_FILE, secret.clone().into());
            assert_eq!(integration.env.last(), Some(&told));
        }
        assert!(!takes_secret(OsStr::new("/bin/sh")));
        assert!(integration(OsStr::new("/bin/sh"), &installed, None).is_empty());
        // Scripts bash and fish can't be told the way to have none, so
        // their shells get no secret.
        let unnamed = <OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"\xff");
        let unnamed = dir.path().join(unnamed);
        assert!(integration(OsStr::new("bash"), &unnamed, None).is_empty());
        assert!(integration(OsStr::new("fish"), &unnamed, None).is_empty());
    }

    #[test]
    fn a_fresh_start_removes_secrets_left_behind_and_a_resumed_one_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let shell = install(dir.path().join("shell"), true).unwrap();
        std::fs::write(shell.join("secret.3"), "left\n").unwrap();
        install(shell.clone(), false).unwrap();
        assert!(
            shell.join("secret.3").exists(),
            "kept for the terminal handed over"
        );
        install(shell.clone(), true).unwrap();
        assert!(!shell.join("secret.3").exists());
        assert!(shell.join("bash.bash").exists());
    }

    #[test]
    fn a_secret_is_written_for_the_user_alone_and_removed_when_dropped() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        // One a crashed daemon left is replaced.
        std::fs::write(dir.path().join("secret.7"), "stale\n").unwrap();
        let secret = Secret::new(dir.path(), 7).unwrap();
        assert_eq!(secret.text.len(), 32);
        assert!(secret.text.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let path = secret.file.as_ref().unwrap().path().to_owned();
        assert_eq!(path, dir.path().join("secret.7"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", secret.text)
        );
        let other = Secret::new(dir.path(), 8).unwrap();
        assert_ne!(other.text, secret.text);
        drop(secret);
        assert!(!path.exists());
        // Kept for the next build, it stays.
        let kept = other.file.unwrap().keep();
        assert!(kept.exists());
        drop(SecretFile::kept(kept.clone()));
        assert!(!kept.exists());
        // A link put in its place isn't followed.
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), dir.path().join("secret.9"))
            .unwrap();
        let replaced = Secret::new(dir.path(), 9).unwrap();
        assert!(!dir.path().join("elsewhere").exists());
        drop(replaced);
    }

    #[test]
    fn installing_again_replaces_the_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("shell");
        std::fs::create_dir_all(shell.join("zsh")).unwrap();
        std::fs::write(shell.join("zsh/.zshenv"), "old").unwrap();
        install(shell.clone(), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(shell.join("zsh/.zshenv")).unwrap(),
            ZSHENV
        );
        let left: Vec<_> = std::fs::read_dir(shell.join("zsh")).unwrap().collect();
        assert_eq!(left.len(), 2, "no partial files are left");
    }
}
