//! The daemon's side of the image layer (CLACKA-239): picture panes, the
//! images an editor's Markdown shows, chats' images kept in the store,
//! and clients fetching any of them by their IDs (see `crate::images`).

use std::collections::{BTreeMap, HashMap};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

use clackatoo_protocol::{
    BufferId, Content, Error, Event, ImageAnswer, ImageFormat, ImageFrom, ImageId, ImageInfo,
    ImageNews, PaneId, Picture, PictureId, PictureState, Response, TerminalId,
};
use mio::Token;

use super::Daemon;
use crate::handover;
use crate::images::{Source, Why};

/// The extensions of the files a pane opens as a picture: the formats
/// the image layer shows.
const PICTURES: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// Picture panes, and the images buffers' Markdown asked for.
#[derive(Debug, Default)]
pub(super) struct Pictures {
    next: u32,
    shown: BTreeMap<PictureId, Shown>,
    /// The images buffers' Markdown asked for, by buffer and path as
    /// asked.
    markdown: HashMap<(BufferId, String), Markdown>,
}

/// A picture pane's file, and what's shown of it.
#[derive(Debug)]
struct Shown {
    path: PathBuf,
    picture: Picture,
    /// The clients attached, told what it shows each time that changes:
    /// those showing it.
    watchers: Vec<Token>,
    /// The image pinned for it, as it's shown (see
    /// [`Daemon::pin_picture`]).
    pinned: Option<ImageId>,
}

/// The most images a buffer's Markdown is read for.
const MARKDOWN_IMAGES: usize = 64;

/// An image a buffer's Markdown asked for.
#[derive(Debug)]
enum Markdown {
    /// Being read, with who's to be told, each by the path it asked with.
    Loading(Vec<(Token, String)>),
    /// Read: the image, or why not.
    Read(Result<ImageInfo, String>),
}

/// Whether the file at `path` is opened as a picture: by its extension.
pub(super) fn is_picture(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            PICTURES
                .iter()
                .any(|picture| extension.eq_ignore_ascii_case(picture))
        })
}

impl Daemon {
    /// A new picture pane's content, showing the file at `path`, which is
    /// read on the images' thread.
    pub(super) fn open_picture(&mut self, path: PathBuf) -> Content {
        self.pictures.next += 1;
        let id = PictureId(self.pictures.next);
        let shown = super::tools::homely(&path, &self.config.home);
        log::info!("picture {id}: {shown}");
        let state = match (self.images).keep(
            ImageFrom::Picture(id),
            Why::Picture(id),
            Source::File(path.clone()),
        ) {
            Ok(()) => PictureState::Loading,
            Err(why) => PictureState::Failed(why),
        };
        self.pictures.shown.insert(
            id,
            Shown {
                path,
                picture: Picture {
                    path: super::tools::visible(&shown),
                    state,
                },
                watchers: Vec::new(),
                pinned: None,
            },
        );
        Content::Picture(id)
    }

    /// The file picture pane `picture` shows.
    pub(super) fn picture_path(&self, picture: PictureId) -> Option<&Path> {
        (self.pictures.shown.get(&picture)).map(|shown| shown.path.as_path())
    }

    /// Takes back picture pane `id`, handed over, showing `path`: its image
    /// as the store has it, if it does, or else read again.
    pub(super) fn resume_picture(&mut self, id: PictureId, path: PathBuf, image: Option<ImageId>) {
        self.pictures.next = self.pictures.next.max(id.0);
        let owner = ImageFrom::Picture(id);
        let state = match image.and_then(|image| self.images.info(image)) {
            Some(info) if self.images.holds(owner, info.id) => PictureState::Shown(info),
            _ => match self
                .images
                .keep(owner, Why::Picture(id), Source::File(path.clone()))
            {
                Ok(()) => PictureState::Loading,
                Err(why) => PictureState::Failed(why),
            },
        };
        let shown = super::tools::homely(&path, &self.config.home);
        self.pictures.shown.insert(
            id,
            Shown {
                path,
                picture: Picture {
                    path: super::tools::visible(&shown),
                    state,
                },
                watchers: Vec::new(),
                pinned: None,
            },
        );
    }

    /// The image picture pane `picture` shows, if it's read.
    pub(super) fn picture_image(&self, picture: PictureId) -> Option<ImageId> {
        match &self.pictures.shown.get(&picture)?.picture.state {
            PictureState::Shown(info) => Some(info.id),
            PictureState::Loading | PictureState::Failed(_) => None,
        }
    }

    /// Answers [`Request::AttachPicture`](clackatoo_protocol::Request::AttachPicture).
    pub(super) fn attach_picture(&mut self, token: Token, picture: PictureId) -> Response {
        let owner = ImageFrom::Picture(picture);
        let Some(shown) = self.pictures.shown.get_mut(&picture) else {
            return Response::Error(Error::Other("there's no such picture".into()));
        };
        if !shown.watchers.contains(&token) {
            shown.watchers.push(token);
        }
        // Let go while nobody showed it: read again.
        if let PictureState::Shown(info) = shown.picture.state
            && !self.images.holds(owner, info.id)
        {
            let source = Source::File(shown.path.clone());
            shown.picture.state = match self.images.keep(owner, Why::Picture(picture), source) {
                Ok(()) => PictureState::Loading,
                Err(why) => PictureState::Failed(why),
            };
        }
        let shown = shown.picture.clone();
        self.pin_picture(picture);
        Response::Picture(shown)
    }

    /// Answers [`Request::DetachPicture`](clackatoo_protocol::Request::DetachPicture):
    /// the client at `token` no longer shows `picture`.
    pub(super) fn detach_picture(&mut self, token: Token, picture: PictureId) -> Response {
        if let Some(shown) = self.pictures.shown.get_mut(&picture) {
            shown.watchers.retain(|watcher| *watcher != token);
        }
        self.pin_picture(picture);
        Response::Done
    }

    /// Pins picture pane `picture`'s image while some client shows it, and
    /// only then: one nobody shows may be let go like any other, and is
    /// read again from its file when it's shown again.
    fn pin_picture(&mut self, picture: PictureId) {
        let Some(shown) = self.pictures.shown.get_mut(&picture) else {
            return;
        };
        let wanted = match shown.picture.state {
            PictureState::Shown(info) if !shown.watchers.is_empty() => Some(info.id),
            _ => None,
        };
        if wanted == shown.pinned {
            return;
        }
        let old = std::mem::replace(&mut shown.pinned, wanted);
        if let Some(id) = wanted {
            self.images.pin(id);
        }
        if let Some(id) = old {
            self.images.unpin(id);
        }
    }

    /// Closes `pane`, which shows picture `picture`, and the picture.
    pub(super) fn close_picture_pane(&mut self, pane: PaneId, picture: PictureId) {
        if self.workspaces.remove_pane(pane) {
            self.workspaces_changed();
        }
        self.forget_picture(picture);
    }

    /// Lets picture pane `picture` go, and its image, unless something
    /// else holds it.
    pub(super) fn forget_picture(&mut self, picture: PictureId) {
        let Some(shown) = self.pictures.shown.remove(&picture) else {
            return;
        };
        if let Some(id) = shown.pinned {
            self.images.unpin(id);
        }
        self.images.release(ImageFrom::Picture(picture));
    }

    /// Lets buffer `buffer`'s images go, as it's closed.
    pub(super) fn forget_buffer_images(&mut self, buffer: BufferId) {
        (self.pictures.markdown).retain(|(asked, _), _| *asked != buffer);
        self.images.release(ImageFrom::Buffer(buffer));
    }

    /// Lets terminal `terminal`'s images go, as it's closed: a chat's too.
    pub(super) fn forget_terminal_images(&mut self, terminal: TerminalId) {
        self.images.release(ImageFrom::Terminal(terminal));
    }

    /// Forgets the client at `token`, which has gone, as asking for
    /// images and watching pictures.
    pub(super) fn forget_image_asker(&mut self, token: Token) {
        self.images.forget_asker(&token);
        let watched: Vec<PictureId> = (self.pictures.shown.iter_mut())
            .filter(|(_, shown)| shown.watchers.contains(&token))
            .map(|(&picture, shown)| {
                shown.watchers.retain(|watcher| *watcher != token);
                picture
            })
            .collect();
        for picture in watched {
            self.pin_picture(picture);
        }
        for asked in self.pictures.markdown.values_mut() {
            if let Markdown::Loading(waiting) = asked {
                waiting.retain(|(waiter, _)| *waiter != token);
            }
        }
    }

    /// Whether the client at `token` may have the images `from` holds.
    fn may_have_images(&self, token: Token, from: ImageFrom) -> bool {
        match from {
            ImageFrom::Terminal(terminal) => {
                self.terminals.contains_key(&terminal) && self.may_see_terminal(token, terminal)
            }
            ImageFrom::Picture(picture) => self.pictures.shown.contains_key(&picture),
            ImageFrom::Buffer(buffer) => self.buffers.contains_key(&buffer),
        }
    }

    /// Answers [`Request::Image`](clackatoo_protocol::Request::Image).
    pub(super) fn serve_image(
        &mut self,
        token: Token,
        from: ImageFrom,
        image: ImageId,
        fit: Option<[u32; 2]>,
        at: u64,
    ) -> Response {
        if !self.may_have_images(token, from) {
            return Response::Image(ImageAnswer::Missing);
        }
        Response::Image(self.images.fetch(token, from, image, fit, at))
    }

    /// Answers [`Request::ImageAt`](clackatoo_protocol::Request::ImageAt):
    /// the image at `path` from the folder `buffer`'s file is in, read
    /// on the images' thread the first time it's asked for.
    pub(super) fn image_at(&mut self, token: Token, buffer: BufferId, path: String) -> Response {
        let Some(slot) = self.buffers.get(&buffer) else {
            return Response::Error(Error::NoSuchBuffer(buffer));
        };
        let refused = |why: &str| Response::Image(ImageAnswer::Failed(why.to_owned()));
        if path.contains('\0') || path.len() > 4096 {
            return refused("it isn't a path");
        }
        if path.contains("://") || path.starts_with("data:") {
            return refused("images from the web aren't shown");
        }
        let Some(dir) = slot.buffer.path().parent().map(Path::to_owned) else {
            return refused("the file has no folder");
        };
        // From the project's root, if the folder's trusted, and else only
        // from the file's own folder down.
        let (root, from_root) = self.markdown_root(&dir);
        let trusted = from_root.is_some();
        let from_root = from_root.unwrap_or_default();
        if !trusted && path.split('/').any(|part| part == "..") {
            return refused(
                "in a folder not trusted, only a path down from the file's folder is shown",
            );
        }
        let joined = if from_root.as_os_str().is_empty() {
            path.clone()
        } else {
            format!("{}/{path}", from_root.display())
        };
        let Some(plain) = clackatoo_protocol::plain_path(&joined) else {
            return refused("only a path within the file's project is shown");
        };
        let key = (buffer, plain);
        match self.pictures.markdown.get_mut(&key) {
            Some(Markdown::Loading(waiting)) => {
                let asker = (token, path);
                if !waiting.contains(&asker) {
                    waiting.push(asker);
                }
                return Response::Image(ImageAnswer::Loading);
            }
            Some(Markdown::Read(Ok(info)))
                if self.images.holds(ImageFrom::Buffer(buffer), info.id) =>
            {
                return Response::Image(ImageAnswer::Found(*info));
            }
            Some(Markdown::Read(Err(why))) => return refused(why),
            // Let go since: read again.
            Some(Markdown::Read(Ok(_))) => {}
            None => {
                let asked = (self.pictures.markdown.keys())
                    .filter(|(asked, _)| *asked == buffer)
                    .count();
                if asked >= MARKDOWN_IMAGES {
                    return refused(&format!("a file shows at most {MARKDOWN_IMAGES} images"));
                }
            }
        }
        let source = Source::Below {
            root,
            path: PathBuf::from(&key.1),
        };
        let why = Why::Markdown {
            buffer,
            path: key.1.clone(),
        };
        if let Err(why) = self.images.keep(ImageFrom::Buffer(buffer), why, source) {
            return refused(&why);
        }
        (self.pictures.markdown).insert(key, Markdown::Loading(vec![(token, path)]));
        Response::Image(ImageAnswer::Loading)
    }

    /// The folder Markdown in the folder `dir` may name images from, and
    /// where `dir` is in it: its project's, the repository it's in, or
    /// else `dir` itself, if it's trusted (see `trust.rs`); else `dir`
    /// alone, and `None`.
    fn markdown_root(&self, dir: &Path) -> (PathBuf, Option<PathBuf>) {
        if self.trust.folders.trusts(dir) != Some(true) {
            return (dir.to_owned(), None);
        }
        // Stats, as opening a file does: up to the repository's root.
        let repository = (dir.ancestors().take(64))
            .find(|folder| folder.join(".git").symlink_metadata().is_ok())
            .filter(|root| self.trust.folders.trusts(root) == Some(true));
        let root = repository.unwrap_or(dir);
        let from_root = dir.strip_prefix(root).unwrap_or(Path::new("")).to_owned();
        (root.to_owned(), Some(from_root))
    }

    /// Takes in what the images' thread has done: pictures read, Markdown's
    /// images read, chats' images kept, and copies made, telling each who
    /// asked. If `wait`, waits until it has done everything, as before a
    /// handover.
    pub(super) fn take_images(&mut self, wait: Option<std::time::Duration>) {
        let (kept, ready) = self.images.take(wait);
        for kept in kept {
            match kept.why {
                Why::Picture(picture) => self.picture_read(picture, kept.kept),
                Why::Markdown { buffer, path } => {
                    let found = kept.kept;
                    let waiting = match self
                        .pictures
                        .markdown
                        .insert((buffer, path.clone()), Markdown::Read(found.clone()))
                    {
                        Some(Markdown::Loading(waiting)) => waiting,
                        _ => Vec::new(),
                    };
                    for (token, path) in waiting {
                        let news = ImageNews::Found {
                            buffer,
                            path,
                            found: found.clone(),
                        };
                        self.send_event(token, Event::Image(news));
                    }
                }
                Why::Chat { chat, original } => match kept.kept {
                    Ok(info) => {
                        if let Some(chat_ref) = self.chat(chat)
                            && chat_ref.image_kept(original, info.id)
                        {
                            self.collect_damage(chat);
                        }
                    }
                    Err(why) => log::info!("chat {chat}: an image wasn't kept: {why}"),
                },
                #[cfg(test)]
                Why::Test(_) => {}
            }
        }
        for ready in ready {
            for (token, from, fit) in ready.waiting {
                let news = ImageNews::Scaled {
                    from,
                    image: ready.image,
                    fit,
                };
                self.send_event(token, Event::Image(news));
            }
        }
    }

    /// Picture pane `picture`'s file was read, or wasn't: shown to those
    /// watching it.
    fn picture_read(&mut self, picture: PictureId, read: Result<ImageInfo, String>) {
        let Some(shown) = self.pictures.shown.get_mut(&picture) else {
            // Closed meanwhile.
            self.images.release(ImageFrom::Picture(picture));
            return;
        };
        shown.picture.state = match read {
            Ok(info) => PictureState::Shown(info),
            Err(why) => PictureState::Failed(why),
        };
        let (watchers, picture_shown) = (shown.watchers.clone(), shown.picture.clone());
        self.pin_picture(picture);
        for token in watchers {
            let shown = picture_shown.clone();
            self.send_event(token, Event::Picture { picture, shown });
        }
    }

    /// Keeps in the store the originals chats are waiting to keep: the
    /// images sent in their messages and their tools' results.
    pub(super) fn keep_chat_images(&mut self) {
        let waiting: Vec<(TerminalId, Vec<(u64, Source)>)> = (self.terminals.iter_mut())
            .filter_map(|(&id, slot)| {
                let chat = slot.terminal.chat()?;
                let waiting = chat.originals.take_waiting();
                (!waiting.is_empty()).then_some((id, waiting))
            })
            .collect();
        for (chat, waiting) in waiting {
            for (original, source) in waiting {
                let why = Why::Chat { chat, original };
                if let Err(why) = self.images.keep(ImageFrom::Terminal(chat), why, source) {
                    log::info!("chat {chat}: an image wasn't kept: {why}");
                }
            }
        }
    }

    /// The original of chat `chat`'s thumbnail `original`, as standard
    /// base64, as a client before the image layer asks for it.
    pub(super) fn chat_original(&mut self, chat: TerminalId, original: u64) -> Option<String> {
        let image = self.chat_ref(chat)?.originals.get(original)?;
        match self.images.get(ImageFrom::Terminal(chat), image) {
            crate::images::Found::Kept { bytes, .. } => Some(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                bytes,
            )),
            _ => None,
        }
    }
}

/// The folder in the runtime directory the store's images go in, a file
/// each, for the build after an upgrade to read.
const SPILLED: &str = "images";

impl Daemon {
    /// The image layer, for the handover: each image's bytes written to a
    /// file of its own in a folder private to the user in the runtime
    /// directory, which the next build reads and takes away, and what's
    /// known of each, with `chats`, the images their thumbnails' handles
    /// name, and the picture panes. With no runtime directory, or if the
    /// folder can't be made, the images themselves are lost, and the panes
    /// show what they can again: their files are read again.
    pub(super) fn hand_over_images(&self, chats: Vec<handover::ChatImages>) -> handover::Images {
        let dir = (self.runtime.as_ref())
            .map(|runtime| runtime.join(SPILLED))
            .filter(|dir| match fresh_private_dir(dir) {
                Ok(()) => true,
                Err(err) => {
                    log::error!("the images can't be handed over: {}: {err}", dir.display());
                    false
                }
            });
        let kept = dir
            .as_deref()
            .map(|dir| self.spill(dir))
            .unwrap_or_default();
        let pictures = (self.pictures.shown.iter())
            .map(|(&id, shown)| handover::PictureShown {
                id: id.0,
                path: shown.path.as_os_str().as_bytes().to_vec(),
                image: match &shown.picture.state {
                    PictureState::Shown(info) => Some(info.id.0),
                    PictureState::Loading | PictureState::Failed(_) => None,
                },
            })
            .collect();
        handover::Images {
            dir: dir
                .map(|dir| dir.as_os_str().as_bytes().to_vec())
                .unwrap_or_default(),
            kept,
            dropped: self
                .images
                .dropped()
                .map(|(id, limit)| (id.0, limit))
                .collect(),
            chats,
            pictures,
        }
    }

    /// Writes each image's bytes to a file of its own in `dir`, returning
    /// what's known of those written.
    fn spill(&self, dir: &Path) -> Vec<handover::KeptImage> {
        let mut kept = Vec::new();
        // Those used last first, within the limit in all, as the next
        // build keeps them.
        let mut all: Vec<_> = self.images.all().collect();
        all.sort_unstable_by_key(|&(.., used)| std::cmp::Reverse(used));
        let mut room = self.images.limits().total;
        for (info, bytes, owners, _, _) in all {
            let Some(left) = room.checked_sub(info.bytes) else {
                continue;
            };
            room = left;
            let path = dir.join(info.id.to_string());
            if let Err(err) = write_private(&path, bytes) {
                log::error!("an image can't be handed over: {}: {err}", path.display());
                continue;
            }
            kept.push(handover::KeptImage {
                id: info.id.0,
                format: match info.format {
                    ImageFormat::Png => 0,
                    ImageFormat::Jpeg => 1,
                    ImageFormat::Gif => 2,
                    ImageFormat::Webp => 3,
                },
                width: info.width,
                height: info.height,
                bytes: info.bytes,
                owners: owners.iter().map(|&owner| owner_of(owner)).collect(),
            });
        }
        kept
    }

    /// Takes back the image layer handed over: the images, read from their
    /// files, which are taken away, and the picture panes.
    pub(super) fn resume_images(&mut self, images: handover::Images) {
        let dir = PathBuf::from(std::ffi::OsString::from_vec(images.dir));
        for image in images.kept {
            let format = match image.format {
                0 => ImageFormat::Png,
                1 => ImageFormat::Jpeg,
                2 => ImageFormat::Gif,
                3 => ImageFormat::Webp,
                _ => continue,
            };
            let info = ImageInfo {
                id: ImageId(image.id),
                width: image.width,
                height: image.height,
                format,
                bytes: image.bytes,
            };
            let path = dir.join(info.id.to_string());
            let read =
                crate::regular::open(&path, crate::regular::Links::Refuse).and_then(|file| {
                    let mut bytes = Vec::new();
                    std::io::Read::read_to_end(
                        &mut std::io::Read::take(file, info.bytes + 1),
                        &mut bytes,
                    )
                    .map(|_| bytes)
                });
            let owners: Vec<ImageFrom> = image
                .owners
                .iter()
                .map(|&owner| from_owner(owner))
                .collect();
            match read {
                Ok(bytes) => {
                    if !self.images.resume(info, bytes, &owners, 0) {
                        log::warn!("an image handed over wasn't kept: {}", info.id);
                    }
                }
                Err(err) => {
                    log::warn!(
                        "an image handed over can't be read: {}: {err}",
                        path.display()
                    );
                }
            }
        }
        for (id, limit) in images.dropped {
            self.images.resume_dropped(ImageId(id), limit);
        }
        if !dir.as_os_str().is_empty()
            && let Err(err) = std::fs::remove_dir_all(&dir)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!(
                "the images handed over can't be taken away: {}: {err}",
                dir.display()
            );
        }
        for picture in images.pictures {
            let path = PathBuf::from(std::ffi::OsString::from_vec(picture.path));
            self.resume_picture(PictureId(picture.id), path, picture.image.map(ImageId));
        }
    }
}

/// `owner` as it's handed over.
fn owner_of(owner: ImageFrom) -> handover::ImageOwner {
    match owner {
        ImageFrom::Terminal(terminal) => handover::ImageOwner::Terminal(terminal.0),
        ImageFrom::Picture(picture) => handover::ImageOwner::Picture(picture.0),
        ImageFrom::Buffer(buffer) => handover::ImageOwner::Buffer(buffer.0),
    }
}

/// An owner handed over, as it is.
fn from_owner(owner: handover::ImageOwner) -> ImageFrom {
    match owner {
        handover::ImageOwner::Terminal(terminal) => ImageFrom::Terminal(TerminalId(terminal)),
        handover::ImageOwner::Picture(picture) => ImageFrom::Picture(PictureId(picture)),
        handover::ImageOwner::Buffer(buffer) => ImageFrom::Buffer(BufferId(buffer)),
    }
}

/// Makes `dir` afresh, empty and the user's alone, taking away whatever a
/// handover before left there.
fn fresh_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    match std::fs::remove_dir_all(dir) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

/// Writes `bytes` to a new file at `path`, the user's alone.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use clackatoo_protocol::{
        ImageAnswer, ImageFormat, ImageFrom, OpenFile, PictureState, Response, TerminalId,
    };
    use mio::Token;

    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;

    /// Whoever asks, in these tests.
    const ASKER: Token = Token(usize::MAX);

    /// How long tests wait for images to be read.
    const WAIT: std::time::Duration = std::time::Duration::from_mins(1);

    /// A PNG `width` by `height`, each pixel `shade`.
    fn png(width: u32, height: u32, shade: u8) -> Vec<u8> {
        let rgba = vec![shade; (width * height * 4) as usize];
        crate::images::png_of(width, height, &rgba).unwrap()
    }

    /// The whole of image `id` of `from`'s, fetched a part at a time.
    fn fetch(daemon: &mut Daemon, from: ImageFrom, id: ImageId) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let at = bytes.len() as u64;
            let Response::Image(ImageAnswer::Part {
                total, bytes: part, ..
            }) = daemon.serve_image(ASKER, from, id, None, at)
            else {
                panic!("no part of {id}");
            };
            bytes.extend(part);
            if bytes.len() as u64 == total {
                return bytes;
            }
        }
    }

    /// What picture pane `picture` shows, as a client attaching is told.
    fn shown(daemon: &mut Daemon, picture: PictureId) -> PictureState {
        match daemon.attach_picture(ASKER, picture) {
            Response::Picture(shown) => shown.state,
            other => panic!("{other:?}"),
        }
    }

    fn open(daemon: &mut Daemon, dir: &Path, path: &str) -> PictureId {
        let file = OpenFile {
            path: path.into(),
            cwd: Some(dir.display().to_string()),
        };
        match daemon.open_content(file, None).unwrap() {
            Content::Picture(picture) => picture,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_image_file_opens_as_a_picture_read_off_the_loop_and_is_fetched_whole() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let bytes = png(300, 200, 9);
        std::fs::write(dir.path().join("plan.PNG"), &bytes).unwrap();
        let picture = open(&mut daemon, dir.path(), "plan.PNG");
        assert_eq!(shown(&mut daemon, picture), PictureState::Loading);
        daemon.take_images(Some(WAIT));
        let PictureState::Shown(info) = shown(&mut daemon, picture) else {
            panic!("not read");
        };
        assert_eq!(
            (info.width, info.height, info.format),
            (300, 200, ImageFormat::Png)
        );
        assert_eq!(info.id, crate::images::id_of(&bytes));
        let from = ImageFrom::Picture(picture);
        assert_eq!(fetch(&mut daemon, from, info.id), bytes);
        // Only its owner's: a chat's asking finds nothing.
        let chat = ImageFrom::Terminal(TerminalId(1));
        assert_eq!(
            daemon.serve_image(ASKER, chat, info.id, None, 0),
            Response::Image(ImageAnswer::Missing)
        );
        // Text with an image's name says it isn't one; a text file opens as
        // text, as ever.
        std::fs::write(dir.path().join("fake.png"), "hello").unwrap();
        let fake = open(&mut daemon, dir.path(), "fake.png");
        daemon.take_images(Some(WAIT));
        assert_eq!(
            shown(&mut daemon, fake),
            PictureState::Failed("it isn't an image".into())
        );
        std::fs::write(dir.path().join("notes.txt"), "words").unwrap();
        let file = OpenFile {
            path: "notes.txt".into(),
            cwd: Some(dir.path().display().to_string()),
        };
        assert!(matches!(
            daemon.open_content(file, None),
            Ok(Content::Buffer(_))
        ));
        // Its image goes with it, as nothing else holds it.
        daemon.forget_picture(picture);
        assert!(daemon.images.info(info.id).is_none());
    }

    /// A picture pane's image is kept, pinned, only while a client shows
    /// it: once none does, it may be let go, and it's read again from its
    /// file as one shows it again.
    #[test]
    fn a_picture_nobody_shows_may_be_let_go_and_is_read_again_when_shown() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let one = png(40, 40, 5).len() as u64;
        let limits = crate::images::Limits {
            per_owner: one * 4,
            total: one + one / 2,
        };
        daemon.images = crate::images::Images::new(limits, None);
        std::fs::write(dir.path().join("plan.png"), png(40, 40, 5)).unwrap();
        let picture = open(&mut daemon, dir.path(), "plan.png");
        daemon.take_images(Some(WAIT));
        let PictureState::Shown(read) = shown(&mut daemon, picture) else {
            panic!("not read");
        };
        // Shown, it stays: another image past the limit isn't kept.
        let chat = ImageFrom::Terminal(chat);
        let other = |shade| {
            let bytes = png(40, 40, shade);
            let (format, width, height) = crate::images::look_at(&bytes).unwrap();
            let info = ImageInfo {
                id: crate::images::id_of(&bytes),
                width,
                height,
                format,
                bytes: bytes.len() as u64,
            };
            (info, bytes)
        };
        let (info, bytes) = other(6);
        assert!(daemon.images.insert(chat, info, bytes.into()).is_err());
        // Shown nowhere, it goes for another.
        daemon.detach_picture(ASKER, picture);
        let (info, bytes) = other(7);
        assert!(daemon.images.insert(chat, info, bytes.into()).is_ok());
        assert!(daemon.images.info(read.id).is_none(), "let go");
        // Shown again: read again from its file.
        assert_eq!(shown(&mut daemon, picture), PictureState::Loading);
        daemon.take_images(Some(WAIT));
        assert_eq!(shown(&mut daemon, picture), PictureState::Shown(read));
    }

    /// The answer to asking for each of `paths`, read, from Markdown in
    /// `docs/notes.md` in a project at `dir`: a repository, its folder
    /// trusted if `trusted`.
    fn markdown_answers(
        dir: &Path,
        trusted: bool,
        paths: &[&str],
    ) -> Vec<Result<ImageInfo, String>> {
        let (mut daemon, _) = daemon_with_chat(dir);
        if !trusted {
            daemon.trust.folders = crate::trust::Trust::load(&crate::Trusting::ThisRun);
        }
        let file = OpenFile {
            path: dir.join("docs/notes.md").display().to_string(),
            cwd: None,
        };
        let buffer = daemon.open_buffer(file, None).unwrap();
        for path in paths {
            daemon.image_at(ASKER, buffer, (*path).into());
        }
        daemon.take_images(Some(WAIT));
        (paths.iter())
            .map(
                |path| match daemon.image_at(ASKER, buffer, (*path).into()) {
                    Response::Image(ImageAnswer::Found(info)) => {
                        assert_eq!(
                            fetch(&mut daemon, ImageFrom::Buffer(buffer), info.id).len() as u64,
                            info.bytes
                        );
                        Ok(info)
                    }
                    Response::Image(ImageAnswer::Failed(why)) => Err(why),
                    other => panic!("{path}: {other:?}"),
                },
            )
            .collect()
    }

    /// Markdown's images are read from below its project's root, in a
    /// folder trusted, `..` taking them up only as far as that; in one not
    /// trusted, only from the file's own folder down; never through a link
    /// at any part of the path; and never from the web.
    #[test]
    fn markdowns_images_are_read_only_from_within_its_project_never_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("docs/img")).unwrap();
        std::fs::write(root.join("docs/notes.md"), "![plan](img/plan.png)").unwrap();
        std::fs::write(root.join("docs/img/plan.png"), png(4, 4, 1)).unwrap();
        std::fs::write(root.join("up.png"), png(2, 2, 2)).unwrap();
        std::os::unix::fs::symlink(root.join("up.png"), root.join("docs/link.png")).unwrap();
        std::fs::create_dir(root.join("elsewhere")).unwrap();
        std::fs::write(root.join("elsewhere/x.png"), png(3, 3, 3)).unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("docs/linked")).unwrap();
        let paths = [
            "img/plan.png",
            "./img/../img/plan.png",
            "../up.png",
            "../../up.png",
            "link.png",
            "linked/x.png",
            "/etc/passwd",
            "https://example.com/x.png",
            "data:image/png;base64,AA",
        ];
        let trusted = markdown_answers(root, true, &paths);
        assert_eq!(trusted[0].as_ref().map(|info| info.width), Ok(4));
        assert_eq!(trusted[1], trusted[0], "the same image, read once");
        assert_eq!(
            trusted[2].as_ref().map(|info| info.width),
            Ok(2),
            "within the project"
        );
        for (path, answer) in paths.iter().zip(&trusted).skip(3) {
            assert!(answer.is_err(), "{path} was read");
        }
        assert!(
            trusted[4].as_ref().unwrap_err().contains("link"),
            "{trusted:?}"
        );
        let untrusted = markdown_answers(root, false, &paths);
        assert_eq!(untrusted[0].as_ref().map(|info| info.width), Ok(4));
        assert!(
            untrusted[2].as_ref().unwrap_err().contains("not trusted"),
            "{untrusted:?}"
        );
        for (path, answer) in paths.iter().zip(&untrusted).skip(3) {
            assert!(answer.is_err(), "{path} was read");
        }
    }

    /// A buffer's Markdown is read so many images at most, each kept by its
    /// path written plainly, so one named two ways is read once.
    #[test]
    fn a_files_markdown_is_read_so_many_images_at_most() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        std::fs::write(dir.path().join("notes.md"), "").unwrap();
        let file = OpenFile {
            path: dir.path().join("notes.md").display().to_string(),
            cwd: None,
        };
        let buffer = daemon.open_buffer(file, None).unwrap();
        for n in 0..MARKDOWN_IMAGES {
            let path = format!("img/{n}.png");
            let loading = Response::Image(ImageAnswer::Loading);
            assert_eq!(daemon.image_at(ASKER, buffer, path), loading);
        }
        // The same again, named otherwise, is no more.
        let again = daemon.image_at(ASKER, buffer, "./img/../img/0.png".into());
        assert_eq!(again, Response::Image(ImageAnswer::Loading));
        let Response::Image(ImageAnswer::Failed(why)) =
            daemon.image_at(ASKER, buffer, "one/more.png".into())
        else {
            panic!("read past the most");
        };
        assert!(why.contains("at most"), "{why}");
    }

    #[test]
    fn images_survive_an_upgrade_through_files_of_their_own_which_go_after() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        std::fs::write(dir.path().join("plan.png"), png(30, 20, 3)).unwrap();
        let picture = open(&mut daemon, dir.path(), "plan.png");
        // A chat's image, kept for one of its thumbnails.
        let sent = png(8, 8, 7);
        let original = (daemon.chat(chat).unwrap().originals)
            .insert(crate::images::Source::Bytes(sent.clone()));
        daemon.keep_chat_images();
        daemon.take_images(Some(WAIT));
        let PictureState::Shown(info) = shown(&mut daemon, picture) else {
            panic!("not read");
        };
        let kept = daemon
            .chat_ref(chat)
            .unwrap()
            .originals
            .get(original)
            .unwrap();
        let config = daemon.config.clone();
        let handover = daemon.hand_over(Instant::now()).unwrap();
        let spilled = dir.path().join(SPILLED);
        assert!(
            spilled.join(kept.to_string()).is_file(),
            "spilled to a file"
        );
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&spilled).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o700, "the user's alone");
        drop(daemon);
        let mut resumed = Daemon::resume(handover, config).unwrap();
        assert!(!spilled.exists(), "taken away once read");
        assert_eq!(shown(&mut resumed, picture), PictureState::Shown(info));
        let from = ImageFrom::Terminal(chat);
        assert_eq!(fetch(&mut resumed, from, kept), sent);
        assert_eq!(
            resumed.chat_ref(chat).unwrap().originals.get(original),
            Some(kept)
        );
        // An older client's fetch of the original still answers.
        let old = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &sent);
        assert_eq!(resumed.chat_original(chat, original), Some(old));
    }
}
