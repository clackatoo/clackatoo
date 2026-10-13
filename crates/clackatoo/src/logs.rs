//! Where each part of clackatoo logs, and how lines are stamped.
//!
//! Every line carries the time, to the millisecond, in UTC, so a window's
//! log on one machine can be laid beside the daemon's on another. The
//! daemon writes to `daemon.log` in the runtime directory, where it was
//! started with its output. A window also writes to `window.log` in
//! clackatoo's state directory, so that when a connection to another
//! machine drops, what happened is still there to read afterwards.
//! `RUST_LOG` overrides what's logged, as usual.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// What a window logs unless `RUST_LOG` says otherwise: warnings, and how
/// a connection to another machine is going, which is what's needed to
/// find out why one dropped. Quiet while it goes well: a few lines an
/// hour.
const WINDOW: &str = "warn,clackatoo_daemon::quic=info,clackatoo_daemon::remote=info,\
                      clackatoo_gui::session=info";

/// How big a window's log may grow before the next window to start moves
/// it aside, to `window.log.old`, and starts another.
const WINDOW_LOG_LIMIT: u64 = 8 * 1024 * 1024;

/// A logger with `default` as its filter unless `RUST_LOG` says otherwise.
fn builder(default: &str) -> env_logger::Builder {
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default));
    builder.format_timestamp_millis();
    builder
}

/// Logs for the daemon, to its output.
pub(crate) fn daemon() {
    builder("info").init();
}

/// Logs for a command run from the shell, as `default` says, to its
/// error output.
pub(crate) fn command(default: &str) {
    builder(default).init();
}

/// Logs for a window: to its error output, as always, and to `window.log`
/// in the state directory, each line with the window's process ID, since
/// several windows share the file.
pub(crate) fn window() {
    let mut builder = builder(WINDOW);
    let pid = std::process::id();
    builder.format(move |out, record| {
        writeln!(
            out,
            "[{} {:<5} {pid} {}] {}",
            out.timestamp_millis(),
            record.level(),
            record.target(),
            record.args()
        )
    });
    match open_window_log() {
        Ok(file) => {
            builder.target(env_logger::Target::Pipe(Box::new(Both(file))));
        }
        Err(err) => eprintln!("clackatoo: the window's log can't be written: {err}"),
    }
    builder.init();
}

/// Where windows log: `window.log` in clackatoo's state directory.
#[must_use]
pub(crate) fn window_log_path() -> Option<PathBuf> {
    Some(clackatoo_daemon::state_dir()?.join("window.log"))
}

/// Opens the window's log to add to, moving it aside first if it has grown
/// too big.
fn open_window_log() -> io::Result<File> {
    let path = window_log_path().ok_or_else(|| io::Error::other("$HOME isn't set"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > WINDOW_LOG_LIMIT) {
        std::fs::rename(&path, path.with_extension("log.old"))?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
}

/// Writes each line both to the error output and to a file. Each record
/// comes in one write, and the file is opened to append, so lines from
/// several windows don't interleave.
struct Both(File);

impl Write for Both {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // A window started without a terminal may have nowhere for errors.
        let _ = io::stderr().write_all(bytes);
        self.0.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
