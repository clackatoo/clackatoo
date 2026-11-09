//! The pictures kept on the GPU, for every window of the process to draw
//! (CLACKA-239): one texture each, by what it is ([`TextureKey`]), kept
//! from one frame to the next, and let go of oldest first once they take
//! more than [`BUDGET`] bytes. The windows share one GPU device, so a
//! picture shown in two windows is uploaded once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use clackatoo_protocol::ImageId;

/// How many bytes of pictures are kept on the GPU, beyond those drawn in
/// the frame: past it, those drawn longest ago are let go.
pub(crate) const BUDGET: u64 = 256 * 1024 * 1024;

/// How many frames, of any window's, a slot's picture is kept undrawn:
/// windows count frames together, so one window drawing doesn't let go of
/// another's.
const SLOT_FRAMES: u64 = 16;

/// What a texture is kept by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TextureKey {
    /// A picture a pane changes as it goes, as a browser pane's page: by
    /// the pane, kept only while it's drawn, or was a few frames ago.
    Slot(u64),
    /// An image of the daemon's (see `crate::images`), decoded at a side.
    Image(ImageId, u32),
    /// Pixels the window has, as a thumbnail an older daemon sent, by the
    /// picture's own number, which is never reused.
    Pixels(u64),
}

/// A picture on the GPU.
#[derive(Debug)]
pub(super) struct Kept {
    pub(super) texture: wgpu::Texture,
    pub(super) view: wgpu::TextureView,
    pub(super) size: [u32; 2],
    /// The number of the picture uploaded to it.
    pub(super) picture: u64,
    /// Which texture this is, never reused, so a window knows when what
    /// it bound is gone.
    pub(super) generation: u64,
    /// The frame it was last drawn in.
    pub(super) used: u64,
}

impl Kept {
    fn bytes(&self) -> u64 {
        u64::from(self.size[0]) * u64::from(self.size[1]) * 4
    }
}

/// The pictures on the GPU.
#[derive(Debug, Default)]
pub(crate) struct Textures {
    pub(super) kept: HashMap<TextureKey, Kept>,
    /// The frames drawn, counted.
    pub(super) frame: u64,
    generations: u64,
    bytes: u64,
}

/// The textures, shared by the windows of a process.
pub(crate) type Shared = Arc<Mutex<Textures>>;

/// Textures of their own, as for a renderer drawing alone.
pub(crate) fn shared() -> Shared {
    Arc::new(Mutex::new(Textures::default()))
}

/// Locks `shared`, whatever a panic left there: textures are only ever
/// kept or let go.
pub(super) fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Textures> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Textures {
    /// Starts a frame.
    pub(super) fn begin(&mut self) -> u64 {
        self.frame += 1;
        self.frame
    }

    /// Keeps `kept`, by `key`, numbered afresh.
    pub(super) fn insert(&mut self, key: TextureKey, mut kept: Kept) {
        self.generations += 1;
        kept.generation = self.generations;
        self.bytes += kept.bytes();
        if let Some(old) = self.kept.insert(key, kept) {
            self.bytes -= old.bytes();
        }
    }

    /// Lets go of what wasn't drawn in this frame: slots not drawn in the
    /// last few, and the rest oldest first once they take more than
    /// [`BUDGET`].
    pub(super) fn end(&mut self, frame: u64) {
        let bytes = &mut self.bytes;
        self.kept.retain(|key, kept| {
            let keep = kept.used + SLOT_FRAMES >= frame || !matches!(key, TextureKey::Slot(_));
            if !keep {
                *bytes -= kept.bytes();
            }
            keep
        });
        while self.bytes > BUDGET {
            let Some((&key, _)) = (self.kept.iter())
                .filter(|(_, kept)| kept.used != frame)
                .min_by_key(|(_, kept)| kept.used)
            else {
                break;
            };
            if let Some(kept) = self.kept.remove(&key) {
                self.bytes -= kept.bytes();
            }
        }
    }

    /// How many bytes they take.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Whether a picture is kept by `key`.
    #[cfg(test)]
    pub(crate) fn has(&self, key: TextureKey) -> bool {
        self.kept.contains_key(&key)
    }
}

/// A texture `size` pixels, to keep a picture in.
pub(super) fn new_texture(device: &wgpu::Device, [width, height]: [u32; 2]) -> Kept {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("image"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Kept {
        texture,
        view,
        size: [width, height],
        picture: u64::MAX,
        generation: 0,
        used: 0,
    }
}
