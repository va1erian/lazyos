//! Pixel buffers handed back by the service thread for the engine's thread to fill again.
//!
//! Every whole frame, band and delta rectangle is copied out of WebKit's picture on its main thread
//! (`pictures.rs`, `full.rs`) and written into a client's slot by the service thread, which then
//! dropped the copy. A frame at 2560x1440 is 14.7 MB: allocating it fresh maps new pages and faults
//! each one in on the very thread that runs the page's scripts and layout, and freeing it unmaps them
//! again (on the build host, 2.3-4.7 ms a frame against 0.7 ms for a copy into a buffer already in
//! use). So the service gives each turn's buffers back at once ([`EventQueue::give_back`]), and the
//! next copy reuses a kept one of about its size.
//!
//! The rules keep that cheap and bounded. Only buffers of at least [`SPARE_MIN_BYTES`] are kept
//! (smaller ones come from the allocator's own lists for free), and one is reused only for a copy of
//! at least four fifths of its size, so a band or a strip of rows rarely takes the buffer the next
//! whole frame needs. What is kept is bounded in count (the smallest go first: they are the cheapest
//! to make again) and in bytes (the oldest go first), and dropped after [`SPARE_IDLE_MS`] unused. A buffer that goes is never freed on the
//! engine's thread or under the queue's lock: it waits in `retired` for the service to drop.

use super::EventQueue;
use crate::limits::{SPARES_KEPT, SPARE_BYTES_MAX, SPARE_IDLE_MS, SPARE_MIN_BYTES};

/// The buffers kept, each with the time it was given back, and those waiting to be freed.
#[derive(Debug, Default)]
pub struct Spares {
    kept: Vec<(Vec<u8>, u64)>,
    retired: Vec<Vec<u8>>,
    /// The queue's clock as the caller last read it ([`Spares::at`]): what [`Spares::keep`] stamps.
    now: u64,
}

impl Spares {
    /// The pool at `now_ms` (the queue's clock, read by the callback that holds the lock).
    pub fn at(&mut self, now_ms: u64) -> &mut Spares {
        self.now = now_ms;
        self
    }

    /// An empty buffer that holds `bytes` without growing: the smallest kept one that fits and is
    /// at most a quarter larger, or a new one.
    pub fn take(&mut self, bytes: usize) -> Vec<u8> {
        if bytes < SPARE_MIN_BYTES {
            return Vec::with_capacity(bytes);
        }
        let fit = self
            .kept
            .iter()
            .enumerate()
            .filter(|(_, (buffer, _))| {
                buffer.capacity() >= bytes && buffer.capacity() - buffer.capacity() / 5 <= bytes
            })
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

    /// `rect` (`[x, y, width, height]`, already checked to lie inside the `stride`-byte rows of
    /// `rgba`) copied into a buffer from [`Spares::take`].
    pub fn cut(&mut self, rgba: &[u8], stride: usize, rect: [i32; 4]) -> Vec<u8> {
        let [x, y, w, h] = rect.map(|v| v.max(0) as usize);
        let mut out = self.take(w * h * 4);
        for row in y..y + h {
            let start = row * stride + x * 4;
            out.extend_from_slice(&rgba[start..start + w * 4]);
        }
        out
    }

    /// [`Spares::give`] at the time of the last [`Spares::at`].
    pub fn keep(&mut self, buffer: Vec<u8>) {
        self.give(buffer, self.now);
    }

    /// Keep `buffer` (given back at `now_ms`) for a later copy. Past the count the smallest kept
    /// buffer is retired, past the bytes the one given back longest ago.
    pub fn give(&mut self, buffer: Vec<u8>, now_ms: u64) {
        let size = buffer.capacity();
        if !(SPARE_MIN_BYTES..=SPARE_BYTES_MAX).contains(&size) {
            self.retire(buffer);
            return;
        }
        self.kept.push((buffer, now_ms));
        while self.bytes() > SPARE_BYTES_MAX {
            let oldest = self.position(|(_, at)| *at as usize);
            let gone = self.kept.swap_remove(oldest).0;
            self.retire(gone);
        }
        while self.kept.len() > SPARES_KEPT {
            let smallest = self.position(|(buffer, _)| buffer.capacity());
            let gone = self.kept.swap_remove(smallest).0;
            self.retire(gone);
        }
    }

    /// Where the kept buffer with the least `key` is.
    fn position(&self, key: impl Fn(&(Vec<u8>, u64)) -> usize) -> usize {
        self.kept
            .iter()
            .enumerate()
            .min_by_key(|(_, kept)| key(kept))
            .map_or(0, |(at, _)| at)
    }

    /// A buffer to free later, by the service (a small one is freed here: that costs nothing).
    fn retire(&mut self, buffer: Vec<u8>) {
        if buffer.capacity() >= SPARE_MIN_BYTES {
            self.retired.push(buffer);
        }
    }

    /// Retire what has not been used for [`SPARE_IDLE_MS`] (or everything, with `all`), and hand
    /// every retired buffer to the caller to free outside the lock.
    pub fn trim(&mut self, now_ms: u64, all: bool) -> Vec<Vec<u8>> {
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.kept)
            .into_iter()
            .partition(|(_, at)| all || now_ms.saturating_sub(*at) >= SPARE_IDLE_MS);
        self.kept = kept;
        let mut out = std::mem::take(&mut self.retired);
        out.extend(gone.into_iter().map(|(buffer, _)| buffer));
        out
    }

    /// Bytes the kept buffers hold.
    pub fn bytes(&self) -> usize {
        self.kept.iter().map(|(buffer, _)| buffer.capacity()).sum()
    }
}

impl EventQueue {
    /// The service is done with this turn's pixels (frames, bands, rectangles, written or dropped):
    /// keep them for the engine's next copies and drop what was not used lately, under one lock; the
    /// buffers that go are freed after it, on the caller's thread.
    pub fn give_back(&self, buffers: Vec<Vec<u8>>) {
        let now = self.now_ms();
        let freed = {
            let mut inner = self.lock();
            for buffer in buffers {
                inner.spares.give(buffer, now);
            }
            inner.spares.trim(now, false)
        };
        drop(freed);
    }

    /// Free every kept buffer (the service is about to read the user's memory charge).
    pub fn drop_spares(&self) {
        let now = self.now_ms();
        let freed = self.lock().spares.trim(now, true);
        drop(freed);
    }

    /// Bytes kept for reuse, for the tests and the memory counters.
    pub fn spare_bytes(&self) -> usize {
        self.lock().spares.bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIG: usize = SPARE_MIN_BYTES;

    #[test]
    fn a_buffer_is_reused_only_for_a_copy_of_about_its_size() {
        let mut spares = Spares::default();
        spares.give(Vec::with_capacity(4 * BIG), 0);
        spares.give(vec![9; BIG], 0);
        let band = spares.take(BIG);
        assert!(band.is_empty() && band.capacity() >= BIG && band.capacity() < 4 * BIG);
        assert!(
            spares.take(BIG + 1).capacity() < 4 * BIG,
            "a frame's buffer is not given to a band"
        );
        assert_eq!(spares.bytes(), 4 * BIG);
        assert!(
            spares.take(3 * BIG).capacity() < 4 * BIG,
            "a quarter too large"
        );
        assert!(spares.take(7 * BIG / 2).capacity() >= 4 * BIG);
        assert_eq!(spares.bytes(), 0);
        assert_eq!(spares.take(0).capacity(), 0);
    }

    #[test]
    fn small_buffers_are_not_kept_and_do_not_push_large_ones_out() {
        let mut spares = Spares::default();
        spares.give(Vec::with_capacity(4 * BIG), 0);
        for at in 1..=(SPARES_KEPT as u64 * 2) {
            spares.give(Vec::with_capacity(16), at);
        }
        assert_eq!(spares.bytes(), 4 * BIG);
        assert!(
            spares.trim(1, false).is_empty(),
            "small ones are freed at once"
        );
    }

    #[test]
    fn what_is_kept_is_bounded_in_count_and_bytes_and_the_oldest_goes() {
        let mut spares = Spares::default();
        for at in 0..(SPARES_KEPT as u64 + 3) {
            spares.give(Vec::with_capacity(BIG + at as usize), at);
        }
        assert_eq!(spares.kept.len(), SPARES_KEPT);
        assert!(spares.kept.iter().all(|(_, at)| *at >= 3));
        assert_eq!(
            spares.trim(3, false).len(),
            3,
            "retired, freed by the caller"
        );
        let half = SPARE_BYTES_MAX / 2 + 1;
        spares.give(Vec::with_capacity(half), 50);
        spares.give(Vec::with_capacity(half), 51);
        assert!(spares.bytes() <= SPARE_BYTES_MAX);
        assert!(
            spares.kept.iter().any(|(_, at)| *at == 51),
            "the newest stays"
        );
        assert!(spares.kept.iter().all(|(_, at)| *at != 50));
    }

    #[test]
    fn a_frames_buffer_outlives_a_burst_of_strips_given_back_after_it() {
        let mut spares = Spares::default();
        spares.give(Vec::with_capacity(16 * BIG), 0);
        for at in 1..=(SPARES_KEPT as u64 + 4) {
            spares.give(Vec::with_capacity(BIG), at);
        }
        assert!(spares.take(16 * BIG).capacity() >= 16 * BIG, "still kept");
    }

    #[test]
    fn idle_buffers_are_retired_and_all_go_on_request() {
        let mut spares = Spares::default();
        spares.give(Vec::with_capacity(BIG), 10);
        spares.give(Vec::with_capacity(BIG), 1500);
        assert_eq!(spares.trim(10 + SPARE_IDLE_MS, false).len(), 1);
        assert_eq!(spares.bytes(), BIG);
        assert_eq!(spares.trim(1501, true).len(), 1);
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
