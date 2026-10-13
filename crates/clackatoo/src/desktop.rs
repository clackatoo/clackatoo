//! `clackatoo desktop`: puts clackatoo in the desktop's app launcher.
//!
//! Launchers such as GNOME's list the desktop entries (`.desktop` files)
//! in `$XDG_DATA_HOME/applications`, and docks group windows under the
//! entry whose `StartupWMClass` is the window's Wayland app ID and X11
//! class, `clackatoo` (see `window_attributes` in the gui crate). The icon
//! the entry names goes in the hicolor theme beside it. Both files are
//! built into the program, so any build can install them.
//!
//! Files clackatoo writes say so: `X-Clackatoo-Managed=true` in the entry,
//! a comment in the icon. Only those are replaced or removed without
//! `--force`, so an entry someone wrote by hand stays theirs.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context as _;
use clap::Subcommand;

/// The desktop entry, as shipped. Its `Exec` and `TryExec` say plain
/// `clackatoo`, for packagers; [`entry`] puts a build's path there.
const ENTRY: &str = include_str!("../../../assets/clackatoo.desktop");

/// The icon, as shipped.
const ICON: &str = include_str!("../../../assets/clackatoo.svg");

/// The line that marks an entry as clackatoo's.
const MANAGED: &str = "X-Clackatoo-Managed=true";

/// The comment that marks an icon as clackatoo's.
const ICON_MANAGED: &str = "<!-- X-Clackatoo-Managed: written by `clackatoo desktop install` -->";

#[derive(Clone, Copy, Debug, Subcommand)]
pub(crate) enum DesktopCommand {
    /// Add clackatoo to your desktop's app launcher, as an entry that
    /// starts this build, with clackatoo's icon. An entry or icon
    /// clackatoo didn't write is left alone.
    Install {
        /// Replace an entry or icon clackatoo didn't write.
        #[arg(long)]
        force: bool,
    },
    /// Take clackatoo out of the app launcher. An entry or icon clackatoo
    /// didn't write is left alone.
    Remove {
        /// Remove them even if clackatoo didn't write them.
        #[arg(long)]
        force: bool,
    },
    /// Rewrite the entry and icon as this build would, if clackatoo wrote
    /// them. `clackatoo update` runs this with the build it installed.
    #[command(hide = true)]
    Refresh,
}

pub(crate) fn run(command: DesktopCommand) -> anyhow::Result<()> {
    let data = data_dir().context(
        "there's nowhere to put the launcher's entry: neither XDG_DATA_HOME nor HOME is set",
    )?;
    let paths = Paths::new(&data);
    match command {
        DesktopCommand::Install { force } => {
            let program = program()?;
            match install(&paths, &program, force)? {
                Outcome::Done(_) => {
                    update_caches(&paths);
                    println!(
                        "added clackatoo to the app launcher, starting {}: wrote {} and {}",
                        program.display(),
                        paths.entry().display(),
                        paths.icon().display()
                    );
                    Ok(())
                }
                Outcome::Refused(theirs) => anyhow::bail!(
                    "{} clackatoo's, so nothing was written; `clackatoo desktop install --force` \
                     replaces {}. The entry would be:\n\n{}",
                    are_not(&theirs),
                    them(&theirs),
                    entry(&program)?.trim_end()
                ),
            }
        }
        DesktopCommand::Remove { force } => match remove(&paths, force)? {
            Outcome::Done(removed) if removed.is_empty() => {
                println!("clackatoo isn't in the app launcher");
                Ok(())
            }
            Outcome::Done(removed) => {
                update_caches(&paths);
                let removed: Vec<String> = (removed.iter())
                    .map(|path| path.display().to_string())
                    .collect();
                println!(
                    "took clackatoo out of the app launcher: removed {}",
                    removed.join(" and ")
                );
                Ok(())
            }
            Outcome::Refused(theirs) => anyhow::bail!(
                "{} clackatoo's, so nothing was removed; `clackatoo desktop remove --force` \
                 removes {}",
                are_not(&theirs),
                them(&theirs)
            ),
        },
        DesktopCommand::Refresh => {
            let program = program()?;
            if refresh(&paths, &program)? {
                update_caches(&paths);
                println!("the app launcher's entry now starts {}", program.display());
            }
            Ok(())
        }
    }
}

/// `paths` and "isn't" or "aren't", for a sentence.
fn are_not(paths: &[PathBuf]) -> String {
    let names: Vec<String> = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let verb = if paths.len() == 1 { "isn't" } else { "aren't" };
    format!("{} {verb}", names.join(" and "))
}

/// "it" or "them", for `paths`.
fn them(paths: &[PathBuf]) -> &'static str {
    if paths.len() == 1 { "it" } else { "them" }
}

/// Where programs keep their data: `$XDG_DATA_HOME`, or else
/// `~/.local/share`. The spec says a relative `$XDG_DATA_HOME` is to be
/// ignored, and a relative `$HOME` would put the files wherever the
/// command happened to be run, so it's ignored too.
pub(crate) fn data_dir() -> Option<PathBuf> {
    let absolute =
        |name: &str| (std::env::var_os(name).map(PathBuf::from)).filter(|dir| dir.is_absolute());
    absolute("XDG_DATA_HOME").or_else(|| Some(absolute("HOME")?.join(".local/share")))
}

/// The program the entry should start: this one, with any links followed,
/// or if it's the build an update replaced, the one that replaced it.
/// Updates rename new builds over the same path, so it lasts.
fn program() -> anyhow::Result<PathBuf> {
    let running = std::env::current_exe().context("this program can't be found")?;
    let running = fs::canonicalize(&running).unwrap_or(running);
    Ok(clackatoo_update::install::installed(&running))
}

/// Where the files go, under a data directory such as `~/.local/share`.
#[derive(Debug)]
pub(crate) struct Paths {
    applications: PathBuf,
    hicolor: PathBuf,
}

impl Paths {
    pub(crate) fn new(data: &Path) -> Self {
        Self {
            applications: data.join("applications"),
            hicolor: data.join("icons/hicolor"),
        }
    }

    fn entry(&self) -> PathBuf {
        self.applications.join("clackatoo.desktop")
    }

    fn icon(&self) -> PathBuf {
        self.hicolor.join("scalable/apps/clackatoo.svg")
    }
}

/// What `install` or `remove` did.
#[derive(Debug, PartialEq, Eq)]
#[must_use]
pub(crate) enum Outcome {
    /// It's done, to these files.
    Done(Vec<PathBuf>),
    /// Nothing was done, because these files aren't clackatoo's.
    Refused(Vec<PathBuf>),
}

/// Writes the entry, starting `program`, and the icon, unless either is
/// there already and isn't clackatoo's, or `force`.
fn install(paths: &Paths, program: &Path, force: bool) -> anyhow::Result<Outcome> {
    let entry = entry(program)?;
    let theirs = theirs(paths)?;
    if !theirs.is_empty() && !force {
        return Ok(Outcome::Refused(theirs));
    }
    write(&paths.entry(), &entry)?;
    write(&paths.icon(), &icon())?;
    Ok(Outcome::Done(vec![paths.entry(), paths.icon()]))
}

/// Rewrites the entry, starting `program`, and the icon, if clackatoo
/// wrote the entry; says whether it did. An icon put in place of
/// clackatoo's since is left alone.
fn refresh(paths: &Paths, program: &Path) -> anyhow::Result<bool> {
    if !installed(paths) {
        return Ok(false);
    }
    write(&paths.entry(), &entry(program)?)?;
    if found(&paths.icon(), icon_is_ours)? != Found::Theirs {
        write(&paths.icon(), &icon())?;
    }
    Ok(true)
}

/// Removes the entry and the icon, unless either isn't clackatoo's, or
/// `force`.
fn remove(paths: &Paths, force: bool) -> anyhow::Result<Outcome> {
    let theirs = theirs(paths)?;
    if !theirs.is_empty() && !force {
        return Ok(Outcome::Refused(theirs));
    }
    let mut removed = Vec::new();
    for path in [paths.entry(), paths.icon()] {
        match fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(
                    anyhow::Error::new(err).context(format!("couldn't remove {}", path.display()))
                );
            }
        }
    }
    Ok(Outcome::Done(removed))
}

/// Whether the launcher's entry is one clackatoo wrote, which `update`
/// should refresh.
pub(crate) fn installed(paths: &Paths) -> bool {
    matches!(found(&paths.entry(), entry_is_ours), Ok(Found::Ours))
}

/// The entry and icon that are there but aren't clackatoo's.
fn theirs(paths: &Paths) -> anyhow::Result<Vec<PathBuf>> {
    let mut theirs = Vec::new();
    for (path, is_ours) in [
        (paths.entry(), entry_is_ours as fn(&str) -> bool),
        (paths.icon(), icon_is_ours),
    ] {
        if found(&path, is_ours)? == Found::Theirs {
            theirs.push(path);
        }
    }
    Ok(theirs)
}

/// What's at a path.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    Nothing,
    Ours,
    Theirs,
}

fn found(path: &Path, is_ours: fn(&str) -> bool) -> anyhow::Result<Found> {
    match fs::read(path) {
        Ok(bytes) if is_ours(&String::from_utf8_lossy(&bytes)) => Ok(Found::Ours),
        Ok(_) => Ok(Found::Theirs),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Found::Nothing),
        Err(err) => {
            Err(anyhow::Error::new(err).context(format!("couldn't read {}", path.display())))
        }
    }
}

/// An entry is clackatoo's if it's marked.
fn entry_is_ours(text: &str) -> bool {
    text.lines().any(|line| line.trim() == MANAGED)
}

/// An icon is clackatoo's if it's marked, or exactly the one shipped, as
/// when it was copied from the repository by hand: replacing that loses
/// nothing.
fn icon_is_ours(text: &str) -> bool {
    text == ICON || text.contains(ICON_MANAGED)
}

/// The desktop entry for the build at `program`: it starts that build, is
/// hidden if the build is gone (`TryExec`), and is marked as clackatoo's,
/// at the end of its main group.
fn entry(program: &Path) -> anyhow::Result<String> {
    let path = (program.to_str())
        .filter(|path| !path.contains(char::is_control))
        .with_context(|| {
            format!(
                "{} can't go in a desktop entry: it isn't UTF-8, or has control characters",
                program.display()
            )
        })?;
    let exec = escape_value(&exec_argument(path));
    let try_exec = escape_value(path);
    let mut text = String::with_capacity(ENTRY.len() + 2 * exec.len() + MANAGED.len());
    let mut in_main = false;
    for line in ENTRY.lines() {
        if line.starts_with('[') {
            if in_main {
                end_main(&mut text);
            }
            in_main = line == "[Desktop Entry]";
        }
        if line.starts_with("Exec=") {
            text.push_str("Exec=");
            text.push_str(&exec);
        } else if line.starts_with("TryExec=") {
            text.push_str("TryExec=");
            text.push_str(&try_exec);
        } else {
            text.push_str(line);
        }
        text.push('\n');
    }
    if in_main {
        end_main(&mut text);
    }
    Ok(text)
}

/// Adds the mark as the last key of the main group, before the blank line
/// that ends it.
fn end_main(text: &mut String) {
    let blank = text.ends_with("\n\n");
    if blank {
        text.pop();
    }
    text.push_str(MANAGED);
    text.push('\n');
    if blank {
        text.push('\n');
    }
}

/// `path` as the program in an `Exec` line: quoted if it has any
/// character the spec reserves, with `"`, `` ` ``, `$` and `\` escaped
/// inside the quotes, and `%` doubled, as it starts a field code.
fn exec_argument(path: &str) -> String {
    const RESERVED: &[char] = &[
        ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(',
        ')', '`',
    ];
    let quoted = if path.contains(RESERVED) {
        let mut quoted = String::with_capacity(path.len() + 4);
        quoted.push('"');
        for c in path.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted.push('"');
        quoted
    } else {
        path.to_owned()
    };
    quoted.replace('%', "%%")
}

/// `value` escaped as a desktop entry's string values are: a backslash
/// is written twice.
fn escape_value(value: &str) -> String {
    value.replace('\\', "\\\\")
}

/// The icon, marked as clackatoo's. The mark goes first, or after an XML
/// declaration, which nothing may come before.
fn icon() -> String {
    let split = if ICON.starts_with("<?xml") {
        ICON.find('\n').map_or(ICON.len(), |end| end + 1)
    } else {
        0
    };
    let (declaration, rest) = ICON.split_at(split);
    format!("{declaration}{ICON_MANAGED}\n{rest}")
}

/// Writes `text` to `path`, creating its directory: to a new file beside
/// it, renamed over it, so launchers watching the directory never read
/// half of it. The new file is made afresh, never opened through a link
/// someone left in its place.
fn write(path: &Path, text: &str) -> anyhow::Result<()> {
    let dir = path.parent().context("a file with no directory")?;
    fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".clackatoo-{}", std::process::id()));
    let staged = dir.join(name);
    // One left by a crash. A link is removed, not followed.
    let _ = fs::remove_file(&staged);
    let written = (OpenOptions::new().write(true).create_new(true))
        .open(&staged)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .and_then(|()| fs::rename(&staged, path));
    if written.is_err() {
        let _ = fs::remove_file(&staged);
    }
    written.with_context(|| format!("couldn't write {}", path.display()))
}

/// Tells the desktop about a change, with the tools that keep its caches,
/// if they're installed. Launchers that watch the directories, as GNOME's
/// does, see it anyway.
fn update_caches(paths: &Paths) {
    quietly("update-desktop-database", &[paths.applications.as_os_str()]);
    // An icon cache that's there hides icons added after it was made, so
    // it's brought up to date; none at all is fine, so none is made.
    if paths.hicolor.join("icon-theme.cache").exists() {
        quietly(
            "gtk-update-icon-cache",
            &[
                OsStr::new("--quiet"),
                OsStr::new("--force"),
                OsStr::new("--ignore-theme-index"),
                paths.hicolor.as_os_str(),
            ],
        );
    }
}

/// Runs `program`, ignoring what it says, whether it worked, and whether
/// it's there at all.
fn quietly(program: &str, args: &[&OsStr]) {
    let _ = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROGRAM: &str = "/home/someone/.local/bin/clackatoo";

    fn setup() -> (tempfile::TempDir, Paths) {
        let data = tempfile::tempdir().unwrap();
        let paths = Paths::new(data.path());
        (data, paths)
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    /// The values of `key` in every group of `entry`, in order.
    fn values<'a>(entry: &'a str, key: &str) -> Vec<&'a str> {
        let prefix = format!("{key}=");
        (entry.lines())
            .filter_map(|line| line.strip_prefix(prefix.as_str()))
            .collect()
    }

    /// A launcher entry someone wrote by hand.
    const HANDMADE: &str = "[Desktop Entry]\nType=Application\nName=clackatoo\nExec=env \
                            \"CLACKATOO_FONT=Mono\" /src/target/release/clackatoo\n";

    #[test]
    fn install_writes_an_entry_starting_the_program_and_the_icon() {
        let (data, paths) = setup();

        let outcome = install(&paths, Path::new(PROGRAM), false).unwrap();

        let entry_path = data.path().join("applications/clackatoo.desktop");
        let icon_path = data
            .path()
            .join("icons/hicolor/scalable/apps/clackatoo.svg");
        assert_eq!(
            outcome,
            Outcome::Done(vec![entry_path.clone(), icon_path.clone()])
        );
        let entry = read(&entry_path);
        // The main entry and the New Window action both start it.
        assert_eq!(values(&entry, "Exec"), [PROGRAM, PROGRAM]);
        assert_eq!(values(&entry, "TryExec"), [PROGRAM]);
        assert_eq!(values(&entry, "StartupWMClass"), ["clackatoo"]);
        assert_eq!(values(&entry, "Icon"), ["clackatoo"]);
        // Marked, in the main group.
        let action = entry.find("[Desktop Action").unwrap();
        assert!(
            entry[..action].contains(&format!("\n{MANAGED}\n\n")),
            "{entry}"
        );
        assert!(installed(&paths));
        let icon = read(&icon_path);
        assert!(icon.starts_with(ICON_MANAGED), "{icon}");
        assert!(icon.ends_with(ICON));
        // No staged files are left behind.
        let left: Vec<_> = fs::read_dir(data.path().join("applications"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["clackatoo.desktop"]);
    }

    #[test]
    fn install_replaces_what_clackatoo_wrote() {
        let (_data, paths) = setup();
        assert!(matches!(
            install(&paths, Path::new("/old/clackatoo"), false).unwrap(),
            Outcome::Done(_)
        ));

        let outcome = install(&paths, Path::new(PROGRAM), false).unwrap();

        assert!(matches!(outcome, Outcome::Done(_)));
        assert_eq!(values(&read(&paths.entry()), "TryExec"), [PROGRAM]);
    }

    #[test]
    fn install_leaves_an_entry_clackatoo_didnt_write_unless_forced() {
        let (_data, paths) = setup();
        write(&paths.entry(), HANDMADE).unwrap();

        let outcome = install(&paths, Path::new(PROGRAM), false).unwrap();

        assert_eq!(outcome, Outcome::Refused(vec![paths.entry()]));
        assert_eq!(read(&paths.entry()), HANDMADE);
        // Nothing was written, not even the icon.
        assert!(!paths.icon().exists());
        assert!(!installed(&paths));

        let forced = install(&paths, Path::new(PROGRAM), true).unwrap();

        assert!(matches!(forced, Outcome::Done(_)));
        assert_eq!(values(&read(&paths.entry()), "TryExec"), [PROGRAM]);
        assert!(installed(&paths));
    }

    #[test]
    fn install_leaves_an_icon_clackatoo_didnt_write_but_takes_a_copy_of_its_own() {
        let (_data, paths) = setup();
        write(&paths.icon(), "<svg>someone's own</svg>").unwrap();

        let outcome = install(&paths, Path::new(PROGRAM), false).unwrap();

        assert_eq!(outcome, Outcome::Refused(vec![paths.icon()]));
        assert!(!paths.entry().exists());

        // One copied from the repository by hand is clackatoo's.
        write(&paths.icon(), ICON).unwrap();
        let outcome = install(&paths, Path::new(PROGRAM), false).unwrap();
        assert!(matches!(outcome, Outcome::Done(_)));
        assert!(read(&paths.icon()).starts_with(ICON_MANAGED));
    }

    #[test]
    fn remove_takes_out_only_what_clackatoo_wrote() {
        let (_data, paths) = setup();
        assert_eq!(remove(&paths, false).unwrap(), Outcome::Done(vec![]));
        assert!(matches!(
            install(&paths, Path::new(PROGRAM), false).unwrap(),
            Outcome::Done(_)
        ));

        let outcome = remove(&paths, false).unwrap();

        assert_eq!(outcome, Outcome::Done(vec![paths.entry(), paths.icon()]));
        assert!(!paths.entry().exists());
        assert!(!paths.icon().exists());

        // A handmade entry keeps the icon it shows, too.
        write(&paths.entry(), HANDMADE).unwrap();
        write(&paths.icon(), ICON).unwrap();
        let outcome = remove(&paths, false).unwrap();
        assert_eq!(outcome, Outcome::Refused(vec![paths.entry()]));
        assert_eq!(read(&paths.entry()), HANDMADE);
        assert!(paths.icon().exists());

        let forced = remove(&paths, true).unwrap();
        assert_eq!(forced, Outcome::Done(vec![paths.entry(), paths.icon()]));
    }

    #[test]
    fn refresh_rewrites_only_an_entry_clackatoo_wrote() {
        let (_data, paths) = setup();
        // Nothing there: nothing written.
        assert!(!refresh(&paths, Path::new(PROGRAM)).unwrap());
        assert!(!paths.entry().exists());
        assert!(!paths.icon().exists());

        // An older build's entry, at another path, with an older icon.
        assert!(matches!(
            install(&paths, Path::new("/old/clackatoo"), false).unwrap(),
            Outcome::Done(_)
        ));
        write(&paths.icon(), &format!("{ICON_MANAGED}\n<svg>old</svg>")).unwrap();

        assert!(refresh(&paths, Path::new(PROGRAM)).unwrap());

        assert_eq!(values(&read(&paths.entry()), "Exec"), [PROGRAM, PROGRAM]);
        assert_eq!(read(&paths.icon()), icon());

        // An icon put in place of clackatoo's since stays.
        write(&paths.icon(), "<svg>mine</svg>").unwrap();
        assert!(refresh(&paths, Path::new(PROGRAM)).unwrap());
        assert_eq!(read(&paths.icon()), "<svg>mine</svg>");

        // A handmade entry, and its icon, stay as they are.
        write(&paths.entry(), HANDMADE).unwrap();
        write(&paths.icon(), ICON).unwrap();
        assert!(!refresh(&paths, Path::new(PROGRAM)).unwrap());
        assert_eq!(read(&paths.entry()), HANDMADE);
        assert_eq!(read(&paths.icon()), ICON);
    }

    #[test]
    fn writing_never_follows_a_link_left_where_it_stages() {
        let (data, paths) = setup();
        let elsewhere = data.path().join("elsewhere");
        fs::write(&elsewhere, "untouched").unwrap();
        let applications = data.path().join("applications");
        fs::create_dir_all(&applications).unwrap();
        let staged = applications.join(format!(
            ".clackatoo.desktop.clackatoo-{}",
            std::process::id()
        ));
        std::os::unix::fs::symlink(&elsewhere, &staged).unwrap();

        assert!(matches!(
            install(&paths, Path::new(PROGRAM), false).unwrap(),
            Outcome::Done(_)
        ));

        assert_eq!(read(&elsewhere), "untouched");
        assert!(installed(&paths));
        assert!(fs::symlink_metadata(&staged).is_err());
    }

    #[test]
    fn exec_quotes_and_escapes_paths_the_spec_reserves_characters_in() {
        assert_eq!(exec_argument("/usr/bin/clackatoo"), "/usr/bin/clackatoo");
        assert_eq!(
            exec_argument("/home/a b/clackatoo"),
            "\"/home/a b/clackatoo\""
        );
        assert_eq!(exec_argument("/opt/100%/clackatoo"), "/opt/100%%/clackatoo");
        assert_eq!(
            exec_argument(r#"/x/$"`\%/clackatoo"#),
            r#""/x/\$\"\`\\%%/clackatoo""#
        );
        // Then the key file's own escaping doubles every backslash.
        let written = entry(Path::new(r"/x/a\b c/clackatoo")).unwrap();
        assert_eq!(values(&written, "Exec")[0], r#""/x/a\\\\b c/clackatoo""#);
        assert_eq!(values(&written, "TryExec"), [r"/x/a\\b c/clackatoo"]);
        assert!(entry(Path::new("/x/a\nb/clackatoo")).is_err());
    }

    #[test]
    fn the_shipped_entry_is_one_install_can_fill_in() {
        // Every line `entry` rewrites is there, once in each group.
        assert_eq!(values(ENTRY, "Exec"), ["clackatoo", "clackatoo"]);
        assert_eq!(values(ENTRY, "TryExec"), ["clackatoo"]);
        assert!(!entry_is_ours(ENTRY));
        assert!(ENTRY.starts_with("[Desktop Entry]\n"));
    }
}
