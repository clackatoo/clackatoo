//! Image tool results: bounded decoding on one worker, never on the event loop.
//! Inline ACP images and local resource links use the same thumbnail path.
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use agent_client_protocol_schema::v1::{
    ContentBlock, EmbeddedResourceResource, SessionUpdate, ToolCallContent,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use clackatoo_protocol::{ChatPreview, TerminalId, ToolContent};

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
    pub turn: usize,
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
    pub images: Vec<(ChatPreview, String)>,
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
                "png" | "jpg" | "jpeg" | "bmp"
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

fn decode(source: Source, cwd: &Path) -> Option<(ChatPreview, String)> {
    use std::os::unix::fs::OpenOptionsExt;
    let bytes = match source {
        Source::Inline(data) => STANDARD.decode(data).ok()?,
        Source::File(path) => {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(cwd.join(path))
                .ok()?;
            let meta = file.metadata().ok()?;
            if !meta.is_file() || meta.len() > BYTES as u64 {
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
    let format = image::guess_format(&bytes).ok()?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::Bmp
    ) {
        return None;
    }
    let reader = || image::ImageReader::with_format(Cursor::new(&bytes), format);
    let (width, height) = reader().into_dimensions().ok()?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 16_000_000 {
        return None;
    }
    let mut reader = reader();
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits.max_image_width = Some(16_000_000);
    limits.max_image_height = Some(16_000_000);
    reader.limits(limits);
    let image = reader
        .decode()
        .ok()?
        .thumbnail(ChatPreview::SIDE, ChatPreview::SIDE)
        .to_rgba8();
    Some((
        ChatPreview {
            original: 0,
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw().into(),
        },
        STANDARD.encode(bytes),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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

/// Compressed originals are separate from snapshots. A chat retains at most
/// 32 MiB / 128 originals; fetching never touches the filesystem or decodes.
#[derive(Debug, Default)]
pub(crate) struct Originals {
    next: u64,
    bytes: usize,
    images: std::collections::VecDeque<(u64, String)>,
}

impl Originals {
    pub(crate) fn insert(&mut self, data: String) -> u64 {
        if data.is_empty() || data.len() > BYTES * 4 / 3 + 4 {
            return 0;
        }
        let Some(next) = self.next.checked_add(1) else {
            return 0;
        };
        self.next = next;
        while self.bytes + data.len() > 32 * 1024 * 1024 || self.images.len() >= 128 {
            let Some((_, old)) = self.images.pop_front() else {
                break;
            };
            self.bytes -= old.len();
        }
        self.bytes += data.len();
        self.images.push_back((next, data));
        next
    }

    pub(crate) fn get(&self, id: u64) -> Option<String> {
        self.images
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, data)| data.clone())
    }

    pub(crate) fn hand_over(
        &self,
        chat: u32,
        references: Vec<(u32, Option<String>, u32, u64)>,
    ) -> crate::handover::ImageOriginals {
        crate::handover::ImageOriginals {
            chat,
            next: self.next,
            images: self.images.iter().cloned().collect(),
            references,
        }
    }

    pub(crate) fn resume(record: &mut crate::handover::ImageOriginals) -> Self {
        let mut store = Self {
            next: record.next,
            ..Self::default()
        };
        for (id, data) in std::mem::take(&mut record.images)
            .into_iter()
            .rev()
            .take(128)
        {
            if id == 0
                || id > store.next
                || data.len() > BYTES * 4 / 3 + 4
                || store.bytes + data.len() > 32 * 1024 * 1024
            {
                continue;
            }
            store.bytes += data.len();
            store.images.push_front((id, data));
        }
        store
    }
}

#[cfg(test)]
mod original_tests {
    use super::*;
    #[test]
    fn retained_originals_are_bounded_and_ids_never_alias_evicted_images() {
        let mut originals = Originals::default();
        let first = originals.insert("a".repeat(8 * 1024 * 1024));
        for _ in 0..4 {
            originals.insert("b".repeat(8 * 1024 * 1024));
        }
        assert!(originals.get(first).is_none());
        assert_eq!(originals.bytes, 32 * 1024 * 1024);
        let mut saved = originals.hand_over(1, Vec::new());
        let mut resumed = Originals::resume(&mut saved);
        assert_eq!(resumed.bytes, originals.bytes);
        assert_eq!(resumed.get(5), originals.get(5));
        assert_eq!(resumed.insert("c".into()), 6);
        assert!(resumed.get(0).is_none());
    }
}
