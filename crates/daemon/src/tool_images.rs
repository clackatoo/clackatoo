//! Image tool results: bounded decoding on one worker, never on the event loop.
//! Inline ACP images and local resource links use the same thumbnail path.
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use agent_client_protocol_schema::v1::{
    ContentBlock, EmbeddedResourceResource, SessionUpdate, ToolCallContent,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use clackatoo_protocol::{ChatImage, ChatPreview, ImageId, TerminalId, ToolContent};

const BYTES: usize = 8 * 1024 * 1024;
const COUNT: usize = 4;

#[derive(Debug)]
pub(crate) enum Source {
    Inline(String),
    File(PathBuf),
}

/// Identifies exactly the result to which the worker may attach pixels. Later
/// turns, an undo, or replacement content must never receive a stale preview.
#[derive(Debug)]
pub(crate) struct Target {
    pub terminal: TerminalId,
    /// The turn, by its uid, which stays with it.
    pub turn: crate::chat::TurnUid,
    pub sent: u64,
    pub tool: String,
    pub content: Vec<ToolContent>,
    pub receipt: Arc<()>,
}

#[derive(Debug)]
pub(crate) struct Job {
    pub target: Target,
    pub cwd: PathBuf,
    pub sources: Vec<Source>,
}

#[derive(Debug)]
pub(crate) struct Done {
    pub target: Target,
    /// Each thumbnail, with the image's bytes.
    pub images: Vec<(ChatPreview, Vec<u8>)>,
}

#[derive(Debug, Default)]
pub(crate) struct Queue {
    worker: Option<(mpsc::SyncSender<Job>, mpsc::Receiver<Done>)>,
}

impl Queue {
    pub(crate) fn submit(&mut self, job: Job, waker: Arc<mio::Waker>) {
        if self.worker.is_none() {
            let (send, jobs) = mpsc::sync_channel::<Job>(2);
            let (done, receive) = mpsc::sync_channel(2);
            let spawned = std::thread::Builder::new()
                .name("tool-images".into())
                .spawn(move || {
                    while let Ok(job) = jobs.recv() {
                        let images = job
                            .sources
                            .into_iter()
                            .filter_map(|source| decode(source, &job.cwd))
                            .collect();
                        if done
                            .send(Done {
                                target: job.target,
                                images,
                            })
                            .is_err()
                        {
                            break;
                        }
                        let _ = waker.wake();
                    }
                });
            if spawned.is_err() {
                return;
            }
            self.worker = Some((send, receive));
        }
        if let Some((send, _)) = &self.worker {
            // A busy preview worker must never stall the chat or retain an
            // unbounded queue of base64. The tool's ordinary label remains.
            let _ = send.try_send(job);
        }
    }

    pub(crate) fn take(&self) -> Vec<Done> {
        self.worker
            .as_ref()
            .map_or_else(Vec::new, |(_, receive)| receive.try_iter().collect())
    }
}

/// Move image payloads out before normal tool processing discards them. Inspect
/// only actual result content, never a path inferred from a tool's title/input.
pub(crate) fn extract(update: &mut SessionUpdate) -> Option<(String, Vec<Source>)> {
    let (id, content) = match update {
        SessionUpdate::ToolCall(call) => (&call.tool_call_id, &mut call.content),
        SessionUpdate::ToolCallUpdate(call) => (&call.tool_call_id, call.fields.content.as_mut()?),
        _ => return None,
    };
    let mut sources = Vec::new();
    let mut bytes = 0;
    for item in content.iter_mut().take(256) {
        let ToolCallContent::Content(item) = item else {
            continue;
        };
        let source = match &mut item.content {
            ContentBlock::Image(image) if image.mime_type.starts_with("image/") => {
                inline(&mut image.data, &mut bytes)
            }
            ContentBlock::Resource(resource) => match &mut resource.resource {
                EmbeddedResourceResource::BlobResourceContents(blob)
                    if blob
                        .mime_type
                        .as_deref()
                        .is_some_and(|mime| mime.starts_with("image/")) =>
                {
                    inline(&mut blob.blob, &mut bytes)
                }
                _ => None,
            },
            ContentBlock::ResourceLink(link) if link.uri.len() <= 4096 => local_path(&link.uri)
                .filter(|path| {
                    link.mime_type
                        .as_deref()
                        .is_some_and(|mime| mime.starts_with("image/"))
                        || image_path(path)
                })
                .map(Source::File),
            _ => None,
        };
        if let Some(source) = source {
            sources.push(source);
        }
        if sources.len() == COUNT {
            break;
        }
    }
    (!sources.is_empty()).then(|| (id.0.to_string(), sources))
}

/// The image in `update`, if it's a piece of the agent's words that's an
/// image given inline, which it leaves empty: within the same size caps as
/// a tool's.
pub(crate) fn extract_message(update: &mut SessionUpdate) -> Option<Source> {
    let SessionUpdate::AgentMessageChunk(chunk) = update else {
        return None;
    };
    let ContentBlock::Image(image) = &mut chunk.content else {
        return None;
    };
    if !image.mime_type.starts_with("image/") {
        return None;
    }
    inline(&mut image.data, &mut 0)
}

fn inline(data: &mut String, bytes: &mut usize) -> Option<Source> {
    if data.is_empty() || data.len() > BYTES.saturating_sub(*bytes) {
        return None;
    }
    *bytes += data.len();
    Some(Source::Inline(std::mem::take(data)))
}

fn image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
            )
        })
}

/// Antigravity 1.4.3's native `view_file` reports the viewed file in ACP
/// locations, followed by a successful completion with no result content.
/// Only that completed native read is adapted; ordinary tool inputs/titles
/// remain insufficient. Decoding and original storage use the same worker.
pub(crate) fn antigravity_result(
    call: &agent_client_protocol_schema::v1::ToolCallUpdate,
    tool: &clackatoo_protocol::Tool,
) -> Option<Source> {
    use agent_client_protocol_schema::v1::ToolCallStatus;
    use clackatoo_protocol::{ToolKind, ToolStatus};
    if call.fields.status != Some(ToolCallStatus::Completed)
        || call.fields.content.is_some()
        || tool.kind != ToolKind::Read
        || tool.status != ToolStatus::Running
        || tool.title != "Running view_file"
        || tool.locations.len() != 1
        || tool.locations[0].path.len() > 4096
    {
        return None;
    }
    let path = local_path(&tool.locations[0].path)?;
    image_path(&path).then_some(Source::File(path))
}

fn local_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let Some(path) = uri.strip_prefix("file://") else {
        return (!uri.contains("://") && !uri.contains('\0')).then(|| PathBuf::from(uri));
    };
    let path = path.strip_prefix("localhost").unwrap_or(path);
    if !path.starts_with('/') {
        return None;
    }
    let mut decoded = Vec::new();
    let mut bytes = path.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        let byte = if byte == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?
        } else {
            byte
        };
        if byte == 0 {
            return None;
        }
        decoded.push(byte);
    }
    Some(std::ffi::OsString::from_vec(decoded).into())
}

fn decode(source: Source, cwd: &Path) -> Option<(ChatPreview, Vec<u8>)> {
    let bytes = match source {
        Source::Inline(data) => STANDARD.decode(data).ok()?,
        Source::File(path) => {
            let file = crate::regular::open(&cwd.join(path), crate::regular::Links::Follow).ok()?;
            if file.metadata().ok()?.len() > BYTES as u64 {
                return None;
            }
            let mut bytes = Vec::new();
            file.take(BYTES as u64 + 1).read_to_end(&mut bytes).ok()?;
            bytes
        }
    };
    if bytes.len() > BYTES {
        return None;
    }
    // What the image layer keeps, and BMP, kept as a PNG.
    let format = image::guess_format(&bytes).ok()?;
    if !matches!(
        format,
        image::ImageFormat::Png
            | image::ImageFormat::Jpeg
            | image::ImageFormat::Gif
            | image::ImageFormat::WebP
            | image::ImageFormat::Bmp
    ) {
        return None;
    }
    let reader = || image::ImageReader::with_format(Cursor::new(&bytes), format);
    let (width, height) = reader().into_dimensions().ok()?;
    if !fits(width, height) {
        return None;
    }
    let mut reader = reader();
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    let most = u32::try_from(ChatImage::MAX_PIXELS).ok();
    limits.max_image_width = most;
    limits.max_image_height = most;
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    let bytes = if format == image::ImageFormat::Bmp {
        let rgba = decoded.to_rgba8();
        crate::images::png_of(rgba.width(), rgba.height(), rgba.as_raw()).ok()?
    } else {
        bytes
    };
    let image = decoded
        .thumbnail(ChatPreview::SIDE, ChatPreview::SIDE)
        .to_rgba8();
    Some((
        ChatPreview {
            original: 0,
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw().into(),
            image: None,
        },
        bytes,
    ))
}

/// Whether an image `width` by `height` is one a chat takes: no more
/// pixels than one pasted may have.
fn fits(width: u32, height: u32) -> bool {
    width > 0 && height > 0 && u64::from(width) * u64::from(height) <= ChatImage::MAX_PIXELS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_has_as_many_pixels_as_one_pasted_may() {
        assert!(fits(4096, 4096));
        assert!(!fits(4097, 4096));
        assert!(!fits(0, 1));
    }

    fn png() -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(512, 256, image::Rgba([20, 40, 60, 255]))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn inline_and_local_results_produce_the_same_bounded_preview() {
        let bytes = png();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("picture.png");
        std::fs::write(&path, &bytes).unwrap();
        let inline = decode(Source::Inline(STANDARD.encode(&bytes)), dir.path()).unwrap();
        let linked = decode(Source::File(PathBuf::from("picture.png")), dir.path()).unwrap();
        assert_eq!(inline, linked);
        assert_eq!((inline.0.width, inline.0.height), (256, 128));
        assert!(inline.0.valid());
        assert!(decode(Source::Inline("not base64".into()), dir.path()).is_none());
        assert!(decode(Source::File(dir.path().to_owned()), dir.path()).is_none());
        assert!(decode(Source::File(PathBuf::from("/dev/zero")), dir.path()).is_none());
        assert!(local_path("https://example.com/image.png").is_none());
        assert!(local_path("file://elsewhere/image.png").is_none());
        assert_eq!(
            local_path("file:///tmp/a%20b.png"),
            Some(PathBuf::from("/tmp/a b.png"))
        );
    }

    #[test]
    fn adapter_shapes_are_generic_but_inputs_and_titles_are_never_read() {
        let mut codex: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call", "toolCallId": "image", "title": "View Image /tmp/actual.png", "kind": "read", "status": "completed",
            "content": [{"type":"content", "content":{"type":"resource_link", "uri":"/tmp/actual.png", "name":"picture"}}]
        })).unwrap();
        assert!(
            matches!(extract(&mut codex).unwrap().1.as_slice(), [Source::File(path)] if path == Path::new("/tmp/actual.png"))
        );
        let mut claude: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "read", "status": "completed",
            "content": [{"type":"content", "content":{"type":"image", "data": STANDARD.encode(png()), "mimeType":"image/png"}}]
        })).unwrap();
        let (_, sources) = extract(&mut claude).unwrap();
        assert!(
            decode(sources.into_iter().next().unwrap(), Path::new("/"))
                .unwrap()
                .0
                .valid()
        );
        assert!(
            extract(&mut claude).is_none(),
            "payload was moved, not cloned"
        );
        let mut input_only: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate":"tool_call", "toolCallId":"other", "title":"View Image /secret.png", "kind":"read", "status":"completed", "rawInput":{"path":"/secret.png"}
        })).unwrap();
        assert!(extract(&mut input_only).is_none());
    }
}

/// The handles chats' thumbnails name their originals by, each with the
/// image once the daemon's store keeps it (CLACKA-239), whose bytes are
/// the store's. A chat names at most 128; the store lets the images
/// themselves go as its limits say.
#[derive(Debug, Default)]
pub(crate) struct Originals {
    next: u64,
    images: std::collections::VecDeque<(u64, Option<ImageId>)>,
    /// Images given handles and not yet sent to the store.
    waiting: Vec<(u64, crate::images::Source)>,
}

impl Originals {
    /// Most handles kept.
    const MOST: usize = 128;

    /// A handle for the image `source` holds, which is to be kept in the
    /// store: 0, none, for one that can't be.
    pub(crate) fn insert(&mut self, source: crate::images::Source) -> u64 {
        let Some(next) = self.next.checked_add(1) else {
            return 0;
        };
        self.next = next;
        while self.images.len() >= Self::MOST {
            self.images.pop_front();
        }
        self.images.push_back((next, None));
        self.waiting.push((next, source));
        next
    }

    /// The images given handles and not yet sent to the store, to send.
    pub(crate) fn take_waiting(&mut self) -> Vec<(u64, crate::images::Source)> {
        std::mem::take(&mut self.waiting)
    }

    /// The store keeps the image with handle `original` as `image`: false
    /// if the handle's gone.
    pub(crate) fn kept(&mut self, original: u64, image: ImageId) -> bool {
        match self
            .images
            .iter_mut()
            .find(|(handle, _)| *handle == original)
        {
            Some((_, kept)) => {
                *kept = Some(image);
                true
            }
            None => false,
        }
    }

    /// The image with handle `id`, if the store keeps it.
    pub(crate) fn get(&self, id: u64) -> Option<ImageId> {
        (self.images.iter())
            .find(|(key, _)| *key == id)
            .and_then(|(_, image)| *image)
    }

    /// The handles, for the handover, with the images the store keeps for
    /// them: the bytes are the store's, handed over on their own.
    pub(crate) fn hand_over(
        &self,
        chat: u32,
        references: Vec<(u32, Option<String>, u32, u64)>,
    ) -> (crate::handover::ImageOriginals, crate::handover::ChatImages) {
        let handles = (self.images.iter())
            .filter_map(|&(handle, image)| Some((handle, image?.0)))
            .collect();
        (
            crate::handover::ImageOriginals {
                chat,
                next: self.next,
                images: Vec::new(),
                references,
            },
            crate::handover::ChatImages { chat, handles },
        )
    }

    /// The handles handed over in `record`, with the images `handles`
    /// says the store keeps for them, from a build of version 47 or later;
    /// or, from an earlier one, the originals `record` holds, to be kept.
    pub(crate) fn resume(
        record: &mut crate::handover::ImageOriginals,
        handles: Option<&crate::handover::ChatImages>,
    ) -> Self {
        let mut store = Self {
            next: record.next,
            ..Self::default()
        };
        if let Some(handles) = handles {
            for &(id, image) in handles.handles.iter().rev().take(Self::MOST) {
                if id != 0 && id <= store.next {
                    store.images.push_front((id, Some(ImageId(image))));
                }
            }
            return store;
        }
        for (id, data) in std::mem::take(&mut record.images)
            .into_iter()
            .rev()
            .take(Self::MOST)
        {
            if id == 0 || id > store.next || data.len() > BYTES * 4 / 3 + 4 {
                continue;
            }
            store.images.push_front((id, None));
            store
                .waiting
                .push((id, crate::images::Source::Base64(data)));
        }
        store
    }
}

#[cfg(test)]
mod original_tests {
    use super::*;
    use crate::images::Source;

    #[test]
    fn handles_are_bounded_and_never_alias_images_let_go() {
        let mut originals = Originals::default();
        let first = originals.insert(Source::Bytes(vec![1]));
        for _ in 0..Originals::MOST {
            originals.insert(Source::Bytes(vec![2]));
        }
        assert_eq!(originals.take_waiting().len(), Originals::MOST + 1);
        assert!(!originals.kept(first, ImageId([1; 16])));
        let last = originals.next;
        assert!(originals.kept(last, ImageId([2; 16])));
        assert_eq!(originals.get(last), Some(ImageId([2; 16])));
        assert_eq!(originals.get(first), None);
        let (mut saved, handles) = originals.hand_over(1, Vec::new());
        assert!(saved.images.is_empty(), "the bytes are the store's");
        let mut resumed = Originals::resume(&mut saved, Some(&handles));
        assert_eq!(resumed.get(last), Some(ImageId([2; 16])));
        assert_eq!(resumed.insert(Source::Bytes(vec![3])), last + 1);
        assert!(resumed.get(0).is_none());
    }

    #[test]
    fn originals_handed_over_by_an_older_build_are_kept_again() {
        let mut record = crate::handover::ImageOriginals {
            chat: 1,
            next: 3,
            images: vec![(2, "cG5n".into()), (9, "bm8=".into())],
            references: Vec::new(),
        };
        let mut resumed = Originals::resume(&mut record, None);
        let waiting = resumed.take_waiting();
        assert_eq!(waiting.len(), 1, "a handle past `next` is no handle");
        assert_eq!(waiting[0].0, 2);
        assert!(matches!(&waiting[0].1, Source::Base64(data) if data == "cG5n"));
    }
}
