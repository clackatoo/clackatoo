//! The daemon, in a process of its own, as `clackatoo daemon` runs it, for
//! tests that signal it or upgrade it: set up as the environment says
//! (see `DaemonConfig::from_env`), for the runtime directory in
//! `$XDG_RUNTIME_DIR`, logging to stderr. Run with `--handover-versions`,
//! it says which handovers it reads, and that it takes signals held back
//! for it, as an upgrade asks first. With `--fresh`, it makes none of the
//! workspaces kept from its last run again, as `clackatoo daemon --fresh`.
//!
//! Run as `upgrade NEXT`, it asks the daemon in `$XDG_RUNTIME_DIR` to
//! upgrade in place to the program NEXT, as `clackatoo upgrade` does, and
//! returns once it has.
//!
//! Run as a program whose name ends `-old`, it acts as a build from before
//! daemons caught signals: it doesn't say it takes them, and catches none.
//! Run as one ending `-unadapted`, it says it can't take agents' processes
//! over as an upgrade asks; ending `-repinned`, it says it pins every
//! adapter at another version; ending `-redeclared`, that it declares
//! otherwise to every agent.
//!
//! It stands in for `clackatoo` in tests of daemons connecting to each
//! other, too: run as `bridge`, it relays to the daemon in
//! `$XDG_RUNTIME_DIR`, as `clackatoo bridge` does through ssh; and run by
//! ssh as its askpass program, with
//! [`ASKPASS_VAR`](clackatoo_daemon::remote::ASKPASS_VAR) set, it passes
//! ssh's question to the daemon that asks. A daemon it runs asks ssh's
//! questions through it. Run as `old bridge`, it acts as the bridge of a
//! daemon too old to talk to: it answers the client's hello with protocol
//! 60, and hangs up.

use std::ffi::OsString;
use std::process::ExitCode;

use clackatoo_daemon::upgrade::{self, RESUME_FLAG, VERSIONS_FLAG};
use clackatoo_daemon::{DaemonConfig, RuntimeDir, remote};

fn main() -> ExitCode {
    if let Some(socket) = std::env::var_os(remote::ASKPASS_VAR) {
        let question = std::env::args().nth(1).unwrap_or_default();
        return remote::ask(std::path::Path::new(&socket), &question);
    }
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "old" && args[1] == "bridge" {
        return old_bridge();
    }
    if args.len() == 2 && args[0] == "upgrade" {
        return upgrade_to(std::path::Path::new(&args[1]));
    }
    if args.first().is_some_and(|arg| arg == "bridge") {
        return match remote::bridge(&RuntimeDir::for_current_user(), false) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("bridge: {err}");
                ExitCode::FAILURE
            }
        };
    }
    let program = (std::env::current_exe().ok())
        .map(|program| program.to_string_lossy().into_owned())
        .unwrap_or_default();
    let old = program.ends_with("-old");
    if args.iter().any(|arg| arg == VERSIONS_FLAG) {
        if old {
            let versions: Vec<String> = (upgrade::readable_versions().iter())
                .map(u32::to_string)
                .collect();
            println!("{}", versions.join(" "));
        } else {
            println!("{}", versions_answer(&program));
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
    let mut config = DaemonConfig::from_env();
    if let Some(links) = &mut config.links {
        links.askpass = std::env::current_exe().ok();
    }
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

/// What this build answers [`VERSIONS_FLAG`] with, run as `program`: as
/// the daemon's, but run as a program whose name ends `-unadapted`, as a
/// build that can't take agents' processes over, and ending `-repinned`,
/// as one that pins every adapter at another version, and `-redeclared`,
/// as one that declares otherwise to every agent.
fn versions_answer(program: &str) -> String {
    let answer = upgrade::versions_answer();
    let words = answer.split(' ').filter_map(|word| {
        if program.ends_with("-unadapted")
            && (word == upgrade::TAKES_ADAPTERS || word.starts_with(upgrade::ADAPTER_PIN))
        {
            return None;
        }
        match word.split_once('=') {
            Some((pin, _))
                if program.ends_with("-repinned") && pin.starts_with(upgrade::ADAPTER_PIN) =>
            {
                Some(format!("{pin}=0.0.0-repinned"))
            }
            Some((kind, _))
                if program.ends_with("-redeclared")
                    && kind.starts_with(upgrade::CLIENT_DECLARES) =>
            {
                Some(format!("{kind}=redeclared"))
            }
            _ => Some(word.to_owned()),
        }
    });
    words.collect::<Vec<String>>().join(" ")
}

/// Asks the daemon in `$XDG_RUNTIME_DIR` to upgrade in place to `next`, as
/// `clackatoo upgrade` does, and waits until it has: its connection closes
/// as the new build starts. Fails, saying why, if it won't.
fn upgrade_to(next: &std::path::Path) -> ExitCode {
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStrExt;

    use clackatoo_protocol::frame::{self, FrameDecoder};
    use clackatoo_protocol::{ClientMessage, PROTOCOL_VERSION, ServerMessage};

    let socket = RuntimeDir::for_current_user().socket_path();
    let upgrade = ClientMessage::Upgrade {
        protocol_version: PROTOCOL_VERSION,
        executable: next.as_os_str().as_bytes().to_vec(),
    };
    let asked = std::os::unix::net::UnixStream::connect(&socket).and_then(|mut stream| {
        let message = frame::encode(PROTOCOL_VERSION, &upgrade).map_err(std::io::Error::other)?;
        stream.write_all(&message)?;
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer)?;
        Ok(answer)
    });
    let answer = match asked {
        Ok(answer) => answer,
        Err(err) => {
            eprintln!("upgrade: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut decoder = FrameDecoder::new();
    decoder.push(&answer);
    match decoder.decode::<ServerMessage>(PROTOCOL_VERSION) {
        Ok(None) => ExitCode::SUCCESS,
        refused => {
            eprintln!("upgrade: {refused:?}");
            ExitCode::FAILURE
        }
    }
}

/// Acts as a bridge to a daemon of protocol 60, older than any this build
/// talks to: announces itself, takes the hello, answers with its own
/// version, and hangs up, as daemons before the compatibility window do.
fn old_bridge() -> ExitCode {
    use std::io::{Read, Write};

    use clackatoo_protocol::ServerMessage;
    use clackatoo_protocol::frame::{self, FrameDecoder};

    let mut output = std::io::stdout().lock();
    let _ = output.write_all(b"\x1b[clackatoo-bridge]\n");
    let _ = output.flush();
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0; 1024];
    while decoder.next_payload().ok().flatten().is_none() {
        match std::io::stdin().read(&mut buffer) {
            Ok(n @ 1..) => decoder.push(&buffer[..n]),
            _ => return ExitCode::FAILURE,
        }
    }
    let hello = ServerMessage::Hello {
        protocol_version: 60,
    };
    let _ = output.write_all(&frame::encode(60, &hello).expect("a hello encodes"));
    let _ = output.flush();
    ExitCode::SUCCESS
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
