//! Where the daemon's socket lives, and making sure only one daemon runs.
//!
//! Everything lives in a per-user runtime directory:
//! `$XDG_RUNTIME_DIR/clackatoo/`, or `/tmp/clackatoo-$UID/` where there is no
//! XDG runtime directory. The directory is only accessible by its owner. Any
//! process that can reach the socket can run commands as that user, so this
//! is the daemon's access control.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::FlockOperation;

/// The daemon's per-user runtime directory.
#[derive(Clone, Debug)]
pub struct RuntimeDir {
    path: PathBuf,
}

impl RuntimeDir {
    /// The runtime directory for the current user. It may not exist yet.
    #[must_use]
    pub fn for_current_user() -> Self {
        let path = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("clackatoo"),
            _ => std::env::temp_dir()
                .join(format!("clackatoo-{}", rustix::process::getuid().as_raw())),
        };
        Self { path }
    }

    /// A runtime directory at a specific path, for tests.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// The directory's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where the daemon listens.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.path.join("daemon.sock")
    }

    /// Creates the directory if needed, then takes the lock that makes this
    /// the only daemon for the directory. The lock lasts as long as the
    /// returned [`DaemonLock`].
    ///
    /// # Errors
    ///
    /// [`LockError::AlreadyRunning`] if another daemon holds the lock, or
    /// [`LockError::Insecure`] if the directory exists but other users could
    /// get into it.
    pub fn lock(&self) -> Result<DaemonLock, LockError> {
        self.create_private()?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.path.join("daemon.lock"))?;
        match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(DaemonLock { _file: file }),
            Err(rustix::io::Errno::WOULDBLOCK) => Err(LockError::AlreadyRunning),
            Err(err) => Err(LockError::Io(err.into())),
        }
    }

    /// Creates the directory, owner-only, or checks that an existing one is
    /// owner-only. In `/tmp`, another user could have created it first.
    ///
    /// # Errors
    ///
    /// [`LockError::Insecure`] if the directory exists but other users could
    /// get into it, or [`LockError::Io`] if it can't be made.
    pub fn create_private(&self) -> Result<(), LockError> {
        match DirBuilder::new()
            .recursive(false)
            .mode(0o700)
            .create(&self.path)
        {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err.into()),
        }
        let metadata = std::fs::symlink_metadata(&self.path)?;
        let ours = metadata.uid() == rustix::process::getuid().as_raw();
        if !metadata.is_dir() || !ours || metadata.mode() & 0o077 != 0 {
            return Err(LockError::Insecure(self.path.clone()));
        }
        Ok(())
    }
}

/// Proof that this process is the only daemon for a runtime directory.
/// Released when dropped, or when the process exits.
#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
}

/// The daemon lock couldn't be taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another daemon is running.
    #[error("a clackatoo daemon is already running")]
    AlreadyRunning,
    /// The runtime directory is not private to this user.
    #[error("refusing to use {0}: it must be a directory only you can access")]
    Insecure(PathBuf),
    /// Creating the directory or lock file failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn only_one_lock_at_a_time() {
        let temp = tempfile::tempdir().unwrap();
        let dir = RuntimeDir::at(temp.path().join("clackatoo"));
        let lock = dir.lock().unwrap();
        assert!(matches!(dir.lock(), Err(LockError::AlreadyRunning)));
        drop(lock);
        dir.lock().unwrap();
    }

    #[test]
    fn the_directory_is_created_private() {
        let temp = tempfile::tempdir().unwrap();
        let dir = RuntimeDir::at(temp.path().join("clackatoo"));
        let _lock = dir.lock().unwrap();
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn a_shared_directory_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("clackatoo");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            RuntimeDir::at(path).lock(),
            Err(LockError::Insecure(_))
        ));
    }
}
