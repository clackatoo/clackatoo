//! Process groups: a program the daemon starts in a group of its own,
//! and whatever it starts in turn, which stays in that group unless it
//! moves itself out. Agents and language servers are often run through
//! `npx`, which starts node, which starts the program doing the work: a
//! signal to the program the daemon started alone would leave the rest
//! running, holding its pipes, and sometimes a lock or a port. So agents,
//! language servers, the browser and git each run in a group of their
//! own, and stopping one signals the whole group.
//!
//! Terminals' programs are left out: each has a session of its own, with
//! the terminal as its controlling terminal, for job control.
//!
//! A group is signalled through its leader's pidfd, never by its number
//! (Linux 6.9 on). The pidfd keeps the kernel's record of the leader's
//! process ID, and the group hangs off that record for as long as
//! anything is in it, so a signal can never reach another group that
//! took the number, even after the leader has been reaped. Signal 0 says
//! whether anything is left. An older kernel can only signal the leader,
//! as the daemon did before groups.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::sync::OnceLock;

use rustix::process::{PidfdFlags, Signal};

/// `pidfd_send_signal`'s flag to signal the pidfd's process group, from
/// `linux/pidfd.h`, which the libc crate doesn't have.
const PIDFD_SIGNAL_PROCESS_GROUP: libc::c_uint = 4;

/// The process group a child leads, known by the child's pidfd.
#[derive(Debug)]
pub(crate) struct Group {
    pidfd: OwnedFd,
}

impl Group {
    /// The group led by the process whose pidfd is `pidfd`.
    pub(crate) fn new(pidfd: OwnedFd) -> Self {
        Self { pidfd }
    }

    /// Sends `signal` to everything in the group. Returns whether there
    /// was anything to send it to.
    pub(crate) fn signal(&self, signal: Signal) -> bool {
        signal_group(self.pidfd.as_fd(), signal)
    }

    /// Whether anything is left in the group. A leader not yet reaped
    /// counts, even once it has exited.
    pub(crate) fn alive(&self) -> bool {
        send(self.pidfd.as_fd(), 0).is_ok()
    }
}

/// Sends `signal` to the group led by the process whose pidfd is
/// `pidfd`, as [`Group::signal`] does. SIGKILL goes to the leader itself
/// as well, in case it moved into another group and left something in
/// its own.
pub(crate) fn signal_group(pidfd: BorrowedFd<'_>, signal: Signal) -> bool {
    let sent = send(pidfd, signal.as_raw()).is_ok();
    if signal == Signal::KILL {
        pidfd_send_signal(pidfd, signal.as_raw(), 0).is_ok() || sent
    } else {
        sent
    }
}

/// Sends signal number `signal` to the group led by the process whose
/// pidfd is `pidfd`. With nothing left in the group, the leader alone is
/// sent it: a leader that moved itself into another group still hears.
/// With a kernel too old to signal a group by its leader's pidfd, the
/// leader alone is sent it.
fn send(pidfd: BorrowedFd<'_>, signal: libc::c_int) -> io::Result<()> {
    if !groups_signalled() {
        return pidfd_send_signal(pidfd, signal, 0);
    }
    match pidfd_send_signal(pidfd, signal, PIDFD_SIGNAL_PROCESS_GROUP) {
        Err(err) if err.raw_os_error() == Some(libc::ESRCH) => pidfd_send_signal(pidfd, signal, 0),
        sent => sent,
    }
}

/// Whether this kernel can signal a process group through its leader's
/// pidfd (Linux 6.9 can), asked once by sending signal 0, which does
/// nothing, to the daemon's own group, if it leads one. A kernel that
/// doesn't know the flag says the call is invalid; any other answer means
/// it does. If it can't, the log says so once.
fn groups_signalled() -> bool {
    static SIGNALLED: OnceLock<bool> = OnceLock::new();
    *SIGNALLED.get_or_init(|| {
        let ours = rustix::process::pidfd_open(rustix::process::getpid(), PidfdFlags::empty());
        let signalled = ours.is_ok_and(|ours| {
            let asked = pidfd_send_signal(ours.as_fd(), 0, PIDFD_SIGNAL_PROCESS_GROUP);
            !matches!(asked, Err(err) if err.raw_os_error() == Some(libc::EINVAL))
        });
        if !signalled {
            log::warn!(
                "this kernel can't signal a process group through a pidfd (Linux 6.9 can): \
                 stopping an agent or a language server reaches only its own process"
            );
        }
        signalled
    })
}

/// The `pidfd_send_signal` system call, with `flags`, which rustix's
/// doesn't take.
#[allow(unsafe_code)]
fn pidfd_send_signal(
    pidfd: BorrowedFd<'_>,
    signal: libc::c_int,
    flags: libc::c_uint,
) -> io::Result<()> {
    // SAFETY: the system call takes an fd, borrowed for the call, a signal
    // number, a pointer to a `siginfo_t`, which may be null, and flags. It
    // reads no memory of ours, as the pointer is null, and writes none.
    let sent = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            flags,
        )
    };
    if sent == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use rustix::process::{Pid, PidfdFlags};

    use super::*;

    /// Whether the process `pid` is still running: there, and not a
    /// zombie.
    fn running(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            (stat.rsplit_once(") ")).is_some_and(|(_, rest)| !rest.starts_with('Z'))
        })
    }

    #[test]
    fn a_group_is_signalled_through_its_leaders_pidfd_even_once_the_leader_is_reaped() {
        // The leader starts a sleep in its group, says its pid, and exits.
        let mut leader = Command::new("sh")
            .args(["-c", "sleep 100 & echo $!; exec sleep 0.1"])
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let pidfd =
            rustix::process::pidfd_open(Pid::from_child(&leader), PidfdFlags::empty()).unwrap();
        let group = Group::new(pidfd);
        let mut line = String::new();
        BufReader::new(leader.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let sleep: u32 = line.trim().parse().unwrap();
        leader.wait().unwrap();
        assert!(running(sleep));
        assert!(group.alive(), "the sleep is still in it");

        assert!(group.signal(Signal::KILL));
        let deadline = Instant::now() + Duration::from_secs(5);
        while group.alive() {
            assert!(Instant::now() < deadline, "the group never emptied");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!running(sleep));
        assert!(!group.signal(Signal::TERM), "nothing left to signal");
    }

    #[test]
    fn a_leader_that_left_its_group_still_hears() {
        // Started in the test's own group, it leads none: it's signalled
        // alone, rather than not at all, and never the group it's in.
        let mut alone = Command::new("sleep").arg("100").spawn().unwrap();
        let pidfd =
            rustix::process::pidfd_open(Pid::from_child(&alone), PidfdFlags::empty()).unwrap();
        let group = Group::new(pidfd);
        assert!(group.alive());
        assert!(group.signal(Signal::KILL));
        alone.wait().unwrap();
        assert!(!group.alive());
    }
}
