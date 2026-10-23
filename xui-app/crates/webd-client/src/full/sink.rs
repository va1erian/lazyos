//! A full view whose app keeps its own picture (LazyWeb's page view, docs/webd-plan.md P4): instead
//! of copying each frame into the view's [`Picture`](crate::picture::Picture), [`FullWebView::poll_into`]
//! hands the slot to a [`PixelSink`], which reads only what it needs straight out of the shared
//! pages (a whole frame once, a delta's rectangles), so a frame costs one copy, as it did when the
//! engine ran in the browser's process.
//!
//! The view still checks every frame as [`FullWebView::poll`] does (the size, the slot, a delta's
//! base and rectangles) before the sink sees it, and still releases the slot after it.

use super::event::FrameInfo;
use super::{FullWebView, FRAME_SLOTS};
use crate::transport::{ScopedTransport, SharedMem};

/// The pixels of a slot webd painted, as a sink may read them.
pub trait SlotPixels {
    /// Bytes the slot holds.
    fn len(&self) -> usize;
    /// Copy `into.len()` bytes from `offset` (fewer when the slot ends first).
    fn read_at(&self, offset: usize, into: &mut [u8]);
}

impl<M: SharedMem> SlotPixels for M {
    fn len(&self) -> usize {
        SharedMem::len(self)
    }
    fn read_at(&self, offset: usize, into: &mut [u8]) {
        SharedMem::read_at(self, offset, into)
    }
}

/// Where a view's frames go when the app keeps its own picture. The slot is laid out as a whole
/// frame: `width * 4` bytes a row, RGBA.
pub trait PixelSink {
    /// A whole frame (`frame.whole`): every pixel of `frame.width` x `frame.height` is new.
    fn whole(&mut self, frame: &FrameInfo, slot: &dyn SlotPixels);
    /// A delta on top of the frame before it (`frame.shift` first, then `frame.rects`, every one
    /// inside the view). `false` when the sink cannot apply it (it has no picture to apply it to):
    /// the view then asks webd for a whole frame.
    fn delta(&mut self, frame: &FrameInfo, slot: &dyn SlotPixels) -> bool;
}

/// Which slots a view attaches: `frames` of the frame slots (1 to 3) and `bands` band slots (0 to
/// 2, with `deltas` only). A slot past the first that cannot be had (the process's shared-buffer
/// limit, `limit.shared_buffer_max`) is left out rather than failing the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotPlan {
    pub frames: usize,
    pub bands: usize,
}

impl Default for SlotPlan {
    fn default() -> Self {
        SlotPlan {
            frames: FRAME_SLOTS,
            bands: super::BAND_SLOTS,
        }
    }
}

impl SlotPlan {
    /// The slot numbers to attach, most needed first: one frame slot, a second (so webd can paint
    /// while the app reads), the bands, the third frame slot.
    pub(super) fn order(self, deltas: bool) -> Vec<usize> {
        let frames = self.frames.clamp(1, FRAME_SLOTS);
        let bands = if deltas {
            self.bands.min(super::BAND_SLOTS)
        } else {
            0
        };
        let mut order = vec![0];
        if frames > 1 {
            order.push(1);
        }
        order.extend((0..bands).map(|n| FRAME_SLOTS + n));
        if frames > 2 {
            order.push(2);
        }
        order
    }
}

impl<T: ScopedTransport> FullWebView<T> {
    /// Like [`FullWebView::poll`], but frames go to `sink` and the view keeps no picture of its own.
    pub fn poll_into(&mut self, sink: &mut dyn PixelSink) -> Vec<super::FullEvent> {
        self.poll_with(Some(sink))
    }

    /// Which slots to attach from the next [`FullWebView::create`] or resize on.
    pub fn set_slot_plan(&mut self, plan: SlotPlan) {
        self.plan = plan;
    }

    /// The slots attached now, by number (`true` for one webd may paint into).
    pub fn attached(&self) -> Vec<bool> {
        self.slots.iter().map(Option::is_some).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_attaches_what_matters_first() {
        let all = SlotPlan::default();
        assert_eq!(all.order(true), vec![0, 1, 3, 4, 2]);
        assert_eq!(all.order(false), vec![0, 1, 2]);
        let small = SlotPlan {
            frames: 2,
            bands: 1,
        };
        assert_eq!(small.order(true), vec![0, 1, 3]);
        let none = SlotPlan {
            frames: 0,
            bands: 9,
        };
        assert_eq!(none.order(true), vec![0, 3, 4], "at least one frame slot");
    }
}
