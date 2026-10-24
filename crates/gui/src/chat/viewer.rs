//! Shared, on-demand original loading for local drafts and daemon thumbnails.
//! One bounded decoder serves the window; closing a viewer cancels queued work.
use crate::render::Image;
use clackatoo_protocol::{ChatImage, ChatPreview, Response};
use std::sync::{
    Arc, OnceLock, Weak,
    atomic::{AtomicU64, Ordering},
    mpsc,
};

pub(super) struct Viewer {
    pub preview: ChatPreview,
    pub picture: Option<Arc<Image>>,
    pub status: String,
    pub serial: u64,
    request: Option<u64>,
    decoding: Option<mpsc::Receiver<Result<Arc<Image>, String>>>,
    alive: Arc<()>,
}

impl Viewer {
    pub(super) fn new(preview: ChatPreview, local: Option<String>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let request = (local.is_none() && preview.original != 0).then_some(preview.original);
        let mut viewer = Self {
            preview,
            picture: None,
            serial: NEXT.fetch_add(1, Ordering::Relaxed),
            request,
            status: if local.is_some() || request.is_some() {
                "Loading original…"
            } else {
                "Original unavailable · thumbnail only"
            }
            .into(),
            decoding: None,
            alive: Arc::new(()),
        };
        if let Some(data) = local {
            viewer.decode(data);
        }
        viewer
    }

    pub(super) fn take_request(&mut self) -> Option<(u64, u64)> {
        self.request.take().map(|id| (self.serial, id))
    }

    pub(super) fn answer(&mut self, serial: u64, response: Response) {
        if serial != self.serial {
            return;
        }
        match response {
            Response::Text(data) => self.decode(data),
            _ => self.status = "Original unavailable · thumbnail only".into(),
        }
    }

    fn decode(&mut self, data: String) {
        if data.len() > 12 * 1024 * 1024 {
            self.status = "Original exceeds image limit · thumbnail only".into();
            return;
        }
        let (send, receive) = mpsc::channel();
        let job = Job {
            data,
            alive: Arc::downgrade(&self.alive),
            send,
        };
        if decoder()
            .as_ref()
            .is_some_and(|worker| worker.try_send(job).is_ok())
        {
            self.decoding = Some(receive);
        } else {
            self.status = "Image decoder busy · reopen to retry".into();
        }
    }

    pub(super) fn offline(&mut self) {
        if self.picture.is_none() && self.decoding.is_none() {
            self.request = None;
            self.status = "Disconnected · reopen to retry".into();
        }
    }

    pub(super) fn loading(&self) -> bool {
        self.decoding.is_some()
    }

    pub(super) fn poll(&mut self) -> bool {
        let Some(receive) = &self.decoding else {
            return false;
        };
        match receive.try_recv() {
            Ok(Ok(image)) => {
                self.status = format!("{} × {}", image.width, image.height);
                self.picture = Some(image);
            }
            Ok(Err(error)) => self.status = error,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.status = "Image decoder stopped · thumbnail only".into();
            }
        }
        self.decoding = None;
        true
    }
}

struct Job {
    data: String,
    alive: Weak<()>,
    send: mpsc::Sender<Result<Arc<Image>, String>>,
}

fn decoder() -> Option<&'static mpsc::SyncSender<Job>> {
    static WORKER: OnceLock<Option<mpsc::SyncSender<Job>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (send, receive) = mpsc::sync_channel::<Job>(2);
            std::thread::Builder::new()
                .name("chat-originals".into())
                .spawn(move || {
                    while let Ok(job) = receive.recv() {
                        if job.alive.strong_count() == 0 {
                            continue;
                        }
                        let result = decode(&job.data)
                            .ok_or_else(|| "Cannot decode original · thumbnail only".into());
                        let _ = job.send.send(result);
                    }
                })
                .ok()
                .map(|_| send)
        })
        .as_ref()
}

fn decode(data: &str) -> Option<Arc<Image>> {
    static NEXT: AtomicU64 = AtomicU64::new(1_u64 << 62);
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::io::Cursor;
    if data.len() > 12 * 1024 * 1024 {
        return None;
    }
    let bytes = STANDARD.decode(data).ok()?;
    let format = image::guess_format(&bytes).ok()?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::Bmp
    ) {
        return None;
    }
    let reader = || image::ImageReader::with_format(Cursor::new(&bytes), format);
    let (width, height) = reader().into_dimensions().ok()?;
    if width == 0
        || height == 0
        || width > crate::render::MAX_IMAGE_SIDE
        || height > crate::render::MAX_IMAGE_SIDE
        || u64::from(width) * u64::from(height) > ChatImage::MAX_PIXELS
    {
        return None;
    }
    let mut reader = reader();
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let rgba = reader.decode().ok()?.to_rgba8().into_raw();
    Some(Arc::new(Image {
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        width,
        height,
        rgba,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn attachment() -> ChatImage {
        crate::clipboard_image::encode(&arboard::ImageData {
            width: 1024,
            height: 512,
            bytes: (0..1024 * 512)
                .flat_map(|pixel| {
                    if pixel % 2 == 0 {
                        [255, 0, 0, 255]
                    } else {
                        [0, 0, 255, 255]
                    }
                })
                .collect::<Vec<_>>()
                .into(),
        })
        .unwrap()
    }
    #[test]
    fn original_decoding_keeps_resolution_and_single_pixel_detail() {
        let source = attachment();
        assert_eq!(source.preview.as_ref().unwrap().width, 256);
        let original = decode(&source.data).unwrap();
        assert_eq!((original.width, original.height), (1024, 512));
        assert_eq!(&original.rgba[..8], &[255, 0, 0, 255, 0, 0, 255, 255]);
        assert!(decode("not base64").is_none());
    }
    #[test]
    fn reopen_discards_old_answers_and_unavailable_originals_are_explicit() {
        let source = attachment();
        let mut preview = source.preview.unwrap();
        preview.original = 7;
        let mut first = Viewer::new(preview.clone(), None);
        assert_eq!(first.take_request(), Some((first.serial, 7)));
        assert!(first.take_request().is_none());
        let mut second = Viewer::new(preview, None);
        second.answer(first.serial, Response::Text(source.data));
        assert!(!second.loading());
        second.answer(
            second.serial,
            Response::Error(clackatoo_protocol::Error::Unsupported),
        );
        assert!(second.status.contains("unavailable"));
    }
}
