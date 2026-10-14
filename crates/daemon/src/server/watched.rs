//! Files watched for language servers: telling each server of changes on
//! disk to the files it asked to hear of, with LSP's
//! `workspace/didChangeWatchedFiles`, so what it says of files not open,
//! changed by git, an agent or another editor, doesn't go stale.
//!
//! A server asks with `client/registerCapability`, giving glob patterns,
//! each matched from a directory, its base (see [`lsp::watchers`]). Each
//! base is watched with inotify, on the instance and under the watches
//! explorers use (see [`super::explorers`]); for a pattern that can match
//! below its base, so is every directory in it, found by a walk on a
//! thread, but git's own, those `.gitignore` leaves out, and those builds
//! and package managers fill, as `target` and `node_modules`, unless a
//! base is in one. A directory made later in one is walked as it's heard
//! of, and walked again once what the walk found is watched, as files may
//! have been made in it before it was, which are told of as made. A
//! directory moved away or deleted is no longer watched where it was, and
//! told of as deleted, with the files in it the server has said are wrong.
//!
//! Nothing outside a server's project is watched for it, whatever its
//! patterns say: a base is taken as it really is, links and `..` followed,
//! before it's looked at. Servers have [`MAX_SERVER_WATCHES`] directories
//! between them, each an equal share of them; past its share, no more are
//! watched for a server, and the log says so.
//!
//! A file made, or moved in, is told as created; one written, as changed;
//! one deleted, or moved away, as deleted. A file written by renaming a
//! new one over it, as most programs write files, is heard of as made.
//! Changes heard of within [`GATHER`] of the first go in one notification,
//! each file once, as what its changes come to.
//!
//! A file open in a buffer in the server isn't told of. The server has its
//! text from the buffer, which told it of every edit and save, the
//! daemon's own included, and a change from outside is taken into the
//! buffer as `autoread` says, and told as an edit. LSP lets a client tell
//! it anyway; it would only have the server read from disk what it
//! doesn't use.
//!
//! A server that asks to hear of nothing is still told of changes to the
//! files not open it has said are wrong, so what it says of them can be
//! said again: their directories are watched for it, as it says of more,
//! until it asks to hear of something itself, or stops.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::inotify::ReadFlags;
use serde_json::Value;

use super::Daemon;
use super::explorers::{MAX_SERVER_WATCHES, MAX_WATCHES};
use super::language::ServerId;
use crate::explorer::{Walk, churns};
use crate::lsp::{self, FileChange, Watcher};

/// How long changes are gathered, from the first, before a server is told
/// of them: a flood of them, as a branch switch makes, goes as a few
/// notifications, not thousands.
const GATHER: Duration = Duration::from_millis(100);

/// What a server has asked to hear of, and what it's to be told.
#[derive(Debug, Default)]
pub(super) struct Wanted {
    /// What it asked to hear of, by the ID of each of its registrations.
    registered: Vec<(String, Vec<Watcher>)>,
    /// The directories watched for it.
    watching: HashSet<PathBuf>,
    /// Changes heard of and not yet told, by file.
    pending: BTreeMap<PathBuf, FileChange>,
    /// When they're to be told.
    due: Option<Instant>,
    /// It's been logged that there's no room to watch more for it.
    full: bool,
}

impl Wanted {
    /// Notes `change` to the file at `path`, to tell in a while.
    fn note(&mut self, path: PathBuf, change: FileChange) {
        (self.pending.entry(path))
            .and_modify(|noted| *noted = noted.then(change))
            .or_insert(change);
        self.due.get_or_insert_with(|| Instant::now() + GATHER);
    }

    /// Whether it wants to hear of `change` to the file at `path`, as one
    /// of its registrations says.
    fn wants(&self, path: &Path, change: FileChange) -> bool {
        (self.registered.iter())
            .flat_map(|(_, watchers)| watchers)
            .any(|watcher| watcher.wants(path, change))
    }

    /// Whether a pattern of its can match files in the directory at `dir`
    /// and those below it, and so a directory made there is walked.
    fn wants_below(&self, dir: &Path) -> bool {
        (self.registered.iter())
            .flat_map(|(_, watchers)| watchers)
            .any(|watcher| watcher.deep && dir.starts_with(&watcher.base))
    }

    /// Whether a pattern's base is the directory at `dir`, or in it, and
    /// so names it: then it's walked, though it's one [`churns`] names.
    fn names(&self, dir: &Path) -> bool {
        (self.registered.iter())
            .flat_map(|(_, watchers)| watchers)
            .any(|watcher| watcher.base.starts_with(dir))
    }

    /// Everything its registrations ask for.
    fn watchers(&self) -> Vec<Watcher> {
        (self.registered.iter())
            .flat_map(|(_, watchers)| watchers.iter().cloned())
            .collect()
    }
}

/// A walk of a directory to watch for a server.
#[derive(Debug)]
pub(super) struct Walking {
    server: ServerId,
    /// The directory was made just now, so the files found in it are told
    /// as made.
    made: bool,
    /// It's to be walked once more once what it found is watched: what
    /// was made in it after the walk read it, and before it was watched,
    /// wasn't heard of either way.
    again: bool,
}

impl Daemon {
    /// Has the files server `server`'s registration `id` asks for in
    /// `options`, a `DidChangeWatchedFilesRegistrationOptions`, watched,
    /// in place of what that registration asked for before.
    pub(super) fn watch_for_server(&mut self, server: ServerId, id: String, options: &Value) {
        let home = self.config.home.clone();
        let Some(slot) = self.servers.slots.get_mut(&server) else {
            return;
        };
        let name = slot.command.join(" ");
        let root = std::fs::canonicalize(&slot.root).unwrap_or_else(|_| slot.root.clone());
        let watchers: Vec<Watcher> = (lsp::watchers(options, &root).into_iter())
            .filter_map(|mut watcher| {
                let base = real(&watcher.base).filter(|base| base.starts_with(&root));
                // A whole home is far too much, were it a project.
                let base = base.filter(|base| !(watcher.deep && home.starts_with(base)));
                let Some(base) = base else {
                    let asked = watcher.base.display();
                    log::info!("{name}: not watching {asked} for it, outside its project");
                    return None;
                };
                watcher.base = base;
                Some(watcher)
            })
            .collect();
        log::info!("{name}: watching {} patterns, as {id}", watchers.len());
        let first = slot.wanted.registered.is_empty();
        let again = (slot.wanted.registered.iter()).any(|(had, _)| *had == id);
        slot.wanted.registered.retain(|(had, _)| *had != id);
        slot.wanted.registered.push((id, watchers.clone()));
        if first || again {
            // What was watched for it till now, the files it said were
            // wrong, or what this registration asked for before, goes.
            let all = slot.wanted.watchers();
            self.stop_watching_for(server);
            self.watch_bases(server, &all);
        } else {
            self.watch_bases(server, &watchers);
        }
    }

    /// Stops watching what server `server`'s registration `id` asked for.
    /// What its other registrations ask for is watched afresh, or, if it
    /// has none left, the files it says are wrong.
    pub(super) fn unwatch_for_server(&mut self, server: ServerId, id: &str) {
        let Some(slot) = self.servers.slots.get_mut(&server) else {
            return;
        };
        let had = slot.wanted.registered.len();
        slot.wanted
            .registered
            .retain(|(registered, _)| registered != id);
        if slot.wanted.registered.len() == had {
            return;
        }
        let left = slot.wanted.watchers();
        self.stop_watching_for(server);
        if left.is_empty() {
            let diagnosed: Vec<PathBuf> = (self.servers.elsewhere.iter())
                .filter(|(_, (said, _))| *said == server)
                .map(|(path, _)| path.clone())
                .collect();
            for path in diagnosed {
                self.watch_diagnosed(server, &path);
            }
        } else {
            self.watch_bases(server, &left);
        }
    }

    /// Watches the directory of the file at `path`, not open, which server
    /// `server` has said is wrong, if the server asks to hear of nothing
    /// itself, so it can be told when the file changes.
    pub(super) fn watch_diagnosed(&mut self, server: ServerId, path: &Path) {
        let registered =
            (self.servers.slots.get(&server)).is_none_or(|slot| !slot.wanted.registered.is_empty());
        if let (false, Some(dir)) = (registered, path.parent()) {
            self.watch_dir_for(server, dir);
        }
    }

    /// No longer watches the directory of the file at `path` for server
    /// `server`, which has stopped saying the file is wrong, if it was
    /// watched for files it said were wrong, and no other such file is in
    /// it.
    pub(super) fn unwatch_undiagnosed(&mut self, server: ServerId, path: &Path) {
        let Some(dir) = path.parent() else { return };
        let servers = &mut self.servers;
        let Some(slot) = servers.slots.get_mut(&server) else {
            return;
        };
        let other = (servers.elsewhere.iter())
            .any(|(file, (said, _))| *said == server && file.parent() == Some(dir));
        if !slot.wanted.registered.is_empty() || other || !slot.wanted.watching.remove(dir) {
            return;
        }
        self.remove_server_watches(|watcher, watched| watcher == server && watched == dir);
    }

    /// No longer watches anything for server `server`, which has stopped
    /// or is stopping.
    pub(super) fn unwatch_files_for(&mut self, server: ServerId) {
        self.remove_server_watches(|watcher, _| watcher == server);
        self.servers
            .walks
            .retain(|_, walking| walking.server != server);
    }

    /// No longer watches anything for server `server`, to watch what it
    /// asks for afresh.
    fn stop_watching_for(&mut self, server: ServerId) {
        if let Some(slot) = self.servers.slots.get_mut(&server) {
            slot.wanted.watching.clear();
        }
        self.unwatch_files_for(server);
    }

    /// Watches the bases of `watchers` for server `server`: each base of a
    /// pattern that can match below it with every directory in it, walked,
    /// and each other base by itself. A base inside another is walked or
    /// watched too, as a walk of the outer may pass over what it's in, as
    /// `target`; what's watched already costs nothing more.
    fn watch_bases(&mut self, server: ServerId, watchers: &[Watcher]) {
        let mut deep: Vec<&Path> = (watchers.iter())
            .filter(|watcher| watcher.deep)
            .map(|watcher| watcher.base.as_path())
            .collect();
        deep.sort_unstable();
        deep.dedup();
        for watcher in watchers.iter().filter(|watcher| !watcher.deep) {
            self.watch_dir_for(server, &watcher.base);
        }
        for base in deep {
            self.walk_for(server, base.to_owned(), false);
        }
    }

    /// Watches everything every server's registrations ask for again, as
    /// changes heard of were lost, so a directory made meanwhile wasn't
    /// heard of: what's watched already costs nothing more. A server with
    /// a walk still to come back is passed over, so overflow after
    /// overflow doesn't stack walks up.
    fn watch_for_servers_again(&mut self) {
        let walking: BTreeSet<ServerId> = (self.servers.walks.values())
            .map(|walking| walking.server)
            .collect();
        let wanted: Vec<(ServerId, Vec<Watcher>)> = (self.servers.slots.iter())
            .filter(|(id, _)| !walking.contains(id))
            .map(|(&id, slot)| (id, slot.wanted.watchers()))
            .collect();
        for (server, watchers) in wanted {
            self.watch_bases(server, &watchers);
        }
    }

    /// How many more directories may be watched for server `server`: its
    /// share of the [`MAX_SERVER_WATCHES`] servers have between them, as if
    /// there were at least two, less
    /// what's watched for it, and no more than there's room for at all.
    fn room_for(&self, server: ServerId) -> usize {
        let Some(slot) = self.servers.slots.get(&server) else {
            return 0;
        };
        // Never all of them, so a server started later has room too.
        let share = MAX_SERVER_WATCHES / self.servers.slots.len().max(2);
        (share.saturating_sub(slot.wanted.watching.len()))
            .min(MAX_SERVER_WATCHES.saturating_sub(self.server_watch_count()))
            .min(MAX_WATCHES.saturating_sub(self.watch_count()))
    }

    /// Watches the directory at `dir` for server `server`, unless there's
    /// no room. Returns whether it's watched.
    fn watch_dir_for(&mut self, server: ServerId, dir: &Path) -> bool {
        let Some(slot) = self.servers.slots.get(&server) else {
            return false;
        };
        if slot.wanted.watching.contains(dir) {
            return true;
        }
        if self.room_for(server) == 0 {
            self.no_room_for(server);
            return false;
        }
        if !self.add_server_watch(server, dir) {
            return false;
        }
        if let Some(slot) = self.servers.slots.get_mut(&server) {
            slot.wanted.watching.insert(dir.to_owned());
        }
        true
    }

    /// Logs, once for server `server`, that no more directories can be
    /// watched for it.
    fn no_room_for(&mut self, server: ServerId) {
        let servers = self.servers.slots.len();
        if let Some(slot) = self.servers.slots.get_mut(&server)
            && !slot.wanted.full
        {
            slot.wanted.full = true;
            log::warn!(
                "{}: {} directories are watched for it, all it may have of the \
                 {MAX_SERVER_WATCHES} {servers} language servers share: changes in more \
                 aren't told it",
                slot.command.join(" "),
                slot.wanted.watching.len(),
            );
        }
    }

    /// Walks the directory at `dir` on a thread, for server `server`, to
    /// watch it and those in it; told of the files in it as made, if it's
    /// been `made` just now.
    fn walk_for(&mut self, server: ServerId, dir: PathBuf, made: bool) {
        let walking = Walking {
            server,
            made,
            again: made,
        };
        self.start_walk(dir, walking);
    }

    /// Starts `walking`, a walk of the directory at `dir`, on a thread,
    /// unless there's no room to watch more.
    fn start_walk(&mut self, dir: PathBuf, walking: Walking) {
        let room = self.room_for(walking.server);
        if room == 0 {
            return self.no_room_for(walking.server);
        }
        let ticket = self.servers.next_walk;
        let Some(workers) = self.files_workers() else {
            return;
        };
        workers.walk(ticket, dir, walking.made, room);
        self.servers.next_walk += 1;
        self.servers.walks.insert(ticket, walking);
    }

    /// Takes what the walk numbered `ticket` found: its directories are
    /// watched, as long as there's room, and the files in one just made
    /// are told of as made, if they're wanted; then, for one made, it's
    /// walked once more, for what was made in it meanwhile.
    pub(super) fn walked(&mut self, ticket: u64, walk: Walk) {
        let Some(walking) = self.servers.walks.remove(&ticket) else {
            // For a server since stopped, or watching afresh.
            return;
        };
        let server = walking.server;
        for dir in &walk.dirs {
            if !self.watch_dir_for(server, dir) && self.room_for(server) == 0 {
                break;
            }
        }
        if walk.cut_short {
            self.no_room_for(server);
        }
        if !walking.made {
            return;
        }
        for file in walk.files {
            self.heard_for_server(server, file, FileChange::Created);
        }
        // The walk's first directory is the one it walked, unless it was
        // passed over, as one git ignores.
        if let (true, Some(dir)) = (walking.again, walk.dirs.first()) {
            let walking = Walking {
                again: false,
                ..walking
            };
            self.start_walk(dir.clone(), walking);
        }
    }

    /// Takes what inotify said happened in the directories watched for
    /// servers, `served`: each server, the path of what it happened to, and
    /// what happened. With `everything`, some changes were lost, which no
    /// notification can say, but what's watched is watched again, for the
    /// directories made meanwhile.
    pub(super) fn heard_for_servers(
        &mut self,
        served: Vec<(ServerId, PathBuf, ReadFlags)>,
        everything: bool,
    ) {
        if everything && !self.servers.slots.is_empty() {
            log::info!("changes to files were lost: language servers may not hear of some");
            self.watch_for_servers_again();
        }
        for (server, path, flags) in served {
            let change = if flags.intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO) {
                FileChange::Created
            } else if flags.intersects(ReadFlags::DELETE | ReadFlags::MOVED_FROM) {
                FileChange::Deleted
            } else if flags.contains(ReadFlags::CLOSE_WRITE) {
                FileChange::Changed
            } else {
                // Its mode or times, which no server reads.
                continue;
            };
            if flags.contains(ReadFlags::ISDIR) {
                let Some(name) = path.file_name() else {
                    continue;
                };
                if name == ".git" {
                    continue;
                }
                if change == FileChange::Deleted {
                    self.server_dir_gone(server, &path);
                    continue;
                }
                let walked = (self.servers.slots.get(&server)).is_some_and(|slot| {
                    slot.wanted.wants_below(&path) && (!churns(name) || slot.wanted.names(&path))
                });
                if change == FileChange::Created && walked {
                    self.walk_for(server, path.clone(), true);
                }
            }
            self.heard_for_server(server, path, change);
        }
    }

    /// The directory at `dir` has gone from where server `server` watched
    /// it, deleted or moved: it's no longer watched there, nor what's in
    /// it, and the server is told it's deleted, if a pattern of its can
    /// match below it, with each file in it the server said was wrong
    /// that it wants to hear of: the files it holds that no one said were
    /// wrong aren't known, and a server that cares is told of the
    /// directory.
    fn server_dir_gone(&mut self, server: ServerId, dir: &Path) {
        self.server_dirs_gone(&[(server, dir.to_owned())]);
        let servers = &mut self.servers;
        let Some(slot) = servers.slots.get_mut(&server) else {
            return;
        };
        let registered = !slot.wanted.registered.is_empty();
        if registered && slot.wanted.wants_below(dir) {
            slot.wanted.note(dir.to_owned(), FileChange::Deleted);
        }
        let files = (servers.elsewhere.iter())
            .filter(|(file, (said, _))| *said == server && file.starts_with(dir))
            .map(|(file, _)| file)
            .filter(|file| !registered || slot.wanted.wants(file, FileChange::Deleted));
        let files: Vec<PathBuf> = files.cloned().collect();
        for file in files {
            slot.wanted.note(file, FileChange::Deleted);
        }
    }

    /// Notes that the directories in `gone`, each with the server it was
    /// watched for, have moved or been deleted: they're no longer watched
    /// for it where they were, nor what was in them.
    pub(super) fn server_dirs_gone(&mut self, gone: &[(ServerId, PathBuf)]) {
        if gone.is_empty() {
            return;
        }
        for (server, dir) in gone {
            if let Some(slot) = self.servers.slots.get_mut(server) {
                slot.wanted
                    .watching
                    .retain(|watched| !watched.starts_with(dir));
            }
        }
        self.remove_server_watches(|server, watched| {
            (gone.iter()).any(|(from, dir)| *from == server && watched.starts_with(dir))
        });
    }

    /// Notes `change` to the file at `path` for server `server`, if it
    /// wants to hear of it: as its patterns say, or, if it has asked to
    /// hear of nothing, if it has said the file is wrong.
    fn heard_for_server(&mut self, server: ServerId, path: PathBuf, change: FileChange) {
        let servers = &mut self.servers;
        let Some(slot) = servers.slots.get_mut(&server) else {
            return;
        };
        let wanted = if slot.wanted.registered.is_empty() {
            (servers.elsewhere.get(&path)).is_some_and(|(said, _)| *said == server)
        } else {
            slot.wanted.wants(&path, change)
        };
        if wanted {
            slot.wanted.note(path, change);
        }
    }

    /// Notes that the directories `gone`, each watched for a server, are no
    /// longer watched, as they were deleted or moved.
    pub(super) fn server_dirs_unwatched(&mut self, gone: Vec<(ServerId, PathBuf)>) {
        for (server, dir) in gone {
            if let Some(slot) = self.servers.slots.get_mut(&server) {
                slot.wanted.watching.remove(&dir);
            }
        }
    }

    /// Tells server `id` the files at `paths`, which the daemon wrote, have
    /// changed on disk, with any changes to them heard of meanwhile.
    pub(super) fn changed_on_disk(&mut self, id: ServerId, paths: &[PathBuf]) {
        if let Some(slot) = self.servers.slots.get_mut(&id) {
            for path in paths {
                slot.wanted.note(path.clone(), FileChange::Changed);
            }
        }
    }

    /// When the next server is to be told of changes, if any is.
    pub(super) fn next_file_changes_due(&self) -> Option<Instant> {
        (self.servers.slots.values())
            .filter_map(|slot| slot.wanted.due)
            .min()
    }

    /// Tells each server whose changes are due of them, but those to files
    /// open in it.
    pub(super) fn tell_file_changes(&mut self) {
        let now = Instant::now();
        let due: Vec<ServerId> = (self.servers.slots.iter())
            .filter(|(_, slot)| slot.wanted.due.is_some_and(|due| due <= now))
            .map(|(&id, _)| id)
            .collect();
        for id in due {
            let open: HashSet<&Path> = (self.buffers.values())
                .filter(|slot| (slot.server.as_ref()).is_some_and(|opened| opened.server() == id))
                .map(|slot| slot.buffer.path())
                .collect();
            let Some(slot) = self.servers.slots.get_mut(&id) else {
                continue;
            };
            slot.wanted.due = None;
            let changes: Vec<(PathBuf, FileChange)> = std::mem::take(&mut slot.wanted.pending)
                .into_iter()
                .filter(|(path, _)| !open.contains(path.as_path()))
                .collect();
            if !changes.is_empty() {
                slot.server.changed_on_disk(&changes);
                self.flush_server(id);
            }
        }
    }
}

/// The directory at `path` as it really is, with links and `..` followed,
/// even if it isn't there yet: what's there of it, real, with the rest
/// after. `None` if what isn't there has `..` in it, which can't be told.
fn real(path: &Path) -> Option<PathBuf> {
    let mut there = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(mut real) = std::fs::canonicalize(there) {
            real.extend(rest.iter().rev());
            return Some(real);
        }
        rest.push(there.file_name()?);
        there = there.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::server::stopping::tests::{daemon_with_fake, read_servers_until};

    /// What's watched for server `server` of `daemon`: as the server's own
    /// note says, and as the watches say.
    fn watching(daemon: &Daemon, server: ServerId) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let mut noted: Vec<PathBuf> = (daemon.servers.slots[&server].wanted.watching.iter())
            .cloned()
            .collect();
        noted.sort();
        (noted, daemon.watched_for_server(server))
    }

    #[test]
    fn a_folder_watched_for_files_said_to_be_wrong_goes_once_none_are() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let other = root.join("app/other.fk");
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(&other, "bad\n").unwrap();
        let elsewhere = other.display().to_string();
        let (mut daemon, server) = daemon_with_fake(&root, &["--elsewhere", &elsewhere]);
        read_servers_until(&mut daemon, |daemon| {
            daemon.servers.elsewhere.contains_key(&other)
        });
        let app = vec![root.join("app")];
        assert_eq!(watching(&daemon, server), (app.clone(), app));

        // It says nothing's wrong there now.
        daemon.diagnosed(server, &other, None, Vec::new());
        assert_eq!(watching(&daemon, server), (vec![], vec![]));
    }

    #[test]
    fn a_registration_made_again_lets_go_of_what_it_asked_for_before() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for sub in ["app/one", "app/two"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let (mut daemon, server) = daemon_with_fake(&root, &[]);
        let asks = |dir: &str| json!({ "watchers": [{ "globPattern": format!("{dir}/*.fk") }] });
        daemon.watch_for_server(server, "w".into(), &asks("one"));
        let one = vec![root.join("app/one")];
        assert_eq!(watching(&daemon, server), (one.clone(), one));
        daemon.watch_for_server(server, "w".into(), &asks("two"));
        let two = vec![root.join("app/two")];
        assert_eq!(watching(&daemon, server), (two.clone(), two));
    }

    #[test]
    fn servers_share_the_watches_they_may_have() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (mut daemon, first) = daemon_with_fake(&root, &[]);
        // Alone, never all of them, so one started later has room.
        assert_eq!(daemon.room_for(first), MAX_SERVER_WATCHES / 2);
        let asks = json!({ "watchers": [{ "globPattern": "*.fk" }] });
        daemon.watch_for_server(first, "w".into(), &asks);
        assert_eq!(daemon.room_for(first), MAX_SERVER_WATCHES / 2 - 1);
        // It fills its share, as a big project would.
        let wanted = &mut daemon.servers.slots.get_mut(&first).unwrap().wanted;
        wanted
            .watching
            .extend((0..MAX_SERVER_WATCHES).map(|n| root.join(format!("d{n}"))));
        assert_eq!(daemon.room_for(first), 0);

        // Another project's file, and so another server, which has room.
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::write(root.join("other/project"), "").unwrap();
        std::fs::write(root.join("other/b.fk"), "bad\n").unwrap();
        let file = clackatoo_protocol::OpenFile {
            path: root.join("other/b.fk").display().to_string(),
            cwd: None,
        };
        daemon.open_buffer(file, None).unwrap();
        read_servers_until(&mut daemon, |daemon| daemon.servers.slots.len() == 2);
        let second = *daemon
            .servers
            .slots
            .keys()
            .find(|&&id| id != first)
            .unwrap();
        assert_eq!(daemon.room_for(second), MAX_SERVER_WATCHES / 2);
        daemon.watch_for_server(second, "w".into(), &asks);
        assert_eq!(daemon.watched_for_server(second), [root.join("other")]);
    }

    #[test]
    fn lost_changes_dont_stack_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (mut daemon, server) = daemon_with_fake(&root, &[]);
        let asks = json!({ "watchers": [{ "globPattern": "**/*.fk" }] });
        daemon.watch_for_server(server, "w".into(), &asks);
        assert_eq!(daemon.servers.walks.len(), 1);
        // Changes are lost, over and over, before the walk comes back.
        for _ in 0..3 {
            daemon.heard_for_servers(Vec::new(), true);
        }
        assert_eq!(daemon.servers.walks.len(), 1);
    }

    #[test]
    fn bases_are_taken_as_they_really_are() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::os::unix::fs::symlink(root.join("a"), root.join("link")).unwrap();
        assert_eq!(real(&root.join("link/b")), Some(root.join("a/b")));
        assert_eq!(real(&root.join("a/b/../..")), Some(root.clone()));
        // Not there yet: what's there, and the rest.
        assert_eq!(real(&root.join("link/new/er")), Some(root.join("a/new/er")));
        assert_eq!(real(&root.join("new/../..")), None);
    }
}
