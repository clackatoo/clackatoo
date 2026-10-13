//! Keeping a repository's own filter drivers from running.
//!
//! A filter driver is a command git runs on a file's way into the index
//! (`clean`, or a long-running `process`) and on its way out (`smudge`),
//! for files whose attributes name it: `filter=lfs` in `.gitattributes`
//! and `filter.lfs.clean` in config. `git status` runs `clean` on files
//! that look changed, `git diff` against a commit does too, and `git
//! cat-file --filters` runs `smudge`. A folder copied from anyone can
//! bring both halves, its own `.git/config` and `.gitattributes`, so
//! opening it in an explorer, or a file of it in an editor, would run
//! whatever command it names.
//!
//! So each git command that can filter is run with every driver the
//! repository's own config defines turned off, unless it's one known to
//! be safe: Git LFS's, exactly as `git lfs install` writes it, while the
//! repository gives Git LFS no commands of its own to run. Drivers
//! from the user's own config, system-wide or global, are the user's
//! choice and are left alone. `$CLACKATOO_GITFILTERS` changes this, read
//! once, when the daemon first needs it: `lfs` (the default), `all` to
//! let every driver run, or `none` to turn off every driver, the user's
//! own and Git LFS's too.
//!
//! git has no setting that turns off every driver, so each is turned off
//! by name, read from `git config --list` each time: it costs a few
//! milliseconds on a thread that's about to run git anyway, and a copy
//! kept between runs could miss a change git would see, such as an
//! `includeIf "onbranch:…"` coming true when the branch changes. The
//! settings go in `GIT_CONFIG_KEY_n` and `GIT_CONFIG_VALUE_n`, not `-c`,
//! which splits at the first `=` and so can't name a driver called
//! `a=b`.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::process::Command;
use std::sync::OnceLock;

/// Which filter drivers git may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Allowed {
    /// The user's own, and Git LFS's from anywhere.
    Lfs,
    /// Every one, wherever it's defined.
    All,
    /// None at all.
    None,
}

impl Allowed {
    /// What `$CLACKATOO_GITFILTERS` says, read the first time it's asked.
    pub(crate) fn chosen() -> Self {
        static CHOSEN: OnceLock<Allowed> = OnceLock::new();
        *CHOSEN.get_or_init(|| {
            let said = std::env::var("CLACKATOO_GITFILTERS").unwrap_or_default();
            Self::parse(&said).unwrap_or_else(|| {
                log::warn!("CLACKATOO_GITFILTERS should be lfs, all or none, not {said:?}: lfs");
                Self::Lfs
            })
        })
    }

    /// `said`, as `$CLACKATOO_GITFILTERS` gives it: empty means the
    /// default.
    fn parse(said: &str) -> Option<Self> {
        match said.trim() {
            "" | "lfs" => Some(Self::Lfs),
            "all" => Some(Self::All),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// Git LFS's commands, as `git lfs install` writes them. Nothing else is
/// let through, not even these with other arguments.
const LFS: [(&[u8], &[u8]); 3] = [
    (b"clean", b"git-lfs clean -- %f"),
    (b"smudge", b"git-lfs smudge -- %f"),
    (b"process", b"git-lfs filter-process"),
];

/// The settings that turn off each driver `config` defines that `allowed`
/// doesn't let run, each a key and its value. `config` is what `git
/// config --list -z --show-scope` prints.
///
/// A driver is turned off whole if any of its commands from the
/// repository isn't let through: git runs `process` if it's set and
/// `clean` or `smudge` if not, so one good command beside a bad one
/// proves nothing. Git LFS's are let through only while the repository
/// adds no LFS extensions, `lfs.extension.*`, which name commands
/// `git-lfs clean` runs: where it does, every driver that runs Git LFS
/// is turned off, the user's own too.
pub(crate) fn turned_off(config: &[u8], allowed: Allowed) -> Vec<(OsString, &'static str)> {
    /// What config says of a driver.
    #[derive(Default)]
    struct Driver {
        /// One of the repository's commands for it isn't let through.
        doubted: bool,
        /// One of its commands, from anywhere, runs Git LFS.
        lfs: bool,
    }
    let mut drivers: BTreeMap<&[u8], Driver> = BTreeMap::new();
    let mut extended = false;
    // Each entry is "SCOPE\0KEY\nVALUE\0", or "SCOPE\0KEY\0" for a key
    // given without a value.
    let mut fields = config.split(|&byte| byte == 0);
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        let (key, value) = match entry.iter().position(|&byte| byte == b'\n') {
            Some(end) => (&entry[..end], Some(&entry[end + 1..])),
            None => (entry, None),
        };
        // The user's own, or what we were started with. The repository's
        // are `local` and `worktree`, with what they include, and
        // anything else is doubted as theirs.
        let users = matches!(scope, b"system" | b"global" | b"command");
        if !users && key.starts_with(b"lfs.extension.") {
            extended = true;
        }
        // The name is everything between `filter.` and the last dot.
        let Some((name, variable)) = key.strip_prefix(b"filter.").and_then(|rest| {
            let dot = rest.iter().rposition(|&byte| byte == b'.')?;
            Some((&rest[..dot], &rest[dot + 1..]))
        }) else {
            continue;
        };
        let driver = drivers.entry(name).or_default();
        if !LFS.iter().any(|(command, _)| *command == variable) {
            continue;
        }
        let value = value.unwrap_or_default();
        driver.lfs |= value.windows(7).any(|word| word == b"git-lfs");
        driver.doubted |= !users && !LFS.contains(&(variable, value));
    }
    let mut settings = Vec::new();
    for (name, driver) in drivers {
        let off = match allowed {
            Allowed::All => false,
            Allowed::None => true,
            Allowed::Lfs => driver.doubted || (extended && driver.lfs),
        };
        if !off {
            continue;
        }
        let key = |variable: &str| {
            let mut key = OsString::from("filter.");
            key.push(OsStr::from_bytes(name));
            key.push(".");
            key.push(variable);
            key
        };
        settings.push((key("clean"), ""));
        settings.push((key("smudge"), ""));
        settings.push((key("process"), ""));
        // A required driver with no command fails every file.
        settings.push((key("required"), "false"));
    }
    settings
}

/// Gives `command` the `settings`, each a key and its value, after any
/// it has from the daemon's own environment.
pub(crate) fn set(command: &mut Command, settings: &[(OsString, &str)]) {
    if settings.is_empty() {
        return;
    }
    let first = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|count| count.trim().parse::<usize>().ok())
        .unwrap_or(0);
    for (at, (key, value)) in settings.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{}", first + at), key)
            .env(format!("GIT_CONFIG_VALUE_{}", first + at), value);
    }
    command.env("GIT_CONFIG_COUNT", (first + settings.len()).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Config as `git config --list -z --show-scope` prints it, from
    /// `(scope, key, value)`s.
    fn config(entries: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut printed = Vec::new();
        for (scope, key, value) in entries {
            printed.extend_from_slice(format!("{scope}\0{key}\n{value}\0").as_bytes());
        }
        printed
    }

    /// The drivers `config` has turned off.
    fn off(config: &[u8], allowed: Allowed) -> Vec<String> {
        let settings = turned_off(config, allowed);
        let mut names: Vec<String> = (settings.iter())
            .filter_map(|(key, _)| {
                let key = key.to_str()?;
                Some(
                    key.strip_prefix("filter.")?
                        .strip_suffix(".clean")?
                        .to_owned(),
                )
            })
            .collect();
        names.dedup();
        // Every driver turned off is turned off whole.
        assert_eq!(settings.len(), names.len() * 4, "{settings:?}");
        names
    }

    const INSTALLED: [(&str, &str, &str); 4] = [
        ("local", "filter.lfs.clean", "git-lfs clean -- %f"),
        ("local", "filter.lfs.smudge", "git-lfs smudge -- %f"),
        ("local", "filter.lfs.process", "git-lfs filter-process"),
        ("local", "filter.lfs.required", "true"),
    ];

    #[test]
    fn a_repositorys_own_drivers_are_turned_off_whole() {
        let printed = config(&[
            ("global", "user.name", "t"),
            ("local", "filter.evil.clean", "touch PWNED"),
            ("local", "filter.evil.required", "true"),
            ("worktree", "filter.odd.smudge", "cat"),
        ]);
        assert_eq!(off(&printed, Allowed::Lfs), ["evil", "odd"]);
        let settings = turned_off(&printed, Allowed::Lfs);
        let evil: Vec<(&str, &str)> = (settings.iter().take(4))
            .map(|(key, value)| (key.to_str().unwrap(), *value))
            .collect();
        assert_eq!(
            evil,
            [
                ("filter.evil.clean", ""),
                ("filter.evil.smudge", ""),
                ("filter.evil.process", ""),
                ("filter.evil.required", "false"),
            ]
        );
    }

    #[test]
    fn git_lfs_is_let_through_only_exactly_as_it_installs_itself() {
        assert!(off(&config(&INSTALLED), Allowed::Lfs).is_empty());
        // A good process beside a bad clean: git would run the process,
        // but a driver isn't half trusted.
        let mut mixed = INSTALLED.to_vec();
        mixed[0] = ("local", "filter.lfs.clean", "touch PWNED");
        assert_eq!(off(&config(&mixed), Allowed::Lfs), ["lfs"]);
        // Its commands with more to them.
        let longer = [(
            "local",
            "filter.lfs.process",
            "git-lfs filter-process; touch PWNED",
        )];
        assert_eq!(off(&config(&longer), Allowed::Lfs), ["lfs"]);
        // Its commands under another name are as good.
        let renamed = [("local", "filter.big.process", "git-lfs filter-process")];
        assert!(off(&config(&renamed), Allowed::Lfs).is_empty());
        // A key with no value is no command git would run, nor one let
        // through.
        let mut bare = config(&INSTALLED);
        bare.extend_from_slice(b"local\0filter.lfs.smudge\0");
        assert_eq!(off(&bare, Allowed::Lfs), ["lfs"]);
    }

    #[test]
    fn git_lfs_is_turned_off_where_the_repository_extends_it() {
        let mut extended = INSTALLED.to_vec();
        extended.push(("local", "lfs.extension.foo.clean", "touch PWNED"));
        assert_eq!(off(&config(&extended), Allowed::Lfs), ["lfs"]);
        // Git LFS reads them wherever its driver comes from, the user's
        // own config, or a wrapper of theirs; other drivers are left be.
        let theirs = config(&[
            ("global", "filter.lfs.process", "git-lfs filter-process"),
            ("global", "filter.wrapped.clean", "wrap git-lfs clean -- %f"),
            ("global", "filter.nb.clean", "nbstripout"),
            ("local", "lfs.extension.foo.clean", "touch PWNED"),
        ]);
        assert_eq!(off(&theirs, Allowed::Lfs), ["lfs", "wrapped"]);
        // The user's own extensions are theirs.
        let mut users = INSTALLED.to_vec();
        users.push(("global", "lfs.extension.foo.clean", "foo %f"));
        assert!(off(&config(&users), Allowed::Lfs).is_empty());
    }

    #[test]
    fn the_users_own_drivers_are_left_alone_unless_none_are_allowed() {
        let printed = config(&[
            ("system", "filter.crypt.clean", "git-crypt clean"),
            ("global", "filter.nb.clean", "nbstripout"),
            ("command", "filter.given.smudge", "cat"),
        ]);
        assert!(off(&printed, Allowed::Lfs).is_empty());
        assert!(off(&printed, Allowed::All).is_empty());
        assert_eq!(off(&printed, Allowed::None), ["crypt", "given", "nb"]);
        // But a repository changing one of them makes it theirs.
        let mut changed = printed.clone();
        changed.extend(config(&[("local", "filter.nb.clean", "touch PWNED")]));
        assert_eq!(off(&changed, Allowed::Lfs), ["nb"]);
        // A scope git may add one day is doubted.
        let unknown = config(&[("elsewhere", "filter.x.clean", "touch PWNED")]);
        assert_eq!(off(&unknown, Allowed::Lfs), ["x"]);
    }

    #[test]
    fn every_driver_is_turned_off_where_none_are_allowed() {
        assert_eq!(off(&config(&INSTALLED), Allowed::None), ["lfs"]);
        let evil = config(&[("local", "filter.evil.clean", "touch PWNED")]);
        assert!(off(&evil, Allowed::All).is_empty());
    }

    #[test]
    fn a_driver_is_named_by_everything_up_to_the_last_dot() {
        let printed = config(&[
            ("local", "filter.a.b.clean", "x"),
            ("local", "filter.a=b.smudge", "x"),
            ("local", "filter.Spaced Out.process", "x"),
        ]);
        assert_eq!(off(&printed, Allowed::Lfs), ["Spaced Out", "a.b", "a=b"]);
        // A name that isn't UTF-8 is named as it is.
        let mut odd = b"local\0filter.\xff.clean\nx\0".to_vec();
        odd.extend_from_slice(b"local\0core.bare\nfalse\0");
        let settings = turned_off(&odd, Allowed::Lfs);
        assert_eq!(settings[0].0.as_bytes(), b"filter.\xff.clean");
    }

    #[test]
    fn the_choice_is_lfs_unless_it_says_otherwise() {
        assert_eq!(Allowed::parse(""), Some(Allowed::Lfs));
        assert_eq!(Allowed::parse("lfs"), Some(Allowed::Lfs));
        assert_eq!(Allowed::parse(" all\n"), Some(Allowed::All));
        assert_eq!(Allowed::parse("none"), Some(Allowed::None));
        assert_eq!(Allowed::parse("some"), None);
    }
}
