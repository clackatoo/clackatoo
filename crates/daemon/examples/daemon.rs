//! The daemon, in a process of its own, as `clackatoo daemon` runs it, for
//! tests that signal it or upgrade it: set up as the environment says
//! (see `DaemonConfig::from_env`), for the runtime directory in
//! `$XDG_RUNTIME_DIR`, logging to stderr. Run with `--handover-versions`,
//! it says which handovers it reads, and that it takes signals held back
//! for it, as an upgrade asks first. With `--fresh`, it makes none of the
//! workspaces kept from its last run again, as `clackatoo daemon --fresh`.
//!
//! Run as a program whose name ends `-old`, it acts as a build from before
//! daemons caught signals: it doesn't say it takes them, and catches none.

use std::ffi::OsString;
use std::process::ExitCode;

use clackatoo_daemon::upgrade::{self, RESUME_FLAG, VERSIONS_FLAG};
use clackatoo_daemon::{DaemonConfig, RuntimeDir};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let old = (std::env::current_exe().ok())
        .is_some_and(|program| program.to_string_lossy().ends_with("-old"));
    if args.iter().any(|arg| arg == VERSIONS_FLAG) {
        if old {
            let versions: Vec<String> = (upgrade::readable_versions().iter())
                .map(u32::to_string)
                .collect();
            println!("{}", versions.join(" "));
        } else {
            println!("{}", upgrade::versions_answer());
        }
        return ExitCode::SUCCESS;
    }
    if log::set_logger(&Stderr).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    let resume = args.iter().any(|arg| arg == RESUME_FLAG);
    let restore = !args.iter().any(|arg| arg == "--fresh");
    // An upgrade runs the next build with the same arguments, but for
    // `--fresh`, which was for this start alone.
    let rerun: Vec<OsString> = (args.into_iter())
        .filter(|arg| arg != RESUME_FLAG && arg != "--fresh")
        .collect();
    let config = DaemonConfig::from_env();
    let runtime = RuntimeDir::for_current_user();
    let served = if old {
        upgrade::serve_catching_no_signals(&runtime, &config, resume, restore, &rerun)
    } else {
        upgrade::serve(&runtime, &config, resume, restore, &rerun)
    };
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("daemon: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Writes the daemon's log to stderr, a line each.
struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}
