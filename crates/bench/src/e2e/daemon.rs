//! Running the real daemon binary, and watching it from outside through
//! `/proc`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;
use clackatoo_daemon::{Client, ClientError};

/// Unix socket paths must fit in 108 bytes, including a terminating NUL.
const MAX_SOCKET_PATH: usize = 107;

/// A `clackatoo daemon` process, with a runtime directory of its own, so
/// it never meets a daemon you're actually using. Killed when dropped.
#[derive(Debug)]
pub(crate) struct DaemonProcess {
    child: Child,
    socket: PathBuf,
    runtime_dir: tempfile::TempDir,
}

impl DaemonProcess {
    /// Starts the daemon and waits until it accepts clients. Also returns
    /// how long that took.
    pub(crate) fn start(binary: &Path) -> anyhow::Result<(Self, Duration)> {
        let runtime_dir = tempfile::Builder::new()
            .prefix("ck")
            .tempdir()
            .context("could not make a runtime directory")?;
        let socket = runtime_dir.path().join("clackatoo/daemon.sock");
        anyhow::ensure!(
            socket.as_os_str().len() <= MAX_SOCKET_PATH,
            "the socket path {} is too long for a Unix socket; set TMPDIR to a shorter directory",
            socket.display()
        );

        let started = Instant::now();
        let child = Command::new(binary)
            .env("CLACKATOO_BOARDS", "off")
            .env("CLACKATOO_RECOVERY", "off")
            .env("CLACKATOO_MACHINE", "off")
            .env("CLACKATOO_DISPLAY", "off")
            .env("CLACKATOO_WORKSPACES", "off")
            .arg("daemon")
            .env("XDG_RUNTIME_DIR", runtime_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("could not start {}", binary.display()))?;
        let mut daemon = Self {
            child,
            socket,
            runtime_dir,
        };
        let deadline = started + Duration::from_secs(5);
        loop {
            match Client::connect(&daemon.socket) {
                Ok(_) => return Ok((daemon, started.elapsed())),
                Err(ClientError::Connect(err))
                    if matches!(
                        err.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) => {}
                Err(err) => return Err(err).context("could not connect to the daemon"),
            }
            if let Some(status) = daemon.child.try_wait()? {
                anyhow::bail!("the daemon exited while starting: {status}");
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "the daemon didn't start listening within 5s"
            );
            std::thread::sleep(Duration::from_micros(50));
        }
    }

    /// The runtime directory the daemon runs in, for clients that find the
    /// daemon through `XDG_RUNTIME_DIR`.
    pub(crate) fn runtime_dir(&self) -> &Path {
        self.runtime_dir.path()
    }

    /// The socket clients connect to.
    pub(crate) fn socket(&self) -> &Path {
        &self.socket
    }

    /// A new connection to the daemon.
    pub(crate) fn connect(&self) -> anyhow::Result<Client> {
        Client::connect(&self.socket).context("could not connect to the daemon")
    }

    /// The daemon's resident memory: the RAM it is actually using, in bytes.
    pub(crate) fn resident_memory(&self) -> anyhow::Result<u64> {
        let status = self.read_proc("status")?;
        let kib = status_field(&status, "VmRSS:").context("no VmRSS in /proc status")?;
        Ok(kib * 1024)
    }

    /// How many times the daemon has been switched out of the CPU, across
    /// all its threads. Each wakeup from sleep is one switch, so a quiet
    /// daemon's count shouldn't move.
    pub(crate) fn context_switches(&self) -> anyhow::Result<u64> {
        let tasks = self.proc_path("task");
        let mut total = 0;
        for task in std::fs::read_dir(&tasks).context("could not list the daemon's threads")? {
            let status = std::fs::read_to_string(task?.path().join("status"))?;
            for field in ["voluntary_ctxt_switches:", "nonvoluntary_ctxt_switches:"] {
                total += status_field(&status, field).context("no context switch counts")?;
            }
        }
        Ok(total)
    }

    /// How much CPU time the daemon has used, across all its threads, to the
    /// nanosecond (from `/proc/<pid>/task/*/schedstat`, unlike the 10ms
    /// ticks of `/proc/<pid>/stat`).
    pub(crate) fn cpu_time(&self) -> anyhow::Result<Duration> {
        let tasks = self.proc_path("task");
        let mut total = 0;
        for task in std::fs::read_dir(&tasks).context("could not list the daemon's threads")? {
            let schedstat = std::fs::read_to_string(task?.path().join("schedstat"))?;
            let on_cpu: u64 = schedstat
                .split_whitespace()
                .next()
                .and_then(|field| field.parse().ok())
                .context("malformed schedstat")?;
            total += on_cpu;
        }
        Ok(Duration::from_nanos(total))
    }

    fn proc_path(&self, file: &str) -> PathBuf {
        Path::new("/proc")
            .join(self.child.id().to_string())
            .join(file)
    }

    fn read_proc(&self, file: &str) -> anyhow::Result<String> {
        let path = self.proc_path(file);
        std::fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        // Killing the daemon closes every PTY, which hangs up the programs on
        // them too.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The number in a `/proc/*/status` line such as `VmRSS:   3412 kB`.
fn status_field(status: &str, name: &str) -> Option<u64> {
    let line = status.lines().find(|line| line.starts_with(name))?;
    line[name.len()..].split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_fields_parse() {
        let status = "Name:\tclackatoo\nVmRSS:\t    3412 kB\nvoluntary_ctxt_switches:\t17\n";
        assert_eq!(status_field(status, "VmRSS:"), Some(3412));
        assert_eq!(status_field(status, "voluntary_ctxt_switches:"), Some(17));
        assert_eq!(status_field(status, "VmSwap:"), None);
    }
}
