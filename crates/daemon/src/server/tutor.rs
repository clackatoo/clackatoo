//! `:Tutor` lessons' files, for windows on another machine than the
//! daemon (CLACKA-243): [`Request::Tutor`].
//!
//! A window whose daemon is on its own machine writes a lesson's files
//! itself, in a scratch folder of its own. One whose daemon is on another
//! machine can't: the lesson's panes are made here, so its files must be
//! here too. It asks this daemon to make them, in a folder of the daemon's
//! own: `tutor` in the folder its socket is in
//! (`$XDG_RUNTIME_DIR/clackatoo`), made for the user alone, and refused if
//! it's a link or another user's. Each lesson gets a new folder there,
//! named for when it was made, as `20261109-101500-1`, holding the
//! lesson's text, `LESSON.txt`, and its practice folder, `LESSON`.
//!
//! Each folder is the client's that made it, for as long as it's
//! connected: only it may write the lesson's text again, or remove the
//! folder, and it's never pruned. One whose client has gone, as by a
//! reconnection, is left to be pruned.
//!
//! A client only ever names a folder made there, and plain file names, with
//! no `/` and not starting with `.`: nothing is written outside it, and
//! nothing through a link. Each file is made new (`O_EXCL`, `O_NOFOLLOW`),
//! for the user alone; the lesson's text, written again, goes to a new
//! file renamed over the old. A web device is refused: a phone has no
//! lessons, and nothing else may write files through this. So is a daemon
//! relaying for clients of its own: a window asks its own daemon, never
//! one another daemon reaches.
//!
//! The window removes a lesson's folder as the lesson ends. One left
//! behind, as by a window that stopped without doing so, goes once it's
//! [`KEPT_FOR`] old, or as more than [`MOST_KEPT`] are kept, the oldest
//! first, as a new one is made; the runtime folder is emptied as the user
//! logs out. A folder's age is from the newest change to it or what's in
//! it, so a lesson practised in isn't old.
//!
//! Its files are written on the loop: a few small files, at most
//! [`MOST_BYTES`] in all, as a buffer's save is. Folders are removed by a
//! thread of its own, as a practice shell may have filled one.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, SystemTime};

use clackatoo_protocol::{Error, Response, TutorFiles};
use mio::Token;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;

use super::pastes::{is_stamped, open_folder, stamp, stamped};
use super::{Daemon, Source};

/// How long a lesson's folder left behind is kept.
const KEPT_FOR: Duration = Duration::from_hours(7 * 24);

/// The most lessons' folders kept: the oldest go first.
const MOST_KEPT: usize = 16;

/// The most practice files a lesson may have.
const MOST_FILES: usize = 32;

/// The most bytes a lesson's text and practice files may have in all.
const MOST_BYTES: usize = 1024 * 1024;

/// The deepest a folder in a lesson's is removed from: one deeper, as a
/// practice shell could make, is left, and the folders it's in.
const DEEPEST: usize = 64;

/// The most of a lesson's folder looked at for when it last changed: its
/// own time, and as many of what's in it.
const MOST_LOOKED_AT: usize = 4096;

/// The lessons' folders made, and the thread removing them.
#[derive(Debug, Default)]
pub(super) struct Tutor {
    /// The lessons' folders made, by name, each with the client that made
    /// it, for as long as it's connected: shared with the thread, which
    /// never removes one of these.
    makers: Arc<Mutex<HashMap<String, Token>>>,
    /// The thread's queue, once it has started.
    removing: Option<mpsc::Sender<Removal>>,
}

/// What the thread removes, in the folder of lessons' folders, open.
struct Removal {
    folder: OwnedFd,
    what: Removing,
}

/// What [`Removal`] removes.
enum Removing {
    /// A lesson's folder, by name.
    Folder(String),
    /// Those left behind, as [`prune`] says, at this time.
    Pruned(SystemTime),
}

impl Tutor {
    /// The client at `token` has gone: the folders it made are no one's.
    pub(super) fn forget(&mut self, token: Token) {
        self.makers().retain(|_, maker| *maker != token);
    }

    fn makers(&self) -> MutexGuard<'_, HashMap<String, Token>> {
        self.makers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Has `what` in `folder` removed by the thread, starting it if it
    /// hasn't started; or here and now, if it can't start.
    fn remove(&mut self, folder: &OwnedFd, what: Removing) {
        let folder = match folder.try_clone() {
            Ok(folder) => folder,
            Err(err) => return log::warn!("lessons' folders can't be removed: {err}"),
        };
        let removal = Removal { folder, what };
        let removal = match &self.removing {
            Some(removing) => match removing.send(removal) {
                Ok(()) => return,
                Err(mpsc::SendError(removal)) => removal,
            },
            None => removal,
        };
        let (removing, removals) = mpsc::channel::<Removal>();
        let makers = Arc::clone(&self.makers);
        let started = std::thread::Builder::new()
            .name("lessons-removed".into())
            .spawn(move || {
                for removal in removals {
                    removal.run(&makers);
                }
            });
        match started {
            Ok(_) => {
                self.removing = Some(removing);
                if let Some(removing) = &self.removing {
                    let _ = removing.send(removal);
                }
            }
            Err(err) => {
                log::warn!("no thread to remove lessons' folders: {err}");
                removal.run(&self.makers);
            }
        }
    }

    /// The name of `dir`, if it's a lesson's folder in `lessons` the
    /// client at `token` made.
    fn made_by<'d>(
        &self,
        lessons: &Lessons,
        dir: &'d str,
        token: Token,
    ) -> Result<&'d str, String> {
        let name = lessons.made(dir)?;
        if self.makers().get(name) == Some(&token) {
            Ok(name)
        } else {
            Err(format!("{dir} isn't a folder this window made"))
        }
    }
}

impl Daemon {
    /// Answers [`Request::Tutor`](clackatoo_protocol::Request::Tutor),
    /// from the client at `token`.
    pub(super) fn tutor_files(
        &mut self,
        token: Token,
        files: TutorFiles,
    ) -> Result<Response, Error> {
        if self.device_of(token).is_some() {
            return Err(Error::Other("a phone has no lessons".into()));
        }
        if matches!(
            self.sources.get(token.0),
            Some(Source::Client(client)) if client.relays || client.machine.is_some()
        ) {
            return Err(Error::Other(
                "only a window's own daemon makes its lessons' files".into(),
            ));
        }
        let lessons = self.lessons_folder().map_err(|why| {
            Error::Other(format!(
                "‘{}’ has nowhere for lessons' files: {why}",
                self.machine.name
            ))
        })?;
        let done = match files {
            TutorFiles::Make {
                lesson,
                text,
                files,
                images,
            } => {
                self.tutor
                    .remove(&lessons.folder, Removing::Pruned(SystemTime::now()));
                // Its maker's from the first, so it's never pruned.
                let mut makers = self.tutor.makers();
                (lessons.make_with(&lesson, &text, &files, &images)).map(|(name, path)| {
                    makers.insert(name, token);
                    Response::Text(path)
                })
            }
            TutorFiles::Rewrite { dir, lesson, text } => {
                (self.tutor.made_by(&lessons, &dir, token))
                    .and_then(|name| lessons.rewrite(name, &lesson, &text))
                    .map(|()| Response::Done)
            }
            TutorFiles::Remove { dir } => (self.tutor.made_by(&lessons, &dir, token))
                .map(str::to_owned)
                .map(|name| {
                    self.tutor.makers().remove(&name);
                    self.tutor.remove(&lessons.folder, Removing::Folder(name));
                    Response::Done
                }),
        };
        done.map_err(|why| Error::Other(format!("the lesson's files: {why}")))
    }

    /// The folder lessons' files are kept in, opened.
    fn lessons_folder(&self) -> Result<Lessons, String> {
        let runtime = (self.runtime.as_ref()).ok_or("its socket isn't in a folder")?;
        let path = runtime.join("tutor");
        let folder = open_folder(&path).map_err(|why| format!("{}: {why}", path.display()))?;
        // As files in it are named by the daemon, through any links: for
        // the window to know them by.
        let path = std::fs::canonicalize(&path).map_err(|err| err.to_string())?;
        Ok(Lessons { path, folder })
    }
}

/// The folder lessons' files are kept in, open.
struct Lessons {
    path: PathBuf,
    folder: OwnedFd,
}

impl Lessons {
    /// Makes a new folder for the lesson `lesson`, with its text and its
    /// practice files, and returns its name and path. Nothing's made if
    /// anything's refused, or can't be.
    #[cfg(test)]
    fn make(
        &self,
        lesson: &str,
        text: &str,
        files: &[(String, String)],
    ) -> Result<(String, String), String> {
        self.make_with(lesson, text, files, &[])
    }

    /// As [`make`](Self::make), with `images` too, practice files of bytes,
    /// as an image is.
    fn make_with(
        &self,
        lesson: &str,
        text: &str,
        files: &[(String, String)],
        images: &[(String, Vec<u8>)],
    ) -> Result<(String, String), String> {
        plain(lesson)?;
        for (name, _) in files {
            plain(name)?;
        }
        for (name, _) in images {
            plain(name)?;
        }
        if files.len() + images.len() > MOST_FILES {
            return Err(format!("more than {MOST_FILES} practice files"));
        }
        let bytes = text.len()
            + (files.iter()).map(|(_, text)| text.len()).sum::<usize>()
            + (images.iter()).map(|(_, bytes)| bytes.len()).sum::<usize>();
        if bytes > MOST_BYTES {
            return Err("more than a MiB of text".into());
        }
        let (name, made) = self.new_folder()?;
        let path = self.path.join(&name);
        let written = (|| -> io::Result<()> {
            rustix::fs::mkdirat(&made, lesson, Mode::RWXU)?;
            let practice = open_dir(&made, lesson)?;
            for (name, text) in files {
                write_new(&practice, name, text.as_bytes())?;
            }
            for (name, bytes) in images {
                write_new(&practice, name, bytes)?;
            }
            write_new(&made, &format!("{lesson}.txt"), text.as_bytes())
        })();
        if let Err(err) = written {
            let _ = remove_named(self.folder.as_fd(), &name);
            return Err(err.to_string());
        }
        let path = (path.into_os_string().into_string())
            .map_err(|_| "the daemon's runtime folder's path isn't UTF-8".to_owned())?;
        Ok((name, path))
    }

    /// Writes the lesson `lesson`'s text in its folder `name` again: to a
    /// new file, renamed over the old. Only the text [`make`](Self::make)
    /// wrote is written again: nothing else is made.
    fn rewrite(&self, name: &str, lesson: &str, text: &str) -> Result<(), String> {
        plain(lesson)?;
        if text.len() > MOST_BYTES {
            return Err("more than a MiB of text".into());
        }
        let folder = open_dir(&self.folder, name).map_err(|err| err.to_string())?;
        let file = format!("{lesson}.txt");
        let stat = rustix::fs::statat(&folder, file.as_str(), AtFlags::SYMLINK_NOFOLLOW);
        if !stat.is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile) {
            return Err(format!("there's no lesson ‘{lesson}’ in that folder"));
        }
        let new = format!(".{file}.new");
        let written = (|| -> io::Result<()> {
            // Left by a write that failed part way.
            match rustix::fs::unlinkat(&folder, new.as_str(), AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => {}
                Err(err) => return Err(err.into()),
            }
            write_new(&folder, &new, text.as_bytes())?;
            rustix::fs::renameat(&folder, new.as_str(), &folder, file.as_str())?;
            Ok(())
        })();
        written.map_err(|err| {
            let _ = rustix::fs::unlinkat(&folder, new.as_str(), AtFlags::empty());
            err.to_string()
        })
    }

    /// The name of `dir`, if it's a folder of this one's, as made for a
    /// lesson: refused if it's anything else.
    fn made<'d>(&self, dir: &'d str) -> Result<&'d str, String> {
        let dir = Path::new(dir);
        let name = (dir.file_name())
            .and_then(|name| name.to_str())
            .filter(|name| dir.parent() == Some(&self.path) && is_stamped(name.as_bytes()));
        name.ok_or_else(|| format!("{} isn't a lesson's folder", dir.display()))
    }

    /// Makes a new folder, for the user alone, named for now, the first
    /// number free after it. Returns its name, and it, open.
    fn new_folder(&self) -> Result<(String, OwnedFd), String> {
        let now = stamp();
        for number in 1..=1000 {
            let name = stamped(&now, number, "");
            match rustix::fs::mkdirat(&self.folder, name.as_str(), Mode::RWXU) {
                Ok(()) => {
                    let made = open_dir(&self.folder, &name).map_err(|err| err.to_string())?;
                    return Ok((name, made));
                }
                Err(Errno::EXIST) => {}
                Err(err) => return Err(err.to_string()),
            }
        }
        Err("a thousand lessons were opened this second".into())
    }
}

impl Removal {
    /// Removes what it says, never a folder one of `makers` made.
    fn run(self, makers: &Mutex<HashMap<String, Token>>) {
        let made = |name: &str| {
            let makers = makers.lock().unwrap_or_else(PoisonError::into_inner);
            makers.contains_key(name)
        };
        match self.what {
            // Made again since, once removed: not this one.
            Removing::Folder(name) if made(&name) => {}
            Removing::Folder(name) => {
                if let Err(err) = remove_named(self.folder.as_fd(), &name) {
                    log::warn!("the lesson's folder {name} couldn't be removed: {err}");
                }
            }
            Removing::Pruned(now) => prune(&self.folder, now, made),
        }
    }
}

/// Removes the lessons' folders in `folder` older than [`KEPT_FOR`] at
/// `now`, and the oldest, leaving room for one more within [`MOST_KEPT`],
/// but never one `made` says a client connected made: asked just before
/// each is removed, as one may be made meanwhile. Only names made for
/// lessons are looked at.
fn prune(folder: &OwnedFd, now: SystemTime, made: impl Fn(&str) -> bool) {
    let Ok(mut dir) = Dir::read_from(folder) else {
        return;
    };
    // The folder may have been read before: its offset is shared.
    dir.rewind();
    let mut kept: Vec<(SystemTime, String)> = Vec::new();
    for entry in dir {
        let Ok(entry) = entry else {
            break;
        };
        let name = entry.file_name();
        let Some(name) = (name.to_str().ok()).filter(|name| is_stamped(name.as_bytes())) else {
            continue;
        };
        let Some(changed) = changed(folder, name) else {
            continue;
        };
        if now.duration_since(changed).is_ok_and(|age| age > KEPT_FOR) && !made(name) {
            let _ = remove_named(folder.as_fd(), name);
        } else {
            kept.push((changed, name.to_owned()));
        }
    }
    kept.sort();
    let mut over = (kept.len() + 1).saturating_sub(MOST_KEPT);
    for (_, name) in kept {
        if over == 0 {
            break;
        }
        if !made(&name) && remove_named(folder.as_fd(), &name).is_ok() {
            over -= 1;
        }
    }
}

/// When the lesson's folder `name` in `folder` last changed: the newest of
/// its own modification time and those of what's in it, of the first
/// [`MOST_LOOKED_AT`]. `None` if it's no folder, or a link.
fn changed(folder: &OwnedFd, name: &str) -> Option<SystemTime> {
    let inner = open_dir(folder, name).ok()?;
    let time = |stat: rustix::fs::Stat| {
        SystemTime::UNIX_EPOCH
            + Duration::from_secs(u64::try_from(stat.st_mtime).unwrap_or_default())
    };
    let mut newest = time(rustix::fs::fstat(&inner).ok()?);
    let mut listing = Dir::read_from(&inner).ok()?;
    listing.rewind();
    for entry in listing.take(MOST_LOOKED_AT) {
        let Ok(entry) = entry else {
            break;
        };
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        if let Ok(stat) = rustix::fs::statat(&inner, name, AtFlags::SYMLINK_NOFOLLOW) {
            newest = newest.max(time(stat));
        }
    }
    Some(newest)
}

/// `name`, if it's a plain file name: not empty, with no `/`, and not
/// starting with `.`, so it's neither `.` nor `..`, nor hidden.
fn plain(name: &str) -> Result<(), String> {
    let plain = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && !name.contains(['/', '\0']);
    if plain {
        Ok(())
    } else {
        Err(format!("‘{}’ isn't a plain file name", name.escape_debug()))
    }
}

/// Removes `name` in `dir`, as [`remove_at`] does.
fn remove_named(dir: BorrowedFd<'_>, name: &str) -> io::Result<()> {
    let name = CString::new(name).map_err(io::Error::other)?;
    remove_at(dir, &name, 0)
}

/// Removes `name` in `dir`, and everything in it if it's a folder, `depth`
/// folders down from where removing started, never following a link:
/// each folder is opened from the one it's in, never by its path, and a
/// link is removed as itself. What isn't there is no mistake.
fn remove_at(dir: BorrowedFd<'_>, name: &CStr, depth: usize) -> io::Result<()> {
    match rustix::fs::unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => return Ok(()),
        Err(Errno::ISDIR) => {}
        Err(err) => return Err(err.into()),
    }
    if depth >= DEEPEST {
        return Err(io::Error::other("its folders go too deep"));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let inner = rustix::fs::openat(dir, name, flags, Mode::empty())?;
    loop {
        let names = some_names(&inner)?;
        if names.is_empty() {
            break;
        }
        for child in &names {
            remove_at(inner.as_fd(), child, depth + 1)?;
        }
    }
    match rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Some of the names in the folder `dir`, from its start: up to 256, and
/// none if it's empty.
fn some_names(dir: &OwnedFd) -> io::Result<Vec<CString>> {
    let mut listing = Dir::read_from(dir)?;
    // The folder may have been read before: its offset is shared.
    listing.rewind();
    let mut names = Vec::new();
    for entry in listing {
        let entry = entry?;
        let name = entry.file_name();
        if name != c"." && name != c".." {
            names.push(name.to_owned());
        }
        if names.len() == 256 {
            break;
        }
    }
    Ok(names)
}

/// The folder `name` in `dir`, open, never through a link.
fn open_dir(dir: &OwnedFd, name: &str) -> io::Result<OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(rustix::fs::openat(dir, name, flags, Mode::empty())?)
}

/// Writes `text` to a new file `name` in `dir`, for the user alone: never
/// over anything there, nor through a link.
fn write_new(dir: &OwnedFd, name: &str, bytes: &[u8]) -> io::Result<()> {
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let made = rustix::fs::openat(dir, name, flags, Mode::RUSR | Mode::WUSR)?;
    File::from(made).write_all(bytes)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    /// The folder for lessons in a temporary folder, as a daemon's whose
    /// socket is there.
    fn lessons(dir: &Path) -> Lessons {
        let path = dir.join("tutor");
        let folder = open_folder(&path).unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        Lessons { path, folder }
    }

    /// A daemon in `dir`, its socket there, not running.
    fn daemon(dir: &Path) -> Daemon {
        crate::server::agent_files::tests::daemon_with_chat(dir).0
    }

    /// A client of `daemon`'s, greeted, over a socket pair whose other end
    /// is returned, to be kept open.
    fn client(daemon: &mut Daemon) -> (Token, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let mut stream = mio::net::UnixStream::from_std(ours);
        let token = Token(daemon.sources.vacant_key());
        (daemon.poll.registry())
            .register(&mut stream, token, mio::Interest::READABLE)
            .unwrap();
        let mut client = crate::server::Client::new(crate::server::Link::Unix(stream));
        client.greeted = true;
        daemon.sources.insert(Source::Client(client));
        (token, theirs)
    }

    /// Makes the lesson `basics`'s files, as the client at `token` asks.
    fn make(daemon: &mut Daemon, token: Token) -> Result<Response, Error> {
        let make = TutorFiles::Make {
            lesson: "basics".into(),
            text: "# Basics\n".into(),
            files: Vec::new(),
            images: Vec::new(),
        };
        daemon.tutor_files(token, make)
    }

    #[test]
    fn a_daemon_relaying_makes_no_lessons_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon(dir.path());
        let (window, _window) = client(&mut daemon);
        assert!(matches!(make(&mut daemon, window), Ok(Response::Text(_))));
        let (relaying, _relaying) = client(&mut daemon);
        let Some(Source::Client(client)) = daemon.sources.get_mut(relaying.0) else {
            unreachable!()
        };
        client.relays = true;
        let Err(Error::Other(why)) = make(&mut daemon, relaying) else {
            panic!("a relaying daemon made a lesson's files");
        };
        assert!(why.contains("own daemon"), "{why}");
        let remove = TutorFiles::Remove {
            dir: dir
                .path()
                .join("tutor/20261109-101500-1")
                .display()
                .to_string(),
        };
        assert!(daemon.tutor_files(relaying, remove).is_err());
        let made = std::fs::read_dir(dir.path().join("tutor")).unwrap().count();
        assert_eq!(made, 1);
    }

    #[test]
    fn only_the_window_that_made_a_lessons_folder_writes_it_again_or_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon(dir.path());
        let (maker, _maker) = client(&mut daemon);
        let (other, _other) = client(&mut daemon);
        let Ok(Response::Text(made)) = make(&mut daemon, maker) else {
            panic!("no folder made");
        };
        let rewrite = || TutorFiles::Rewrite {
            dir: made.clone(),
            lesson: "basics".into(),
            text: "# Again\n".into(),
        };
        let remove = || TutorFiles::Remove { dir: made.clone() };
        assert!(daemon.tutor_files(other, rewrite()).is_err());
        assert!(daemon.tutor_files(other, remove()).is_err());
        assert!(Path::new(&made).is_dir());
        let text = || std::fs::read_to_string(Path::new(&made).join("basics.txt")).unwrap();
        assert_eq!(text(), "# Basics\n");
        assert_eq!(daemon.tutor_files(maker, rewrite()), Ok(Response::Done));
        assert_eq!(text(), "# Again\n");
        // Its maker gone, as on a reconnection, it's no one's: left to be
        // pruned.
        daemon.tutor.forget(maker);
        assert!(daemon.tutor_files(maker, remove()).is_err());
        let Ok(Response::Text(again)) = make(&mut daemon, maker) else {
            panic!("no folder made");
        };
        assert!(
            daemon.tutor_files(maker, remove()).is_err(),
            "still no one's"
        );
        assert_eq!(
            daemon.tutor_files(maker, TutorFiles::Remove { dir: again.clone() }),
            Ok(Response::Done)
        );
        // Removed by a thread of its own.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while Path::new(&again).exists() {
            assert!(std::time::Instant::now() < deadline, "never removed");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_lesson_is_made_in_a_new_folder_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let lessons = lessons(dir.path());
        let files = [("a.txt".to_owned(), "one\n".to_owned())];
        let (name, made) = lessons.make("basics", "# Basics\n", &files).unwrap();
        let made = Path::new(&made);
        assert_eq!(made.parent(), Some(lessons.path.as_path()));
        assert_eq!(lessons.made(made.to_str().unwrap()), Ok(name.as_str()));
        let read = |path: PathBuf| std::fs::read_to_string(path).unwrap();
        assert_eq!(read(made.join("basics.txt")), "# Basics\n");
        assert_eq!(read(made.join("basics/a.txt")), "one\n");
        let (_, again) = lessons.make("basics", "# Basics\n", &files).unwrap();
        assert_ne!(Path::new(&again), made, "each is a folder of its own");
        // The user's alone.
        let bits = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(bits(&lessons.path), 0o700);
        assert_eq!(bits(made), 0o700);
        assert_eq!(bits(&made.join("basics/a.txt")), 0o600);
    }

    #[test]
    fn only_plain_names_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let lessons = lessons(dir.path());
        let file = |name: &str| vec![(name.to_owned(), "x\n".to_owned())];
        for name in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "/etc/passwd",
            ".hidden",
            "a\0b",
        ] {
            assert!(lessons.make("basics", "", &file(name)).is_err(), "{name:?}");
            assert!(lessons.make(name, "", &[]).is_err(), "{name:?}");
        }
        // Nothing was made for any of them.
        assert_eq!(std::fs::read_dir(&lessons.path).unwrap().count(), 0);
        assert!(!dir.path().join("x").exists());
    }

    #[test]
    fn only_a_lessons_folder_is_written_again_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let lessons = lessons(dir.path());
        let (name, made) = lessons.make("basics", "# Basics\n", &[]).unwrap();
        lessons.rewrite(&name, "basics", "# Again\n").unwrap();
        let text = std::fs::read_to_string(Path::new(&made).join("basics.txt")).unwrap();
        assert_eq!(text, "# Again\n");
        // Elsewhere, or a folder of another name, is refused.
        let outside = dir.path().join("20261109-101500-1");
        std::fs::create_dir(&outside).unwrap();
        let other = lessons.path.join("mine");
        std::fs::create_dir(&other).unwrap();
        for dir in [outside.to_str().unwrap(), other.to_str().unwrap(), "/"] {
            assert!(lessons.made(dir).is_err(), "{dir}");
        }
        let traversal = format!("{made}/../{name}");
        assert!(lessons.made(&traversal).is_err());
        assert!(lessons.rewrite(&name, "../basics", "x").is_err());
        assert!(outside.is_dir() && other.is_dir());
        remove_named(lessons.folder.as_fd(), &name).unwrap();
        assert!(!Path::new(&made).exists());
        // Gone already: nothing to do.
        remove_named(lessons.folder.as_fd(), &name).unwrap();
    }

    #[test]
    fn writing_a_lessons_text_again_makes_no_other_file() {
        let dir = tempfile::tempdir().unwrap();
        let lessons = lessons(dir.path());
        let (name, made) = lessons.make("basics", "# Basics\n", &[]).unwrap();
        let listed = || {
            let mut names: Vec<String> = (std::fs::read_dir(&made).unwrap())
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        };
        let before = listed();
        assert_eq!(before, ["basics", "basics.txt"]);
        for lesson in ["wrap", "basics.txt", "basics/notes"] {
            assert!(lessons.rewrite(&name, lesson, "x").is_err(), "{lesson}");
        }
        // Its practice folder isn't its text either.
        std::fs::rename(Path::new(&made).join("basics.txt"), dir.path().join("t")).unwrap();
        std::fs::create_dir(Path::new(&made).join("basics.txt")).unwrap();
        assert!(lessons.rewrite(&name, "basics", "x").is_err());
        std::fs::remove_dir(Path::new(&made).join("basics.txt")).unwrap();
        std::fs::rename(dir.path().join("t"), Path::new(&made).join("basics.txt")).unwrap();
        lessons.rewrite(&name, "basics", "# Again\n").unwrap();
        assert_eq!(listed(), before);
    }

    #[test]
    fn a_lessons_folder_is_removed_through_the_folder_it_is_in_never_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let lessons = lessons(dir.path());
        let precious = dir.path().join("precious");
        std::fs::create_dir(&precious).unwrap();
        std::fs::write(precious.join("mine"), "mine\n").unwrap();
        // A link in a lesson's folder is removed, not what it names; and
        // folders in it go, but no deeper than the deepest.
        let (name, made) = lessons.make("basics", "", &[]).unwrap();
        let practice = Path::new(&made).join("basics");
        std::os::unix::fs::symlink(&precious, practice.join("link")).unwrap();
        std::fs::create_dir_all(practice.join("a/b/c")).unwrap();
        std::fs::write(practice.join("a/b/c/d"), "").unwrap();
        remove_named(lessons.folder.as_fd(), &name).unwrap();
        assert!(!Path::new(&made).exists());
        assert!(precious.join("mine").is_file());
        // The folder swapped for a link to another: only the link goes.
        let (name, made) = lessons.make("basics", "", &[]).unwrap();
        std::fs::rename(&made, dir.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(&precious, &made).unwrap();
        remove_named(lessons.folder.as_fd(), &name).unwrap();
        assert!(!Path::new(&made).exists() && !Path::new(&made).is_symlink());
        assert!(precious.join("mine").is_file());
        // The folder they're all in swapped for a link, once open: what's
        // removed is in the folder open, not where the link goes.
        let (name, _) = lessons.make("basics", "", &[]).unwrap();
        let victim = dir.path().join("victim");
        std::fs::create_dir_all(victim.join(&name)).unwrap();
        std::fs::write(victim.join(&name).join("mine"), "mine\n").unwrap();
        std::fs::rename(&lessons.path, dir.path().join("away")).unwrap();
        std::os::unix::fs::symlink(&victim, &lessons.path).unwrap();
        remove_named(lessons.folder.as_fd(), &name).unwrap();
        assert!(victim.join(&name).join("mine").is_file());
        assert!(!dir.path().join("away").join(&name).exists());
        // Too deep to remove: left, and said.
        std::fs::remove_file(&lessons.path).unwrap();
        std::fs::rename(dir.path().join("away"), &lessons.path).unwrap();
        let (name, made) = lessons.make("basics", "", &[]).unwrap();
        let mut deep = Path::new(&made).join("basics");
        for _ in 0..=DEEPEST {
            deep.push("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        assert!(remove_named(lessons.folder.as_fd(), &name).is_err());
        assert!(Path::new(&made).is_dir());
    }

    #[test]
    fn nothing_is_written_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let lessons = lessons(dir.path());
        let (name, made) = lessons.make("basics", "# Basics\n", &[]).unwrap();
        let made = Path::new(&made);
        // A link where its new text is written first.
        let target = dir.path().join("precious");
        std::fs::write(&target, "mine\n").unwrap();
        std::os::unix::fs::symlink(&target, made.join(".basics.txt.new")).unwrap();
        lessons.rewrite(&name, "basics", "# Again\n").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine\n");
        // The lesson's text a link to a file of the user's: it isn't the
        // lesson's text any more.
        std::fs::remove_file(made.join("basics.txt")).unwrap();
        std::os::unix::fs::symlink(&target, made.join("basics.txt")).unwrap();
        assert!(lessons.rewrite(&name, "basics", "# Again\n").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine\n");
        assert!(made.join("basics.txt").is_symlink());
        // A lesson's folder that's a link isn't one.
        let linked = "20261109-101500-9";
        std::os::unix::fs::symlink(dir.path(), lessons.path.join(linked)).unwrap();
        std::fs::write(dir.path().join("x.txt"), "mine\n").unwrap();
        assert!(lessons.rewrite(linked, "x", "x").is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("x.txt")).unwrap(),
            "mine\n"
        );
        // Nor is the folder for them.
        let elsewhere = tempfile::tempdir().unwrap();
        let link = elsewhere.path().join("tutor");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert!(open_folder(&link).is_err());
    }

    /// Makes the lesson's folder at `path`, and what's in it at its top,
    /// look last changed `ago`.
    fn changed_ago(path: &str, ago: Duration) {
        let when = SystemTime::now() - ago;
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap().path();
            File::open(&entry).unwrap().set_modified(when).unwrap();
        }
        File::open(path).unwrap().set_modified(when).unwrap();
    }

    /// Whether the lesson's folder at `path` is still there: not one made
    /// again since, of the same name, as it's known by a file in it.
    fn still(path: &str) -> bool {
        Path::new(path).join("basics.txt").exists()
    }

    #[test]
    fn old_lessons_go_and_the_oldest_past_the_most_kept() {
        let dir = tempfile::tempdir().unwrap();
        let lessons = lessons(dir.path());
        let made: Vec<String> = (0..MOST_KEPT)
            .map(|_| lessons.make("basics", "", &[]).unwrap().1)
            .collect();
        // Room for one more: the oldest goes.
        changed_ago(&made[3], Duration::from_mins(1));
        prune(&lessons.folder, SystemTime::now(), |_| false);
        assert!(!still(&made[3]));
        assert_eq!(
            made.iter().filter(|made| still(made)).count(),
            MOST_KEPT - 1
        );
        // One past its time goes, whatever else there is.
        changed_ago(&made[5], KEPT_FOR + Duration::from_mins(1));
        prune(&lessons.folder, SystemTime::now(), |_| false);
        assert!(!still(&made[5]));
        assert_eq!(
            made.iter().filter(|made| still(made)).count(),
            MOST_KEPT - 2
        );
        // Nothing not named as a lesson's is touched.
        let mine = lessons.path.join("mine");
        std::fs::create_dir(&mine).unwrap();
        changed_ago(mine.to_str().unwrap(), KEPT_FOR * 2);
        for _ in 0..MOST_KEPT {
            lessons.make("basics", "", &[]).unwrap();
            prune(&lessons.folder, SystemTime::now(), |_| false);
        }
        assert!(mine.is_dir());
    }

    #[test]
    fn a_lesson_practised_in_lately_isnt_old() {
        let dir = tempfile::tempdir().unwrap();
        let lessons = lessons(dir.path());
        let (_, made) = lessons.make("basics", "", &[]).unwrap();
        changed_ago(&made, KEPT_FOR + Duration::from_mins(1));
        // A file made in its practice folder, as by its shell.
        let practice = Path::new(&made).join("basics");
        std::fs::write(practice.join("new.txt"), "").unwrap();
        prune(&lessons.folder, SystemTime::now(), |_| false);
        assert!(still(&made));
        changed_ago(&made, KEPT_FOR + Duration::from_mins(1));
        prune(&lessons.folder, SystemTime::now(), |_| false);
        assert!(!still(&made));
    }

    #[test]
    fn a_lesson_whose_window_is_connected_is_never_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let lessons = lessons(dir.path());
        let made: Vec<(String, String)> = (0..MOST_KEPT + 2)
            .map(|_| lessons.make("basics", "", &[]).unwrap())
            .collect();
        for (_, path) in &made {
            changed_ago(path, KEPT_FOR + Duration::from_mins(1));
        }
        let (kept, _) = &made[0];
        prune(&lessons.folder, SystemTime::now(), |name| name == kept);
        assert!(still(&made[0].1));
        assert!(made[1..].iter().all(|(_, path)| !still(path)));
        // However many there are.
        let made: Vec<(String, String)> = (0..MOST_KEPT + 2)
            .map(|_| lessons.make("basics", "", &[]).unwrap())
            .collect();
        prune(&lessons.folder, SystemTime::now(), |_| true);
        assert!(made.iter().all(|(_, path)| still(path)));
    }

    #[test]
    fn a_window_connected_keeps_its_lessons_from_pruning_and_its_removal_is_on_a_thread() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon(dir.path());
        let (window, _window) = client(&mut daemon);
        let (gone, _gone) = client(&mut daemon);
        let Ok(Response::Text(theirs)) = make(&mut daemon, gone) else {
            panic!("no folder made");
        };
        let Ok(Response::Text(mine)) = make(&mut daemon, window) else {
            panic!("no folder made");
        };
        daemon.tutor.forget(gone);
        for path in [&theirs, &mine] {
            changed_ago(path, KEPT_FOR + Duration::from_mins(1));
        }
        // Pruned, by the thread, as another is made: but never the
        // folder of a window still connected.
        let Ok(Response::Text(another)) = make(&mut daemon, window) else {
            panic!("no folder made");
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while still(&theirs) {
            assert!(std::time::Instant::now() < deadline, "never pruned");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(still(&mine) && still(&another));
        let remove = TutorFiles::Remove { dir: mine.clone() };
        assert_eq!(daemon.tutor_files(window, remove), Ok(Response::Done));
        while still(&mine) {
            assert!(std::time::Instant::now() < deadline, "never removed");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(still(&another));
    }
}
