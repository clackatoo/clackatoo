//! Pseudo-terminals, and starting programs on them.
//!
//! A PTY is a pair of kernel devices. The program gets the "slave" end as its
//! stdin, stdout and stderr, and it behaves like a real terminal (line
//! editing, job control, window size). We keep the "master" end: reading it
//! gives us what the program printed, and writing to it is typing.

use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use clackatoo_term::Size;
use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, PidfdFlags};
use rustix::pty::OpenptFlags;
use rustix::termios::Winsize;

/// Environment variables that describe the terminal the *daemon* was started
/// in. Programs in our terminals would otherwise think they are running
/// inside tmux, kitty and so on, and behave accordingly.
const OUTER_TERMINAL_VARIABLES: &[&str] = &[
    "COLUMNS",
    "LINES",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "VTE_VERSION",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_LISTEN_ON",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_SHELL_FEATURES",
    "ITERM_SESSION_ID",
    "WT_SESSION",
    "KONSOLE_VERSION",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
];

/// A program running on a PTY.
#[derive(Debug)]
pub(crate) struct Spawned {
    pub(crate) pty: Pty,
    pub(crate) child: Child,
    /// A pidfd: a file descriptor that becomes readable when the child exits.
    /// That's how the event loop learns of the exit without signal handlers.
    pub(crate) exit: OwnedFd,
}

/// What to run, and where.
#[derive(Debug)]
pub(crate) struct Program<'a> {
    pub(crate) path: &'a OsStr,
    pub(crate) args: &'a [String],
    pub(crate) cwd: &'a Path,
}

/// Starts `program` on a new PTY of `size`.
#[allow(unsafe_code)]
pub(crate) fn spawn(program: &Program<'_>, size: Size) -> io::Result<Spawned> {
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
    rustix::termios::tcsetwinsize(&master, winsize(size))?;

    let mut command = Command::new(program.path);
    command
        .args(program.args)
        .current_dir(program.cwd)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    for variable in OUTER_TERMINAL_VARIABLES {
        command.env_remove(variable);
    }
    command
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env("TERM_PROGRAM", "clackatoo")
        .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));

    // SAFETY: the closure runs in the child between fork and exec, where only
    // async-signal-safe operations are allowed. It makes two raw system calls
    // through rustix, with no allocation, locking or shared state: setsid,
    // then TIOCSCTTY on fd 0, which std has already pointed at the slave.
    unsafe {
        command.pre_exec(|| {
            // A new session, so the program gets its own process group and
            // job control, with the PTY as its controlling terminal.
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
            Ok(())
        });
    }
    let child = command.spawn()?;
    // The slave fds go with `command`. Close them now, because the master only
    // reports EOF once every copy of the slave is closed, and ours would keep
    // it open forever.
    drop(command);

    let flags = rustix::fs::fcntl_getfl(&master)?;
    rustix::fs::fcntl_setfl(&master, flags | OFlags::NONBLOCK)?;
    let exit = rustix::process::pidfd_open(Pid::from_child(&child), PidfdFlags::empty())?;

    Ok(Spawned {
        pty: Pty { master },
        child,
        exit,
    })
}

/// The master end of a PTY, in non-blocking mode.
#[derive(Debug)]
pub(crate) struct Pty {
    master: OwnedFd,
}

impl Pty {
    /// Reads the program's output. On Linux, once the program and everything
    /// else holding the slave have closed it, this fails with `EIO`, not 0.
    pub(crate) fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(rustix::io::read(&self.master, buf)?)
    }

    /// Writes input for the program.
    pub(crate) fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        Ok(rustix::io::write(&self.master, bytes)?)
    }
}

impl AsRawFd for Pty {
    fn as_raw_fd(&self) -> RawFd {
        self.master.as_fd().as_raw_fd()
    }
}

fn winsize(size: Size) -> Winsize {
    Winsize {
        ws_row: size.rows(),
        ws_col: size.cols(),
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}
