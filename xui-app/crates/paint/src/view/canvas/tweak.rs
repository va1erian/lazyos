#![forbid(unsafe_code)]

//! The adjustment preview: a colour adjustment shown on the pixels the canvas
//! draws, with the document untouched.
//!
//! The Brightness/Contrast and Hue/Saturation dialogs set one while their
//! sliders move. The screen image is a window's worth of pixels, so a drag
//! costs the window, not the layer; OK later applies the adjustment to the
//! layer as one history entry and Cancel just drops the preview.

use crate::model::{Adjustment, Pixel, Selection, mix_by};

/// An adjustment (and the selection it is clipped to) previewed on screen.
pub struct AdjustPreview {
    kernel: Box<dyn Fn(Pixel) -> Pixel>,
    selection: Option<Selection>,
}

impl AdjustPreview {
    /// Previews `adjustment`, limited to `selection` when there is one.
    pub fn new(adjustment: Adjustment, selection: Option<Selection>) -> AdjustPreview {
        AdjustPreview {
            kernel: adjustment.kernel(),
            selection,
        }
    }

    /// What the pixel at picture `(x, y)` shows: the adjusted pixel through
    /// the selection's coverage, so the preview and OK agree at a soft edge.
    pub fn pixel(&self, x: i32, y: i32, pixel: Pixel) -> Pixel {
        let cover = self.selection.as_ref().map_or(255, |s| s.coverage(x, y));
        if cover == 0 {
            return pixel;
        }
        mix_by(cover, pixel, (self.kernel)(pixel))
    }
}
