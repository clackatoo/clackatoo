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
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// What a window logs unless `RUST_LOG` says otherwise: warnings, and how
/// a connection to another machine is going, which is what's needed to
/// find out why one dropped. Quiet while it goes well: a few lines an
/// hour.
const WINDOW: &str = "warn,clackatoo_daemon::quic=info,clackatoo_daemon::remote=info,\
                      clackatoo_gui::session=info";

/// How big a window's log may grow before it's moved aside, to
/// `window.log.old`, and another begun.
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
    let log = window_log_path()
        .ok_or_else(|| io::Error::other("$HOME isn't set"))
        .and_then(|path| Both::open(path, WINDOW_LOG_LIMIT));
    match log {
        Ok(both) => {
            builder.target(env_logger::Target::Pipe(Box::new(both)));
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

/// Writes each line both to the error output and to a log file. Each
/// record comes in one write, and the file is opened to append, so lines
/// from several windows don't interleave. Once the file grows past its
/// limit, it's moved aside, to `.old` in place of the last, and another
/// begun; a window whose file another moved aside follows it to the new
/// one.
struct Both {
    file: File,
    path: PathBuf,
    limit: u64,
}

impl Both {
    fn open(path: PathBuf, limit: u64) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = append(&path)?;
        let mut both = Self { file, path, limit };
        both.move_aside_if_full();
        Ok(both)
    }

    /// Moves the file aside if it has grown past the limit, and begins
    /// another, or follows another window that has.
    fn move_aside_if_full(&mut self) {
        let ours = self.file.metadata().ok().map(|meta| meta.ino());
        let reopen = match std::fs::metadata(&self.path) {
            Ok(meta) if meta.len() > self.limit => {
                let _ = std::fs::rename(&self.path, self.path.with_extension("log.old"));
                true
            }
            Ok(meta) => Some(meta.ino()) != ours,
            Err(_) => true,
        };
        if reopen && let Ok(file) = append(&self.path) {
            self.file = file;
        }
    }
}

/// Opens `path` to add to, making it if it isn't there.
fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

impl Write for Both {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // A window started without a terminal may have nowhere for errors.
        let _ = io::stderr().write_all(bytes);
        self.move_aside_if_full();
        self.file.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_past_its_limit_is_moved_aside_while_written_and_others_follow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.log");
        let mut first = Both::open(path.clone(), 100).unwrap();
        let mut second = Both::open(path.clone(), 100).unwrap();
        first.write_all(&[b'a'; 60]).unwrap();
        second.write_all(&[b'b'; 60]).unwrap();
        // Past 100: moved aside before the next line, by whichever writes.
        first.write_all(b"after").unwrap();
        let old = std::fs::read(path.with_extension("log.old")).unwrap();
        assert_eq!(old.len(), 120);
        second.write_all(b"too").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "aftertoo");
    }
}
