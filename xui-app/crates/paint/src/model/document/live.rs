#![forbid(unsafe_code)]

//! Painting that is redone on every frame: a shape or gradient being dragged,
//! text being typed.
//!
//! The layer shows the preview as real pixels, so the flattened cache, the
//! screen image and the damage path need no special case. Each frame first
//! puts back what the previous one drew (from the undo capture, which saved
//! the pixels before they were first touched) and then paints again, so a
//! preview that shrinks leaves nothing behind, and the whole drag is one
//! history entry built from the same capture.

use std::sync::Arc;

use crate::model::history::UndoCapture;
use crate::model::tools::Clip;
use crate::model::{Bitmap, Rect, Selection};

/// A selection as the stroke compositor wants it: owned, so a drag can keep
/// it while it borrows the layer. A rectangle selection is just its
/// rectangle, and a masked one shares the selection's bytes.
pub(super) enum ClipMask {
    Rect(Rect),
    Mask { rect: Rect, bytes: Arc<[u8]> },
}

impl ClipMask {
    /// The clip of `selection`, if there is one.
    pub(super) fn of(selection: Option<&Selection>) -> Option<ClipMask> {
        selection.map(|selection| match selection.shared_mask() {
            None => ClipMask::Rect(selection.bounds()),
            Some(bytes) => ClipMask::Mask {
                rect: selection.bounds(),
                bytes,
            },
        })
    }

    /// The compositor's view of the clip.
    pub(super) fn clip(&self) -> Clip<'_> {
        match self {
            ClipMask::Rect(rect) => Clip::Rect(*rect),
            ClipMask::Mask { rect, bytes } => Clip::Mask {
                rect: *rect,
                mask: bytes,
            },
        }
    }

    /// The pixels the clip can let through.
    pub(super) fn rect(&self) -> Rect {
        match self {
            ClipMask::Rect(rect) | ClipMask::Mask { rect, .. } => *rect,
        }
    }
}

/// The compositor's clip for an optional mask.
pub(super) fn clip_of(mask: &Option<ClipMask>) -> Option<Clip<'_>> {
    mask.as_ref().map(ClipMask::clip)
}

/// One layer's live preview.
pub(super) struct Live {
    /// The layer being painted.
    pub(super) layer: usize,
    capture: UndoCapture,
    /// What the last frame changed.
    painted: Rect,
}

impl Live {
    pub(super) fn new(layer: usize) -> Live {
        Live {
            layer,
            capture: UndoCapture::new(),
            painted: Rect::EMPTY,
        }
    }

    /// What the capture would span if `reach` were painted too.
    pub(super) fn span_with(&self, reach: Rect) -> Rect {
        self.capture.bounds().union(reach)
    }

    /// Takes back the last frame, lets `paint` draw the next (it may touch
    /// only `reach`, and returns what it changed) and returns the pixels that
    /// differ from what was shown before.
    pub(super) fn repaint(
        &mut self,
        layer: &mut Bitmap,
        reach: Rect,
        paint: impl FnOnce(&mut Bitmap) -> Rect,
    ) -> Rect {
        let old = self.painted;
        self.capture.restore(layer, old);
        self.capture.touch(layer, reach);
        self.painted = paint(layer).intersect(layer.rect());
        old.union(self.painted)
    }

    /// Takes back everything painted; returns the pixels that changed.
    pub(super) fn cancel(self, layer: &mut Bitmap) -> Rect {
        self.capture.restore(layer, self.painted);
        self.painted
    }

    /// The capture, for the history entry.
    pub(super) fn into_capture(self) -> UndoCapture {
        self.capture
    }
}
