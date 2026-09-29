#![forbid(unsafe_code)]

//! The decoded-image cache behind
//! [`Canvas::draw_image`](xui_core::backend::Canvas::draw_image): the first
//! draw of an [`Image`] premultiplies it into a `Pixmap` once, and every later
//! draw — the same row icon on every repaint — reuses it.
//!
//! A fresh image per frame (a GL readback) never hits and only churns the
//! LRU, which the byte budget bounds.

use std::collections::HashMap;
use std::rc::Rc;

use tiny_skia::Pixmap;
use xui_core::image::Image;

/// The default budget, in bytes of premultiplied RGBA.
const DEFAULT_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// One cached image and its LRU bookkeeping.
struct Entry {
    pixmap: Rc<Pixmap>,
    bytes: usize,
    last_used: u64,
}

/// A surface's decoded images, keyed by each image's identity (which clones
/// keep, so the same model image hits on every paint) and bounded by an LRU
/// eviction over bytes.
pub(crate) struct ImageCache {
    images: HashMap<u64, Entry>,
    clock: u64,
    bytes: usize,
    budget: usize,
}

impl ImageCache {
    pub(crate) fn new() -> ImageCache {
        ImageCache {
            images: HashMap::new(),
            clock: 0,
            bytes: 0,
            budget: DEFAULT_BUDGET_BYTES,
        }
    }

    /// The premultiplied pixmap for `image`, uploading it on first use. An
    /// image bigger than the whole budget draws through a one-off upload (the
    /// returned pixmap, uncached) rather than evicting everything for a single
    /// blit.
    pub(crate) fn pixmap(&mut self, image: &Image) -> Option<Rc<Pixmap>> {
        let id = image.id();
        let last_used = self.tick();
        if let Some(entry) = self.images.get_mut(&id) {
            entry.last_used = last_used;
            return Some(Rc::clone(&entry.pixmap));
        }
        let pixmap = Rc::new(premultiplied(image)?);
        let bytes = pixmap.data().len();
        if bytes <= self.budget {
            self.images.insert(
                id,
                Entry {
                    pixmap: Rc::clone(&pixmap),
                    bytes,
                    last_used,
                },
            );
            self.bytes += bytes;
            self.evict();
        }
        Some(pixmap)
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Evicts least-recently-used images until the budget is met. The entry
    /// just inserted carries the newest tick, so it survives unless the image
    /// alone is over budget — excluded before the insert.
    fn evict(&mut self) {
        while self.bytes > self.budget && !self.images.is_empty() {
            let oldest = self
                .images
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(&id, _)| id);
            if let Some(id) = oldest
                && let Some(entry) = self.images.remove(&id)
            {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

/// Uploads `image` to a tiny-skia pixmap, premultiplying its straight alpha.
fn premultiplied(image: &Image) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(image.width(), image.height())?;
    let source = image.pixels().as_chunks::<4>().0;
    for (destination, pixel) in pixmap
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(source)
    {
        let alpha = pixel[3] as u32;
        let scale = |channel: u8| ((channel as u32 * alpha + 127) / 255) as u8;
        destination[0] = scale(pixel[0]);
        destination[1] = scale(pixel[1]);
        destination[2] = scale(pixel[2]);
        destination[3] = pixel[3];
    }
    Some(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, byte: u8) -> Image {
        let pixels = [byte, byte, byte, 255].repeat((width * height) as usize);
        Image::from_rgba(width, height, pixels).unwrap()
    }

    #[test]
    fn the_same_image_uploads_once() {
        let mut cache = ImageCache::new();
        let image = solid(4, 4, 0x11);

        assert!(cache.pixmap(&image).is_some());
        assert!(cache.pixmap(&image).is_some());
        assert_eq!(cache.images.len(), 1, "a repeat draw must not upload again");

        // A clone keeps the identity, so it is the same cache entry.
        assert!(cache.pixmap(&image.clone()).is_some());
        assert_eq!(cache.images.len(), 1);
    }

    #[test]
    fn an_evicted_image_uploads_again() {
        // Two images of one budget each: the second insert evicts the first.
        let mut cache = ImageCache::new();
        cache.budget = 4 * 4 * 4;
        let first = solid(4, 4, 0x11);
        let second = solid(4, 4, 0x22);

        assert!(cache.pixmap(&first).is_some());
        assert!(cache.pixmap(&second).is_some());
        assert_eq!(cache.images.len(), 1, "the budget fits only one image");

        let back = cache.pixmap(&first).expect("re-uploaded");
        let pixels = back.data();
        assert_eq!(&pixels[..4], &[0x11, 0x11, 0x11, 0xFF][..]);
    }

    #[test]
    fn an_image_over_the_budget_draws_uncached() {
        // One image of twice the budget: it draws, and caches nothing.
        let mut cache = ImageCache::new();
        cache.budget = 4 * 4 * 2;
        let image = solid(4, 4, 0x33);

        assert!(cache.pixmap(&image).is_some());
        assert_eq!(cache.bytes, 0);
        assert!(cache.images.is_empty());
    }
}
