//! `clackatoo update`: installs the latest release in place of this build,
//! then moves the daemon onto it.

use std::path::Path;
use std::process::Command;

use anyhow::Context;
use clackatoo_daemon::{RuntimeDir, launch};
use clackatoo_update::{Channel, Source, Version, install, verify};
use clap::{Args, ValueEnum};

#[derive(Debug, Args)]
pub(crate) struct UpdateArgs {
    /// Only say whether there's a newer release.
    #[arg(long, conflicts_with = "rollback")]
    check: bool,
    /// Which releases to follow: `stable`, or `nightly`, which isn't
    /// published yet.
    #[arg(
        long,
        value_name = "CHANNEL",
        default_value = "stable",
        conflicts_with = "rollback"
    )]
    channel: ChannelArg,
    /// Install this version, newer or older, instead of the latest.
    #[arg(long, value_name = "VERSION", conflicts_with = "rollback")]
    to: Option<Version>,
    /// Go back to the build the last update replaced. Run again, it goes
    /// forward again.
    #[arg(long)]
    rollback: bool,
}

/// [`Channel`], as the command line spells it.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum ChannelArg {
    Stable,
    Nightly,
}

impl From<ChannelArg> for Channel {
    fn from(channel: ChannelArg) -> Self {
        match channel {
            ChannelArg::Stable => Self::Stable,
            ChannelArg::Nightly => Self::Nightly,
        }
    }
}

pub(crate) fn run(args: &UpdateArgs) -> anyhow::Result<()> {
    let running = std::env::current_exe().context("this program can't be found")?;
    let target = install::installed(&running);
    if args.rollback {
        install::rollback(&target)?;
        println!(
            "{} is back to {}",
            target.display(),
            says_version(&target).unwrap_or_else(|| "the build before".into())
        );
        return upgrade_daemon(&target);
    }
    let source = Source::from_env();
    let current = Version::current();
    let release = match args.to {
        Some(version) => source.version(version)?,
        None => source.latest(args.channel.into())?,
    };
    let version = release.version;
    if args.check {
        if version > current {
            println!(
                "clackatoo {version} is out, and this is {current}: `clackatoo update` installs it"
            );
        } else {
            println!("clackatoo {current} is up to date");
        }
        return Ok(());
    }
    match args.to {
        Some(_) if version == current => {
            println!("this is clackatoo {current} already");
            return Ok(());
        }
        None if version <= current => {
            println!("clackatoo {current} is up to date");
            return Ok(());
        }
        _ => {}
    }
    let key = verify::release_key()?;
    println!("downloading clackatoo {version}…");
    let program = verify::download(&release, &key)?;
    install::install(&target, &program, version)?;
    println!(
        "installed clackatoo {version} at {}, keeping {current} as {} for `clackatoo update \
         --rollback`",
        target.display(),
        install::previous(&target).display()
    );
    if source == Source::default() {
        println!(
            "what's new: https://github.com/clackatoo/clackatoo/releases/tag/{}",
            version.tag()
        );
    }
    upgrade_daemon(&target)
}

/// Moves the running daemon, if there is one, onto the build at `program`,
/// by running its own `upgrade`: this process's program is the old one.
fn upgrade_daemon(program: &Path) -> anyhow::Result<()> {
    match launch::daemon_protocol(&RuntimeDir::for_current_user()) {
        Ok(None) => {
            println!("no daemon is running, so the next one starts on the new build");
            Ok(())
        }
        Ok(Some(_)) => {
            let status = Command::new(program)
                .arg("upgrade")
                .status()
                .with_context(|| format!("couldn't run {}", program.display()))?;
            anyhow::ensure!(
                status.success(),
                "{} is installed, but the daemon is still on the build it was on",
                program.display()
            );
            println!("windows open before keep the old build until they're opened again");
            Ok(())
        }
        Err(err) => Err(anyhow::Error::new(err).context(format!(
            "{} is installed, but the daemon can't be reached to upgrade it",
            program.display()
        ))),
    }
}

/// The version the program at `path` says it is, from `--version`.
fn says_version(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
    let said = String::from_utf8_lossy(&output.stdout);
    let version = said.split_whitespace().nth(1)?;
    Some(version.to_owned())
}
