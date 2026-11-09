//! Asking whether a folder may run its language servers (see
//! [`crate::trust`]), in the event loop: a buffer whose language's server
//! would be started for a folder not trusted gets none, and the windows
//! showing it are asked, once each for each folder; a key pressed there,
//! or the `clackatoo` command, answers with [`Request::Trust`]. Agents are
//! never asked, and can't answer: no tool of theirs trusts a folder, and
//! `open_file` in a folder not trusted shows the file with no server.
//!
//! [`Request::Trust`]: clackatoo_protocol::Request::Trust

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clackatoo_protocol::{BufferId, BufferUpdate, Error, Response, TrustAction, wire};
use mio::Token;

use super::{Daemon, Source};
use crate::trust::{Trust, Trusting};

/// The folders trusted or not, and which clients have been asked about
/// which.
#[derive(Debug)]
pub(super) struct Asking {
    pub(super) folders: Trust,
    /// Each client asked, and the folder: not asked again while it's
    /// connected, as an install isn't offered again.
    asked: HashSet<(Token, PathBuf)>,
    /// How many folders were trusted as the build before ran their
    /// servers, for the first window the user is at to say.
    pub(super) carried: usize,
}

impl Asking {
    pub(super) fn new(trusting: &Trusting) -> Self {
        Self {
            folders: Trust::load(trusting),
            asked: HashSet::new(),
            carried: 0,
        }
    }

    /// Forgets the client at `token`, as it's gone.
    pub(super) fn forget(&mut self, token: Token) {
        self.asked.retain(|(asked, _)| *asked != token);
    }
}

impl Daemon {
    /// Whether a language server may be started for the folder `root`.
    pub(super) fn trusts(&self, root: &Path) -> bool {
        self.trust.folders.trusts(root) == Some(true)
    }

    /// The folder buffer `id`'s language server would be started for, if
    /// it has a language server and that isn't trusted to run, as of the
    /// last time it was opened in one.
    pub(super) fn untrusted_root(&self, id: BufferId) -> Option<&Path> {
        let slot = self.buffers.get(&id)?;
        slot.untrusted.as_deref().filter(|_| slot.server.is_none())
    }

    /// Asks the windows following buffer `id`, or only the client at
    /// `only`, whether to trust the folder its language server would be
    /// started for, if that's not trusted, nor marked not trusted: each
    /// client once for each folder, and only a client that can answer,
    /// which a device on the web app can't.
    pub(super) fn ask_trust(&mut self, id: BufferId, only: Option<Token>) {
        let Some(root) = self.untrusted_root(id).map(Path::to_owned) else {
            return;
        };
        if self.trust.folders.trusts(&root).is_some() {
            return;
        }
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let asked: Vec<Token> = (slot.viewers.iter().copied())
            .filter(|&viewer| only.is_none_or(|only| only == viewer))
            .filter(|&viewer| {
                matches!(
                    self.sources.get(viewer.0),
                    Some(Source::Client(client)) if client.wire >= wire::TRUST
                ) && !self.is_web_device(viewer)
            })
            .collect();
        let Some(shown) = root.to_str().map(str::to_owned) else {
            // Not UTF-8, so not to be named in a question: `clackatoo
            // trust` trusts it.
            return;
        };
        for token in asked {
            if self.trust.asked.insert((token, root.clone())) {
                log::info!("buffer {id}: asking whether to trust {}", root.display());
                let root = shown.clone();
                self.tell(token, id, BufferUpdate::Trust { root });
            }
        }
    }

    /// Answers [`Request::Trust`](clackatoo_protocol::Request::Trust) from
    /// the client at `token`.
    pub(super) fn serve_trust(
        &mut self,
        token: Token,
        action: TrustAction,
        folder: Option<&str>,
        buffer: Option<BufferId>,
    ) -> Result<Response, Error> {
        if action == TrustAction::List {
            return Ok(Response::Text(self.trusted_list()));
        }
        // What runs a project's code is for the user to allow, not a phone.
        if action == TrustAction::Trust && self.is_web_device(token) {
            return Err(Error::Other(
                "a device on the web app can't trust a folder".into(),
            ));
        }
        let folder = self.trust_folder(folder, buffer)?;
        let shown = super::tools::homely(&folder, &self.config.home);
        let trusted = action == TrustAction::Trust;
        if !self.trust.folders.mark(folder.clone(), trusted) {
            let was = if trusted { "trusted" } else { "not trusted" };
            return Ok(Response::Text(format!("{shown} was {was} already")));
        }
        log::info!(
            "{} is {}",
            folder.display(),
            if trusted { "trusted" } else { "not trusted" }
        );
        if trusted {
            // The files of it open, and shown, have their servers now.
            let waiting: Vec<BufferId> = (self.buffers.iter())
                .filter(|(_, slot)| slot.server.is_none() && slot.untrusted.is_some())
                .map(|(&id, _)| id)
                .collect();
            for id in waiting {
                self.open_in_server(id);
            }
            return Ok(Response::Text(format!(
                "trusted {shown}: its language servers may run its code"
            )));
        }
        let stopped = self.stop_untrusted_servers();
        let stopped = match stopped {
            0 => String::new(),
            1 => ", and its language server stopped".to_owned(),
            n => format!(", and its {n} language servers stopped"),
        };
        Ok(Response::Text(format!(
            "{shown} isn't trusted: its language servers won't run{stopped}"
        )))
    }

    /// The folder `folder` names, from the directory `buffer`'s file was
    /// opened from, or else home; or with no `folder`, the one `buffer`'s
    /// language server would be started for. Made real, and it must be a
    /// folder.
    fn trust_folder(
        &mut self,
        folder: Option<&str>,
        buffer: Option<BufferId>,
    ) -> Result<PathBuf, Error> {
        let slot = match buffer {
            Some(id) => Some(self.buffers.get(&id).ok_or(Error::NoSuchBuffer(id))?),
            None => None,
        };
        let Some(folder) = folder else {
            let slot = slot.ok_or_else(|| {
                Error::Other("name a folder, or show a file of its project".into())
            })?;
            if slot.buffer.is_blank() {
                return Err(Error::Other(
                    "this buffer has no file yet: name a folder".into(),
                ));
            }
            let path = slot.buffer.path().to_owned();
            let spec = (self.languages.as_mut())
                .and_then(|languages| languages.find(&path))
                .and_then(|pack| pack.server.clone())
                .ok_or_else(|| {
                    Error::Other(
                        "this file's language has no language server: name a folder".into(),
                    )
                })?;
            return spec.root(&path, &self.config.home).ok_or_else(|| {
                Error::Other(
                    "this file is in no project, and gets no language server: name a folder".into(),
                )
            });
        };
        let from = slot.map_or_else(
            || self.config.home.clone(),
            |slot| slot.buffer.dir().to_owned(),
        );
        let path = if folder == "~" {
            // Made real, as every other folder is: home may be a link.
            let home = &self.config.home;
            home.canonicalize().unwrap_or_else(|_| home.clone())
        } else {
            super::buffers::resolve(folder, &from, &self.config.home)
        };
        // A stat, as opening a file is.
        if !path.is_dir() || !path.is_absolute() {
            return Err(Error::Other(format!(
                "{} isn't a folder",
                super::tools::homely(&path, &self.config.home)
            )));
        }
        Ok(path)
    }

    /// The folders marked, one a line, for `clackatoo trust --list` and
    /// `:trusted`.
    fn trusted_list(&self) -> String {
        if self.trust.folders.trusts_all() {
            return "every folder is trusted: this daemon asks about none".to_owned();
        }
        let mut list = String::new();
        for (folder, trusted) in self.trust.folders.marked() {
            let shown = super::tools::homely(folder, &self.config.home);
            let mark = if trusted { "trusted" } else { "not trusted" };
            let _ = writeln!(list, "{mark:<12}{shown}");
        }
        if list.is_empty() {
            return "no folder is trusted yet".to_owned();
        }
        list.truncate(list.trim_end().len());
        list
    }

    /// Trusts the folders the build before this one ran language servers
    /// for, if it ran them unasked, as its handover's version, `from`,
    /// says: those of the files open as it handed over, whose servers are
    /// installed, so an upgrade to a build that asks takes no server away.
    /// Only ever from such a build: a file of folders deleted, kept for a
    /// run only, or that couldn't be written, never trusts anything again.
    /// The first window the user is at is told, once.
    pub(super) fn trust_what_was_running(&mut self, from: u32) {
        if from >= crate::handover::ASKS_TRUST {
            return;
        }
        let ids: Vec<BufferId> = self.buffers.keys().copied().collect();
        for id in ids {
            let Some(slot) = self.buffers.get(&id) else {
                continue;
            };
            if slot.buffer.is_blank() {
                continue;
            }
            let path = slot.buffer.path().to_owned();
            let Some(spec) = (self.languages.as_mut())
                .and_then(|languages| languages.find(&path))
                .and_then(|pack| pack.server.clone())
            else {
                continue;
            };
            let Some(root) = spec.root(&path, &self.config.home) else {
                continue;
            };
            let program = spec.command.first().map_or("", String::as_str);
            if self.trust.folders.trusts(&root).is_none() && self.program_found(program) {
                log::info!(
                    "{} is trusted: the build before ran its language server",
                    root.display()
                );
                self.trust.folders.mark(root, true);
                self.trust.carried += 1;
            }
        }
    }

    /// Tells the window at `token`, which the user is at, how many folders
    /// were trusted as the build before ran their servers, if any were and
    /// no window has been told yet.
    pub(super) fn tell_trusted_through_upgrade(&mut self, token: Token) {
        let carried = std::mem::take(&mut self.trust.carried);
        if carried == 0 {
            return;
        }
        let folders = if carried == 1 { "folder" } else { "folders" };
        let said = format!(
            "trusted {carried} {folders} whose language servers were running: :trusted lists them"
        );
        self.send_event(token, clackatoo_protocol::Event::Said(said));
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use clackatoo_protocol::{Content, OpenFile};

    use super::*;

    #[test]
    fn only_an_upgrade_from_a_build_that_never_asked_trusts_what_had_servers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let pack = root.join("languages/fake");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("language.toml"),
            "grammar = \"fake\"\nextensions = [\"fk\"]\nserver = [\"/bin/true\"]\n\
             roots = [\"project\"]\n",
        )
        .unwrap();
        for project in ["open", "other"] {
            std::fs::create_dir(root.join(project)).unwrap();
            std::fs::write(root.join(project).join("project"), "").unwrap();
            std::fs::write(root.join(project).join("a.fk"), "").unwrap();
        }
        let (daemon, _) = crate::server::agent_files::tests::daemon_with_chat(&root);
        let mut config = daemon.config.clone();
        drop(daemon);
        // Asking about every folder, keeping none, as a file deleted or
        // never written leaves it.
        config.languages = Some(root.join("languages"));
        config.trusted = Trusting::ThisRun;
        let mut daemon = Daemon::listening(
            mio::net::UnixListener::bind(root.join("again.sock")).unwrap(),
            config.clone(),
            None,
        )
        .unwrap();
        let open = |daemon: &mut Daemon, project: &str| {
            let file = OpenFile {
                path: root.join(project).join("a.fk").display().to_string(),
                cwd: None,
            };
            let id = daemon.open_buffer(file, None).unwrap();
            let name = daemon.workspaces.name_for_new(None).unwrap();
            daemon.workspaces.new_workspace(name, Content::Buffer(id));
            id
        };
        let id = open(&mut daemon, "open");
        assert!(daemon.servers.slots.is_empty(), "not trusted: not started");
        assert_eq!(daemon.untrusted_root(id), Some(root.join("open").as_path()));
        // From a build that asks, nothing is trusted, though nothing was
        // kept.
        let handover = daemon.hand_over(Instant::now()).unwrap();
        drop(daemon);
        let mut resumed = Daemon::resume(handover, config.clone()).unwrap();
        assert!(!resumed.trusts(&root.join("open")));
        assert_eq!(resumed.trust.carried, 0);
        // From one that ran its servers unasked, what's open is trusted.
        let mut handover = resumed.hand_over(Instant::now()).unwrap();
        drop(resumed);
        handover.version = crate::handover::ASKS_TRUST - 1;
        let resumed = Daemon::resume(handover, config).unwrap();
        assert!(resumed.trusts(&root.join("open")));
        assert!(!resumed.trusts(&root.join("other")));
        assert_eq!(resumed.trust.carried, 1, "a window is told");
    }
}
