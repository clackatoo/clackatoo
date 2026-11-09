//! Config files as clackatoo replaces them: whole, all at once, under a
//! lock on their folder. The window's `client/settings.lua` and
//! `startup.toml`, and the daemon's `daemon/settings.lua`, are all saved
//! this way, so a save is never half seen, nor lost to another's.

use std::fs::File;
use std::path::{Path, PathBuf};

/// Who may read and write a new settings file: only its owner, as config
/// can say what's private.
pub const PRIVATE: u32 = 0o600;

/// Locks the folder `dir`, made if it isn't there, until what's returned
/// goes: whatever saves settings there, in any process, the settings menu,
/// `:setpersist` or `clackatoo config`, holds it between its read and its
/// write.
///
/// # Errors
///
/// What stopped it, said.
pub fn lock(dir: &Path) -> Result<File, String> {
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("couldn't make {}: {err}", dir.display()))?;
    let lock = File::open(dir).map_err(|err| format!("couldn't open {}: {err}", dir.display()))?;
    lock.lock()
        .map_err(|err| format!("couldn't lock {}: {err}", dir.display()))?;
    Ok(lock)
}

/// Where `path` is, past any links: itself, if it's no link, or isn't
/// there.
fn through_links(path: &Path) -> Result<PathBuf, String> {
    /// As many links as the kernel follows.
    const MOST: usize = 40;
    let mut path = path.to_owned();
    for _ in 0..MOST {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = std::fs::read_link(&path)
                    .map_err(|err| format!("couldn't read the link {}: {err}", path.display()))?;
                // A link's relative target is from the link's folder.
                path = path.parent().unwrap_or(Path::new("/")).join(target);
            }
            _ => return Ok(path),
        }
    }
    Err(format!(
        "{} is a link to links that go round",
        path.display()
    ))
}

/// Replaces the file at `path` with `text`, all at once, so whatever reads
/// it meanwhile reads either the old file or the new one, and a crash
/// leaves one or the other on the disk. The file keeps its mode, or a new
/// one has `mode`. A file half written is never left behind. A link, as
/// stow and home-manager make, stays one: the file it goes to is replaced.
///
/// # Errors
///
/// What stopped it, said.
pub fn replace(path: &Path, text: &str, mode: u32) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let linked = through_links(path)?;
    let (link, path) = (path, linked.as_path());
    let dir = path.parent().ok_or("the file isn't in a directory")?;
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("couldn't make {}: {err}", dir.display()))?;
    let name = path.file_name().ok_or("the file has no name")?;
    let mode =
        (std::fs::metadata(path)).map_or(mode, |metadata| metadata.permissions().mode() & 0o7777);
    let partial = dir.join(format!(
        ".{}.{}.partial",
        name.to_string_lossy(),
        std::process::id()
    ));
    let written = (|| -> Result<(), String> {
        let mut file = (std::fs::OpenOptions::new())
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&partial)
            .map_err(|err| {
                if link == path {
                    format!("couldn't write in {}: {err}", dir.display())
                } else {
                    format!(
                        "couldn't write in {}, where {} links to: {err}",
                        dir.canonicalize().as_deref().unwrap_or(dir).display(),
                        link.display()
                    )
                }
            })?;
        let failed = |err: std::io::Error| format!("couldn't write {}: {err}", path.display());
        // As the old file was, whatever the umask says.
        (file.set_permissions(std::fs::Permissions::from_mode(mode))).map_err(failed)?;
        file.write_all(text.as_bytes()).map_err(failed)?;
        file.sync_all().map_err(failed)?;
        std::fs::rename(&partial, path)
            .map_err(|err| format!("couldn't replace {}: {err}", path.display()))?;
        // The rename itself, kept.
        (File::open(dir).and_then(|dir| dir.sync_all())).map_err(failed)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    written
}
