//! Upgrading the daemon in place: a newer build takes over its terminals,
//! and the programs in them carry on as if nothing happened.
//!
//! The daemon `exec`s the new build, replacing its own program but keeping
//! its process. That matters: the programs in its terminals are its
//! children, and only a parent can reap a child and learn its exit status.
//! A fresh process handed the PTYs could watch them, but never report that
//! a program exited with 3.
//!
//! Everything else goes across like this. The daemon writes its
//! [`State`](crate::handover::State) into a memfd, an anonymous file, then
//! sends that and every fd the state refers to (the listening socket, the
//! lock, each PTY and pidfd) over a socket pair to itself, as `SCM_RIGHTS`
//! messages. It execs the new build with the receiving end as its stdin
//! and `--resume`. Exec closes the daemon's own copies, which are all
//! close-on-exec, and the new build receives the copies in flight.
//! Receiving fds is safe Rust; inheriting them by number wouldn't be.
//!
//! The listening socket never closes, so clients that connect meanwhile
//! wait in its queue. Every connection does close, and clients reconnect.
//! If exec fails, the daemon still holds everything it sent, and carries
//! on from its own handover.
//!
//! Once exec succeeds there's no way back, so first the daemon checks that
//! the new program is a build that can take over: it runs it with the
//! arguments the exec will use plus [`VERSIONS_FLAG`], which lists the
//! handover versions it reads. A build that wouldn't accept those
//! arguments fails that the same way.
//! Without that, an older build, or any other program, would take the
//! daemon's place and lose every terminal. (Even a file that isn't a
//! program would: exec runs those with `/bin/sh`.)

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, IoSlice, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use clackatoo_protocol::ServerMessage;
use clackatoo_protocol::frame;
use rustix::fs::MemfdFlags;
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketFlags, SocketType,
};

use crate::handover::{Fds, Handover, HandoverError, State};
use crate::runtime::{DaemonLock, LockError, RuntimeDir};
use crate::{Daemon, DaemonConfig};

/// The most fds one message can carry: the kernel's `SCM_MAX_FD`.
const FDS_PER_MESSAGE: usize = 253;

/// The flag that tells a new build it is taking over: see the module docs.
pub const RESUME_FLAG: &str = "--resume";

/// The flag that asks a build which handovers it can take over from,
/// instead of running the daemon. It prints [`readable_versions`],
/// separated by spaces.
pub const VERSIONS_FLAG: &str = "--handover-versions";

/// How long a build may take to answer [`VERSIONS_FLAG`].
const VERSIONS_TIMEOUT: Duration = Duration::from_secs(2);

/// The handover versions this build can take over from.
#[must_use]
pub fn readable_versions() -> &'static [u32] {
    crate::handover::READABLE
}

/// Checks that `executable`, run with `args`, is a build that can take
/// over from this one, by asking it which handover versions it reads.
pub(crate) fn check_takes_over(executable: &Path, args: &[OsString]) -> Result<(), String> {
    let run = |err: io::Error| format!("{} can't be run: {err}", executable.display());
    let mut child = Command::new(executable)
        .args(args)
        .arg(VERSIONS_FLAG)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(run)?;
    let deadline = std::time::Instant::now() + VERSIONS_TIMEOUT;
    while child.try_wait().map_err(run)?.is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{} didn't answer", executable.display()));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut answer = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = io::Read::read_to_string(&mut stdout, &mut answer);
    }
    let takes_over = answer
        .split_whitespace()
        .any(|version| version.parse() == Ok(crate::handover::VERSION));
    if takes_over {
        Ok(())
    } else {
        Err(format!(
            "{} can't take over from this build: it isn't a build of clackatoo that reads \
             its handover (version {})",
            executable.display(),
            crate::handover::VERSION
        ))
    }
}

/// A request to upgrade, which [`Daemon::run`] stopped for. The daemon has
/// handed over everything; [`serve`] does the rest.
#[derive(Debug)]
pub struct Upgrade {
    pub(crate) executable: PathBuf,
    pub(crate) handover: Handover,
    /// The connection that asked, which is told if the upgrade fails. The
    /// exec closes it if it doesn't.
    pub(crate) requester: Option<mio::net::UnixStream>,
}

/// The daemon couldn't run.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The runtime directory is unusable, or another daemon has it.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Listening or waiting for events failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Runs the daemon for `runtime` until it is shut down, upgrading in place
/// when a newer client asks. `args` are the arguments the program was run
/// with, minus the program itself and [`RESUME_FLAG`]: an upgrade runs the
/// new build with the same ones.
///
/// With `resume`, this process was exec'd by an older build, and its
/// handover is on stdin. If it can't be read, the terminals are lost (their
/// programs are hung up), and the daemon starts afresh.
///
/// # Errors
///
/// If another daemon holds the runtime directory, or the daemon can't
/// listen or wait for events.
pub fn serve(
    runtime: &RuntimeDir,
    config: &DaemonConfig,
    resume: bool,
    args: &[OsString],
) -> Result<(), ServeError> {
    let resumed = if resume {
        receive()
            .and_then(|handover| resume_from(handover, config))
            .map_err(|err| log::error!("taking over from the last build failed: {err}"))
            .ok()
    } else {
        None
    };
    let (lock, mut daemon) = if let Some(resumed) = resumed {
        resumed
    } else {
        let lock = runtime.lock()?;
        (lock, Daemon::bind(&runtime.socket_path(), config.clone())?)
    };
    daemon.set_upgrade_args(args);
    // Once the lock is held: the directory is ours.
    daemon.install_shell_integration(runtime.path().join("shell"));

    while let Some(Upgrade {
        executable,
        mut handover,
        requester,
    }) = daemon.run()?
    {
        handover.state.lock = Some(handover.fds.add(lock.as_fd().try_clone_to_owned()?));
        // Closes every connection, including the one that asked.
        drop(daemon);
        log::info!("upgrading to {}", executable.display());
        let err = exec(&executable, args, &handover);
        log::error!("upgrading to {} failed: {err}", executable.display());
        if let Some(requester) = requester {
            tell_failure(requester, &err);
        }
        handover.state.lock = None;
        daemon = Daemon::resume(handover, config.clone()).map_err(io::Error::other)?;
        daemon.set_upgrade_args(args);
        daemon.install_shell_integration(runtime.path().join("shell"));
    }
    Ok(())
}

/// Tells the client that asked for an upgrade why it failed.
fn tell_failure(requester: mio::net::UnixStream, err: &io::Error) {
    let mut stream = std::os::unix::net::UnixStream::from(OwnedFd::from(requester));
    let refusal = ServerMessage::NotUpgrading {
        reason: format!("starting the new build failed: {err}"),
    };
    let sent = stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(1))))
        .and_then(|()| frame::encode(&refusal).map_err(io::Error::other))
        .and_then(|bytes| stream.write_all(&bytes));
    if let Err(err) = sent {
        log::debug!("telling the client failed: {err}");
    }
}

/// The lock and the daemon from `handover`.
fn resume_from(
    mut handover: Handover,
    config: &DaemonConfig,
) -> Result<(DaemonLock, Daemon), HandoverError> {
    let lock = handover
        .state
        .lock
        .take()
        .ok_or(HandoverError::Damaged("the lock is missing"))?;
    let lock = DaemonLock::from_fd(handover.fds.take(lock)?);
    Ok((lock, Daemon::resume(handover, config.clone())?))
}

/// Execs `executable` with `args` and [`RESUME_FLAG`], handing it
/// `handover`. Returns only if that fails.
fn exec(executable: &Path, args: &[OsString], handover: &Handover) -> io::Error {
    // Returns both ends of the socket pair. Ours must stay open until the
    // exec closes it, which ends the stream for the new build; if the exec
    // fails, closing it drops the fds in flight.
    let send = || -> io::Result<(OwnedFd, OwnedFd)> {
        let mut memfd = File::from(rustix::fs::memfd_create(
            "clackatoo-handover",
            MemfdFlags::CLOEXEC,
        )?);
        handover.state.write(&mut memfd)?;
        let (ours, theirs) = rustix::net::socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        // Nothing reads until the exec, so a full buffer must fail rather
        // than wait forever.
        rustix::io::ioctl_fionbio(&ours, true)?;
        let fds: Vec<BorrowedFd<'_>> = std::iter::once(memfd.as_fd())
            .chain(handover.fds.iter().map(AsFd::as_fd))
            .collect();
        for chunk in fds.chunks(FDS_PER_MESSAGE) {
            let mut space =
                vec![MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(chunk.len()))];
            let mut control = SendAncillaryBuffer::new(&mut space);
            control.push(SendAncillaryMessage::ScmRights(chunk));
            rustix::net::sendmsg(
                &ours,
                &[IoSlice::new(&[0])],
                &mut control,
                SendFlags::empty(),
            )?;
        }
        Ok((ours, theirs))
    };
    match send() {
        Ok((_ours, theirs)) => Command::new(executable)
            .args(args)
            .arg(RESUME_FLAG)
            .stdin(Stdio::from(theirs))
            .exec(),
        Err(err) => err,
    }
}

/// Receives the handover the last build sent on stdin (see the module
/// docs), then points stdin at `/dev/null` again.
fn receive() -> Result<Handover, HandoverError> {
    let fds = receive_fds();
    // Whatever happened: anything left in flight on the old stdin, such as
    // the lock, is dropped with it.
    rustix::stdio::dup2_stdin(File::open("/dev/null")?).map_err(io::Error::from)?;

    let mut fds = fds?.into_iter();
    let mut memfd = File::from(fds.next().ok_or(HandoverError::NotAHandover)?);
    let state = State::read(&mut memfd)?;
    Ok(Handover {
        state,
        fds: Fds::from_received(fds.collect()),
    })
}

/// Every fd the last build sent on stdin, in order.
fn receive_fds() -> Result<Vec<OwnedFd>, HandoverError> {
    let stdin = io::stdin();
    let mut fds = Vec::new();
    loop {
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(FDS_PER_MESSAGE))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let mut byte = [0];
        let received = rustix::net::recvmsg(
            stdin.as_fd(),
            &mut [IoSliceMut::new(&mut byte)],
            &mut control,
            RecvFlags::CMSG_CLOEXEC,
        )
        .map_err(io::Error::from)?;
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(received) = message {
                fds.extend(received);
            }
        }
        if received.flags.contains(ReturnFlags::CTRUNC) {
            return Err(HandoverError::Damaged("fds were cut off"));
        }
        if received.bytes == 0 {
            return Ok(fds);
        }
    }
}
