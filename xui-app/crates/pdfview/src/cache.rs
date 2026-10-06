//! Rendered tiles kept for repainting, least recently drawn dropped first
//! when they outgrow a byte budget.

use std::cell::Cell;
use std::collections::HashMap;

use xui_core::Image;

/// What a rendered image is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A [`TILE`](crate::layout::TILE)-sized piece of a page at a scale
    /// (the `f32` bits of pixels per point).
    Tile {
        page: u32,
        scale: u32,
        col: u32,
        row: u32,
    },
    /// A whole page drawn small, shown scaled while its tiles render.
    Preview { page: u32 },
}

impl Key {
    pub fn tile(page: usize, scale: f32, col: u32, row: u32) -> Key {
        Key::Tile {
            page: page as u32,
            scale: scale.to_bits(),
            col,
            row,
        }
    }

    pub fn page(&self) -> usize {
        match *self {
            Key::Tile { page, .. } | Key::Preview { page } => page as usize,
        }
    }
}

struct Entry {
    image: Image,
    bytes: usize,
    used: Cell<u64>,
}

/// The tiles, with the clock that orders them by last use.
pub struct TileCache {
    entries: HashMap<Key, Entry>,
    bytes: usize,
    budget: usize,
    clock: Cell<u64>,
}

impl TileCache {
    pub fn new(budget: usize) -> TileCache {
        TileCache {
            entries: HashMap::new(),
            bytes: 0,
            budget,
            clock: Cell::new(0),
        }
    }

    /// The image for `key`, marked as just used.
    pub fn get(&self, key: &Key) -> Option<&Image> {
        let entry = self.entries.get(key)?;
        let now = self.clock.get() + 1;
        self.clock.set(now);
        entry.used.set(now);
        Some(&entry.image)
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.entries.contains_key(key)
    }

    /// Stores `image` under `key`, then drops the least recently used tiles
    /// (never one in `keep`, the tiles on screen) until the budget holds.
    pub fn insert(&mut self, key: Key, image: Image, keep: &dyn Fn(&Key) -> bool) {
        let bytes = image.width() as usize * image.height() as usize * 4;
        let now = self.clock.get() + 1;
        self.clock.set(now);
        if let Some(old) = self.entries.insert(
            key,
            Entry {
                image,
                bytes,
                used: Cell::new(now),
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        if self.bytes <= self.budget {
            return;
        }
        let mut victims: Vec<(u64, Key)> = self
            .entries
            .iter()
            .filter(|(k, _)| **k != key && !keep(k))
            .map(|(k, e)| (e.used.get(), *k))
            .collect();
        victims.sort_unstable_by_key(|(used, _)| *used);
        for (_, victim) in victims {
            if self.bytes <= self.budget {
                break;
            }
            if let Some(entry) = self.entries.remove(&victim) {
                self.bytes -= entry.bytes;
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Image {
        Image::from_rgba(16, 16, vec![255; 16 * 16 * 4]).unwrap()
    }

    #[test]
    fn the_least_recently_used_tile_goes_first() {
        let one = 16 * 16 * 4;
        let mut cache = TileCache::new(3 * one);
        let k = |n| Key::tile(n, 1.0, 0, 0);
        for n in 0..3 {
            cache.insert(k(n), image(), &|_| false);
        }
        assert!(
            cache.get(&k(0)).is_some(),
            "touch 0, so 1 is now the oldest"
        );
        cache.insert(k(3), image(), &|_| false);
        assert!(!cache.contains(&k(1)));
        assert!(cache.contains(&k(0)) && cache.contains(&k(2)) && cache.contains(&k(3)));
        assert_eq!(cache.bytes(), 3 * one);
    }

    #[test]
    fn tiles_on_screen_are_kept_over_budget() {
        let mut cache = TileCache::new(16 * 16 * 4);
        let k = |n| Key::tile(n, 1.0, 0, 0);
        cache.insert(k(0), image(), &|_| false);
        cache.insert(k(1), image(), &|key| key.page() == 0);
        assert_eq!(cache.len(), 2, "both are on screen, so the budget gives");
        cache.insert(k(2), image(), &|_| false);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(&k(2)));
        cache.clear();
        assert!(cache.is_empty() && cache.bytes() == 0);
    }

    #[test]
    fn replacing_a_key_counts_its_bytes_once() {
        let mut cache = TileCache::new(1 << 20);
        cache.insert(Key::Preview { page: 0 }, image(), &|_| false);
        cache.insert(Key::Preview { page: 0 }, image(), &|_| false);
        assert_eq!(cache.bytes(), 16 * 16 * 4);
    }
}
