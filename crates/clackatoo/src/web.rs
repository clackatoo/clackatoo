//! `clackatoo web` and `clackatoo devices`: the daemon's web app, which
//! phones reach it through. `web password` keeps the password's hash in
//! `daemon/web.toml`; `devices` lists the devices paired with it, and
//! answers those asking to pair, or unpairs one.

use std::io::{self, BufRead as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clackatoo_protocol::{Request, Response};
use clap::{Args, Subcommand};
use toml_edit::DocumentMut;

use crate::Connection;

/// The fewest characters a password may have.
const MIN_PASSWORD: usize = 8;

/// What `clackatoo web` does.
#[derive(Debug, Subcommand)]
pub(crate) enum WebCommand {
    /// Set the password devices pair with, asked for twice, or read from
    /// standard input if it isn't a terminal. Only its hash is kept, in
    /// daemon/web.toml; the daemon reads it when it next starts or
    /// upgrades.
    Password {
        /// Take the password away instead: devices then pair only by
        /// being allowed from a window.
        #[arg(long)]
        unset: bool,
    },
}

/// What `clackatoo devices` does.
#[derive(Debug, Args)]
pub(crate) struct DevicesArgs {
    #[command(subcommand)]
    what: Option<DevicesCommand>,
    #[command(flatten)]
    connection: Connection,
}

#[derive(Debug, Subcommand)]
enum DevicesCommand {
    /// Unpair a device, cutting it off if it's connected.
    Revoke {
        /// Its number, as `clackatoo devices` lists it.
        device: u64,
    },
    /// Allow the device asking to pair that shows this code.
    Allow { code: String },
    /// Turn down every device asking to pair.
    Deny,
    /// Let devices pair with the password again, after too many wrong ones
    /// locked it.
    Unlock,
}

/// Runs `clackatoo web`.
pub(crate) fn run(command: &WebCommand) -> anyhow::Result<()> {
    let WebCommand::Password { unset } = command;
    let path = settings().context("no config directory: set $HOME or $XDG_CONFIG_HOME")?;
    let mut document = read(&path)?;
    if *unset {
        document.remove("password");
        write(&path, &document.to_string())?;
        println!("The password is gone from {}.", path.display());
    } else {
        let password = read_password()?;
        let hash = clackatoo_daemon::web::hash_password(&password);
        document["password"] = toml_edit::value(hash);
        write(&path, &document.to_string())?;
        println!("The password's hash is in {}.", path.display());
    }
    println!("The daemon reads it when it next starts: `clackatoo upgrade` restarts it in place.");
    Ok(())
}

/// Runs `clackatoo devices`.
pub(crate) fn devices(args: &DevicesArgs) -> anyhow::Result<()> {
    let mut client = crate::connect(&args.connection)?;
    let request = match &args.what {
        None => Request::Devices,
        Some(DevicesCommand::Revoke { device }) => Request::RevokeDevice { device: *device },
        Some(DevicesCommand::Allow { code }) => Request::AnswerPairing {
            code: Some(code.clone()),
        },
        Some(DevicesCommand::Deny) => Request::AnswerPairing { code: None },
        Some(DevicesCommand::Unlock) => Request::UnlockPairing,
    };
    match client.request(&request)? {
        Response::Devices(devices) if devices.is_empty() => {
            println!("No devices are paired.");
        }
        Response::Devices(devices) => {
            println!("ID  NAME                 LAST USED             NOTIFIED  LOGIN");
            for device in devices {
                let used = if device.connected {
                    "connected now".to_owned()
                } else {
                    ago(device.used)
                };
                println!(
                    "{:<3} {:<20} {:<21} {:<9} {}",
                    device.id,
                    device.name,
                    used,
                    if device.notifications { "yes" } else { "no" },
                    device.login.as_deref().unwrap_or("-"),
                );
            }
        }
        Response::Text(said) => println!("{said}"),
        Response::Done => println!("Unpaired."),
        Response::Error(err) => anyhow::bail!("{err}"),
        other => anyhow::bail!("the daemon answered {other:?}"),
    }
    Ok(())
}

/// How long ago `millis` since the Unix epoch was, roughly: "3 days ago".
fn ago(millis: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        });
    let minutes = now.saturating_sub(millis) / 60_000;
    let plural = |n: u64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match minutes {
        0 => "just now".to_owned(),
        1..60 => plural(minutes, "minute"),
        60..1440 => plural(minutes / 60, "hour"),
        _ => plural(minutes / 1440, "day"),
    }
}

/// Where the web app's settings are: `daemon/web.toml` in clackatoo's
/// config directory.
fn settings() -> Option<PathBuf> {
    Some(clackatoo_daemon::config_dir()?.join("daemon/web.toml"))
}

/// The settings at `path`, or none if there are none yet.
fn read(path: &Path) -> anyhow::Result<DocumentMut> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .parse()
            .with_context(|| format!("{} isn't TOML", path.display())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(err) => Err(err).with_context(|| format!("couldn't read {}", path.display())),
    }
}

/// Writes `text` as the settings at `path`, which only the user may read,
/// beside the old and renamed over them.
fn write(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    }
    let partial = path.with_extension("toml.partial");
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&partial)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            // On disk before it takes the old one's place.
            file.sync_all()
        })
        .with_context(|| format!("couldn't write {}", partial.display()))?;
    std::fs::rename(&partial, path)
        .with_context(|| format!("couldn't write {}", path.display()))?;
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)
            .and_then(|dir| dir.sync_all())
            .with_context(|| format!("couldn't write {}", path.display()))?;
    }
    Ok(())
}

/// The password: typed twice, without being shown, on a terminal, or else
/// the first line of standard input.
fn read_password() -> anyhow::Result<String> {
    let stdin = io::stdin();
    if !rustix::termios::isatty(&stdin) {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        let password = line.trim_end_matches(['\r', '\n']).to_owned();
        anyhow::ensure!(
            password.chars().count() >= MIN_PASSWORD,
            "a password needs at least {MIN_PASSWORD} characters"
        );
        return Ok(password);
    }
    let first = prompt("Password for devices: ")?;
    anyhow::ensure!(
        first.chars().count() >= MIN_PASSWORD,
        "a password needs at least {MIN_PASSWORD} characters"
    );
    let again = prompt("Again: ")?;
    anyhow::ensure!(first == again, "the two didn't match");
    Ok(first)
}

/// Asks `question` on the terminal, and reads the answer without showing
/// it.
fn prompt(question: &str) -> anyhow::Result<String> {
    let stdin = io::stdin();
    eprint!("{question}");
    io::stderr().flush()?;
    let before = rustix::termios::tcgetattr(&stdin)?;
    let mut quiet = before.clone();
    quiet.local_modes.remove(rustix::termios::LocalModes::ECHO);
    rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &quiet)?;
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &before)?;
    eprintln!();
    read?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}
