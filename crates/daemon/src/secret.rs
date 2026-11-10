//! `clackatoo secret -- COMMAND`: runs a command with what it prints in a
//! region marked private (`OSC 3008`, see [`clackatoo_term::regions`]),
//! opened with an ID picked at random, which nothing the command prints
//! can know, and closed as it ends, however it ends. Its output and its
//! errors are marked, each that goes to a terminal.
//!
//! The wrapper stays to close the region:
//!
//! - Ctrl+C and Ctrl+\ reach the command, which may end by them, but the
//!   wrapper catches them, with a handler that does nothing, which the
//!   command doesn't inherit. Once the command has ended by one, the
//!   region is closed and the wrapper ends by it too, so a shell's loop
//!   running it stops.
//! - SIGTERM and SIGHUP are passed to the command; once it has ended, the
//!   region is closed and the wrapper ends by them.
//! - Stopped (Ctrl+Z) and carried on (`fg`), the shell's prompt between
//!   has closed the region, so a fresh one opens: but the command carries
//!   on at the same moment, so what it prints first may land outside it.
//!
//! A wrapper killed outright (SIGKILL) closes nothing: the shell's next
//! prompt does, or without shell integration, the region's cap.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::atomic::{AtomicU32, Ordering};

use clackatoo_term::regions::OSC;

/// What [`run`] marks a command's output as: every hint, or all but
/// `hide`, or only `hide`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `secret`: masked on screen, never written to disk, kept from
    /// agents.
    Secret,
    /// `private`: never written to disk, kept from agents, but shown.
    Private,
    /// `hide`: masked on screen, but written and read as any output.
    HideOnly,
}

impl Kind {
    fn flags(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Private => "private",
            Self::HideOnly => "hide",
        }
    }
}

/// The signals the wrapper catches.
const CAUGHT: [libc::c_int; 5] = [
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGTERM,
    libc::SIGHUP,
    libc::SIGCONT,
];

/// The signals caught since they were last looked at, a bit each.
static CAUGHT_SINCE: AtomicU32 = AtomicU32::new(0);

/// The terminals the command's output and errors go to, each once, with
/// the region open on them.
struct Marked<'a> {
    terminals: Vec<BorrowedFd<'a>>,
    flags: &'static str,
    id: String,
}

impl Marked<'_> {
    fn write(&self, sequence: &str) -> io::Result<()> {
        for &fd in &self.terminals {
            let mut bytes = sequence.as_bytes();
            while !bytes.is_empty() {
                match rustix::io::write(fd, bytes) {
                    Ok(written) => bytes = &bytes[written..],
                    Err(rustix::io::Errno::INTR) => {}
                    Err(err) => return Err(err.into()),
                }
            }
        }
        Ok(())
    }

    /// Opens a region, with an ID of its own.
    fn start(&mut self) -> io::Result<()> {
        self.id = id()?;
        self.write(&format!(
            "\x1b]{OSC};region;start;{};{}\x1b\\",
            self.id, self.flags
        ))
    }

    fn end(&self) -> io::Result<()> {
        self.write(&format!("\x1b]{OSC};region;end;{}\x1b\\", self.id))
    }
}

/// Runs `command`, its output marked as `kind` says where it goes to a
/// terminal, and returns its exit status as a shell gives it: its code,
/// or 128 and the signal that ended it. Ended by a signal the wrapper
/// caught, the wrapper ends by it too, once the region is closed.
///
/// # Errors
///
/// If the wrapper can't catch the signals it must, before anything's
/// marked, or the command can't be started, or the terminal can't be
/// written to.
pub fn run(kind: Kind, command: &[OsString]) -> io::Result<i32> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no command to run"))?;
    // Never a start without the end the wrapper must stay for.
    catch(true)?;
    let (stdout, stderr) = (io::stdout(), io::stderr());
    let mut marked = Marked {
        terminals: terminals(&[stdout.as_fd(), stderr.as_fd()]),
        flags: kind.flags(),
        id: String::new(),
    };
    marked.start()?;
    let child = match std::process::Command::new(program).args(args).spawn() {
        Ok(child) => child,
        Err(err) => {
            marked.end()?;
            catch(false)?;
            return Err(err);
        }
    };
    let pid = i32::try_from(child.id()).map_err(io::Error::other)?;
    let mut passed_on = None;
    let status = loop {
        match wait(pid) {
            Ok(status) => break status,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {
                let caught = CAUGHT_SINCE.swap(0, Ordering::SeqCst);
                for signal in [libc::SIGTERM, libc::SIGHUP] {
                    if caught & bit(signal) != 0 {
                        signal_to(pid, signal);
                        passed_on = Some(signal);
                    }
                }
                if caught & bit(libc::SIGCONT) != 0 {
                    marked.end()?;
                    marked.start()?;
                }
            }
            Err(err) => {
                marked.end()?;
                return Err(err);
            }
        }
    };
    marked.end()?;
    catch(false)?;
    let ended_by = libc::WIFSIGNALED(status).then(|| libc::WTERMSIG(status));
    match (ended_by, passed_on) {
        (Some(signal @ (libc::SIGINT | libc::SIGQUIT)), _) | (_, Some(signal)) => {
            signal_to(std::process::id().cast_signed(), signal);
        }
        _ => {}
    }
    Ok(match ended_by {
        Some(signal) => 128 + signal,
        None => libc::WEXITSTATUS(status),
    })
}

/// Which of `fds` are terminals, each terminal once.
fn terminals<'a>(fds: &[BorrowedFd<'a>]) -> Vec<BorrowedFd<'a>> {
    let mut seen = Vec::new();
    let mut terminals = Vec::new();
    for &fd in fds {
        if !rustix::termios::isatty(fd) {
            continue;
        }
        let device = rustix::fs::fstat(fd).map(|stat| stat.st_rdev).ok();
        if device.is_none() || !seen.contains(&device) {
            seen.push(device);
            terminals.push(fd);
        }
    }
    terminals
}

/// An ID for a region: 16 random bytes, in hex.
fn id() -> io::Result<String> {
    let mut bytes = [0; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().fold(String::new(), |mut id, byte| {
        let _ = write!(id, "{byte:02x}");
        id
    }))
}

fn bit(signal: libc::c_int) -> u32 {
    1 << signal
}

extern "C" fn caught(signal: libc::c_int) {
    CAUGHT_SINCE.fetch_or(bit(signal), Ordering::SeqCst);
}

/// Catches the signals in [`CAUGHT`], noting each, if `on`, or lets them
/// do what they do by default again. A handler, unlike a signal ignored
/// or held off, isn't inherited by the command. A wait they interrupt
/// returns, to see to them.
#[allow(unsafe_code)]
fn catch(on: bool) -> io::Result<()> {
    for signal in CAUGHT {
        // SAFETY: the action is a valid `sigaction`, zeroed but for its
        // handler, which only sets an atomic, or the default; the old one
        // isn't wanted.
        let set = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = if on {
                caught as extern "C" fn(libc::c_int) as libc::sighandler_t
            } else {
                libc::SIG_DFL
            };
            libc::sigaction(signal, &raw const action, std::ptr::null_mut())
        };
        if set != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Waits for the process `pid` to end, and says how it did, as
/// `waitpid` has it. A signal caught interrupts it.
#[allow(unsafe_code)]
fn wait(pid: i32) -> io::Result<libc::c_int> {
    let mut status = 0;
    // SAFETY: `status` is a valid place for the status.
    match unsafe { libc::waitpid(pid, &raw mut status, 0) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(status),
    }
}

/// Sends `signal` to the process `pid`, if it's there.
#[allow(unsafe_code)]
fn signal_to(pid: i32, signal: libc::c_int) {
    // SAFETY: kill takes any pid and signal, and only fails.
    unsafe {
        libc::kill(pid, signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_random_hex() {
        let (one, two) = (id().unwrap(), id().unwrap());
        assert_eq!(one.len(), 32);
        assert!(one.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(one, two);
    }

    #[test]
    fn a_command_off_a_terminal_runs_unmarked_with_its_status() {
        let sh = |script: &str| ["sh", "-c", script].map(OsString::from);
        assert_eq!(run(Kind::Secret, &sh("exit 3")).unwrap(), 3);
        assert_eq!(run(Kind::Private, &sh("kill -TERM $$")).unwrap(), 128 + 15);
        // Ctrl+C reaches the command, though the wrapper catches it: the
        // command starts with none held off, nor ignored.
        for line in ["SigBlk", "SigIgn"] {
            let none = format!("^{line}:[[:space:]]*0*$");
            let grep = ["grep", "-q", &none, "/proc/self/status"].map(OsString::from);
            assert_eq!(run(Kind::Secret, &grep).unwrap(), 0, "{line}");
        }
        assert!(run(Kind::HideOnly, &[]).is_err());
        assert!(run(Kind::HideOnly, &[OsString::from("/no/such/program")]).is_err());
    }
}
