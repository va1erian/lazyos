//! Decoded pictures on the UI side (docs/lazysky-plan.md §7.5): a byte
//! budget of ready-to-draw `xui_core::Image`s, least recently drawn out
//! first, never one the view is showing.
//!
//! The pictures arrive from the core already at display size; this only
//! wraps them for the canvas and remembers them.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use lazysky_core::{Bitmap, ImageKey};
use xui_core::Image;

/// The cache's budget in decoded RGBA bytes.
pub const BUDGET_BYTES: usize = 64 * 1024 * 1024;

struct Entry {
    image: Image,
    bytes: usize,
    /// When it was last asked for (a tick of the cache's own counter).
    used: Cell<u64>,
}

pub struct ImageCache {
    entries: HashMap<ImageKey, Entry>,
    /// Pictures that could not be had: shown as a grey box, not retried
    /// until a refresh clears them.
    failed: HashSet<ImageKey>,
    bytes: usize,
    budget: usize,
    clock: Cell<u64>,
}

impl Default for ImageCache {
    fn default() -> ImageCache {
        ImageCache::with_budget(BUDGET_BYTES)
    }
}

impl ImageCache {
    pub fn with_budget(budget: usize) -> ImageCache {
        ImageCache {
            entries: HashMap::new(),
            failed: HashSet::new(),
            bytes: 0,
            budget,
            clock: Cell::new(0),
        }
    }

    /// The picture for `key`, marking it used.
    pub fn get(&self, key: &ImageKey) -> Option<&Image> {
        let entry = self.entries.get(key)?;
        let now = self.clock.get() + 1;
        self.clock.set(now);
        entry.used.set(now);
        Some(&entry.image)
    }

    pub fn contains(&self, key: &ImageKey) -> bool {
        self.entries.contains_key(key)
    }

    pub fn has_failed(&self, key: &ImageKey) -> bool {
        self.failed.contains(key)
    }

    /// Whether `key` still needs asking for.
    pub fn wants(&self, key: &ImageKey) -> bool {
        !self.contains(key) && !self.has_failed(key)
    }

    /// Adds a decoded picture. A bitmap the canvas cannot take counts as a
    /// failure.
    pub fn insert(&mut self, key: ImageKey, bitmap: Bitmap) {
        let bytes = bitmap.rgba.len();
        match Image::from_rgba(bitmap.width, bitmap.height, bitmap.rgba) {
            Ok(image) => {
                self.failed.remove(&key);
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
            }
            Err(_) => {
                self.failed.insert(key);
            }
        }
    }

    pub fn fail(&mut self, key: ImageKey) {
        self.failed.insert(key);
    }

    /// Forgets the failures (a refresh): the pictures are asked for again.
    pub fn retry_failed(&mut self) {
        self.failed.clear();
    }

    /// Frees least recently used pictures until the budget holds, never
    /// those in `protect` (what the view shows).
    pub fn evict(&mut self, protect: &HashSet<&ImageKey>) {
        while self.bytes > self.budget {
            let oldest = self
                .entries
                .iter()
                .filter(|(key, _)| !protect.contains(key))
                .min_by_key(|(_, entry)| entry.used.get())
                .map(|(key, _)| key.clone());
            let Some(key) = oldest else { return };
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes -= entry.bytes;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lazysky_core::Shape;

    fn key(n: u32) -> ImageKey {
        ImageKey {
            url: format!("https://cdn.test/{n}"),
            width: 10,
            height: 10,
            shape: Shape::Cover,
        }
    }

    fn bitmap() -> Bitmap {
        Bitmap {
            width: 10,
            height: 10,
            rgba: vec![128; 400],
        }
    }

    #[test]
    fn the_least_recently_drawn_goes_first_and_the_visible_never() {
        let mut cache = ImageCache::with_budget(1000);
        for n in 0..3 {
            cache.insert(key(n), bitmap());
        }
        assert_eq!(cache.bytes(), 1200);
        // 0 was drawn last: 1 is the oldest.
        assert!(cache.get(&key(1)).is_some());
        assert!(cache.get(&key(2)).is_some());
        assert!(cache.get(&key(0)).is_some());
        cache.evict(&HashSet::new());
        assert!(cache.contains(&key(0)) && cache.contains(&key(2)));
        assert!(!cache.contains(&key(1)));
        assert_eq!(cache.bytes(), 800);

        // A visible picture is kept even over budget.
        let mut tight = ImageCache::with_budget(100);
        tight.insert(key(7), bitmap());
        let visible = key(7);
        tight.evict(&HashSet::from([&visible]));
        assert!(tight.contains(&key(7)), "never evicted while visible");
        tight.evict(&HashSet::new());
        assert!(tight.is_empty());
    }

    #[test]
    fn failures_are_remembered_until_a_refresh() {
        let mut cache = ImageCache::default();
        assert!(cache.wants(&key(1)));
        cache.fail(key(1));
        assert!(!cache.wants(&key(1)));
        cache.retry_failed();
        assert!(cache.wants(&key(1)));
        cache.insert(key(1), bitmap());
        assert!(!cache.wants(&key(1)));
    }

    #[test]
    fn a_bitmap_of_the_wrong_length_is_a_failure_not_a_panic() {
        let mut cache = ImageCache::default();
        cache.insert(
            key(9),
            Bitmap {
                width: 10,
                height: 10,
                rgba: vec![0; 7],
            },
        );
        assert!(cache.has_failed(&key(9)));
        assert!(cache.is_empty());
    }
}
