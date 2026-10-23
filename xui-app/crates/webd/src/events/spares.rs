//! Pixel buffers handed back by the service thread for the engine's thread to fill again.
//!
//! Every whole frame, band and delta rectangle is copied out of WebKit's picture on its main thread
//! (`pictures.rs`, `full.rs`) and written into a client's slot by the service thread, which then
//! dropped the copy. A frame at 2560x1440 is 14.7 MB: allocating it fresh maps new pages and faults
//! each one in on the very thread that runs the page's scripts and layout, and freeing it unmaps them
//! again (on the build host, 2.3-4.7 ms a frame against 0.7 ms for a copy into a buffer already in
//! use). So the service gives each buffer back once it is in a slot, and the next copy reuses the
//! smallest one large enough. What is kept is bounded in count and bytes and dropped after
//! [`SPARE_IDLE_MS`] unused, so a page that stopped painting leaves nothing behind.

use super::EventQueue;
use crate::limits::{SPARES_KEPT, SPARE_BYTES_MAX, SPARE_IDLE_MS};

/// The buffers kept, each with the time it was given back.
#[derive(Debug, Default)]
pub struct Spares {
    kept: Vec<(Vec<u8>, u64)>,
    /// The newest time given to [`Spares::give`] or [`Spares::trim`].
    clock: u64,
}

impl Spares {
    /// An empty buffer that can hold `bytes` without growing: the smallest kept one that fits, or a
    /// new one.
    pub fn take(&mut self, bytes: usize) -> Vec<u8> {
        let fit = self
            .kept
            .iter()
            .enumerate()
            .filter(|(_, (buffer, _))| buffer.capacity() >= bytes)
            .min_by_key(|(_, (buffer, _))| buffer.capacity())
            .map(|(at, _)| at);
        match fit {
            Some(at) => {
                let mut buffer = self.kept.swap_remove(at).0;
                buffer.clear();
                buffer
            }
            None => Vec::with_capacity(bytes),
        }
    }

    /// `rgba` cut out of `buffer`'s rows: `rect` (`[x, y, width, height]`, already checked to lie
    /// inside the `stride`-byte rows) copied into a buffer from [`Spares::take`].
    pub fn cut(&mut self, rgba: &[u8], stride: usize, rect: [i32; 4]) -> Vec<u8> {
        let [x, y, w, h] = rect.map(|v| v.max(0) as usize);
        let mut out = self.take(w * h * 4);
        for row in y..y + h {
            let start = row * stride + x * 4;
            out.extend_from_slice(&rgba[start..start + w * 4]);
        }
        out
    }

    /// Keep `buffer` for a later copy. Past the bounds the buffers given back longest ago go first.
    pub fn give(&mut self, buffer: Vec<u8>, now_ms: u64) {
        self.clock = self.clock.max(now_ms);
        if buffer.capacity() == 0 || buffer.capacity() > SPARE_BYTES_MAX {
            return;
        }
        self.kept.push((buffer, now_ms));
        while self.kept.len() > SPARES_KEPT || self.bytes() > SPARE_BYTES_MAX {
            let oldest = self
                .kept
                .iter()
                .enumerate()
                .min_by_key(|(_, (_, at))| *at)
                .map_or(0, |(at, _)| at);
            self.kept.swap_remove(oldest);
        }
    }

    /// [`Spares::give`] on the engine's thread, which has no clock: at the newest time seen.
    pub fn keep(&mut self, buffer: Vec<u8>) {
        self.give(buffer, self.clock);
    }

    /// Drop what has not been used for [`SPARE_IDLE_MS`].
    pub fn trim(&mut self, now_ms: u64) {
        self.clock = self.clock.max(now_ms);
        self.kept
            .retain(|(_, at)| now_ms.saturating_sub(*at) < SPARE_IDLE_MS);
    }

    /// Bytes the kept buffers hold.
    pub fn bytes(&self) -> usize {
        self.kept.iter().map(|(buffer, _)| buffer.capacity()).sum()
    }
}

impl EventQueue {
    /// The service is done with a frame's, band's or rectangle's pixels: the engine copies into them
    /// next.
    pub fn recycle(&self, buffer: Vec<u8>) {
        let now = self.now_ms();
        self.lock().spares.give(buffer, now);
    }

    /// Drop the buffers nobody used lately (the service calls this every turn).
    pub fn trim_spares(&self) {
        let now = self.now_ms();
        self.lock().spares.trim(now);
    }

    /// Bytes kept for reuse, for the tests and the memory counters.
    pub fn spare_bytes(&self) -> usize {
        self.lock().spares.bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_smallest_buffer_that_fits_is_reused_empty() {
        let mut spares = Spares::default();
        spares.give(Vec::with_capacity(100), 0);
        spares.give(vec![9; 40], 0);
        let small = spares.take(30);
        assert!(small.is_empty() && small.capacity() >= 40 && small.capacity() < 100);
        assert!(spares.take(100).capacity() >= 100);
        assert_eq!(spares.bytes(), 0);
        assert!(spares.take(1000).capacity() >= 1000, "a new one");
    }

    #[test]
    fn what_is_kept_is_bounded_and_the_oldest_goes_first() {
        let mut spares = Spares::default();
        for at in 0..(SPARES_KEPT as u64 + 3) {
            spares.give(Vec::with_capacity(16 + at as usize), at);
        }
        assert_eq!(spares.kept.len(), SPARES_KEPT);
        assert!(spares.kept.iter().all(|(_, at)| *at >= 3));
        spares.give(Vec::with_capacity(SPARE_BYTES_MAX + 1), 50);
        assert_eq!(spares.kept.len(), SPARES_KEPT, "too large to keep");
        assert!(spares.bytes() <= SPARE_BYTES_MAX);
    }

    #[test]
    fn idle_buffers_are_dropped() {
        let mut spares = Spares::default();
        spares.give(Vec::with_capacity(64), 10);
        spares.give(Vec::with_capacity(64), 1500);
        spares.trim(10 + SPARE_IDLE_MS);
        assert_eq!(spares.kept.len(), 1);
        spares.trim(1500 + SPARE_IDLE_MS);
        assert_eq!(spares.bytes(), 0);
    }

    #[test]
    fn a_cut_copies_the_rectangles_rows() {
        let mut spares = Spares::default();
        // 3x2 pixels, each byte its index.
        let rgba: Vec<u8> = (0..24).collect();
        let out = spares.cut(&rgba, 12, [1, 0, 2, 2]);
        assert_eq!(
            out,
            [4, 5, 6, 7, 8, 9, 10, 11, 16, 17, 18, 19, 20, 21, 22, 23]
        );
    }
}
