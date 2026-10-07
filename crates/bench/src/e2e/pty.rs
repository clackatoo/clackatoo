//! The PTY on its own: a program's output read straight from the PTY and
//! thrown away. That's the most any terminal could manage on this machine,
//! so it puts the daemon's throughput in context.

use std::io::{self, Read as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;
use clackatoo_term::Size;
use rustix::fs::{Mode, OFlags};
use rustix::pty::OpenptFlags;
use rustix::termios::Winsize;

use super::{FLOOD_SCRIPT, READY_TITLE};

/// Runs [`FLOOD_SCRIPT`] on `file` on a fresh PTY of `size`, and times how
/// long reading everything it prints takes, from the moment it's told to
/// start.
pub(crate) fn flood(file: &Path, size: Size) -> anyhow::Result<Duration> {
    let master =
        rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    rustix::pty::grantpt(&master)?;
    rustix::pty::unlockpt(&master)?;
    let slave_path = rustix::pty::ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        slave_path.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    rustix::termios::tcsetwinsize(
        &master,
        Winsize {
            ws_row: size.rows(),
            ws_col: size.cols(),
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )?;

    // Unlike the daemon, this doesn't make the PTY the program's controlling
    // terminal. The script doesn't need job control, and it would take
    // unsafe code to set up.
    let mut command = Command::new("sh");
    command
        .args(["-c", FLOOD_SCRIPT, "sh"])
        .arg(file)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    let mut child = command.spawn().context("could not start sh")?;
    // The master only reports the end of output once every copy of the slave
    // is closed, including the ones `command` holds.
    drop(command);

    let mut master = std::fs::File::from(master);
    let mut buffer = vec![0; crate::workloads::CHUNK_LEN];
    let mut seen = Vec::new();
    let ready = format!("\x1b]0;{READY_TITLE}\x07");
    while !seen.ends_with(ready.as_bytes()) {
        let n = master.read(&mut buffer)?;
        anyhow::ensure!(n > 0, "the script ended before it was ready");
        seen.extend_from_slice(&buffer[..n]);
    }

    let started = Instant::now();
    master.write_all(b"\r")?;
    loop {
        match master.read(&mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            // Linux reports the slave's last close as EIO.
            Err(err) if err.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => break,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
    }
    let elapsed = started.elapsed();
    child.wait()?;
    Ok(elapsed)
}
