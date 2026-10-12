#![forbid(unsafe_code)]

//! Image operations and adjustments (P5): pure functions over bitmaps.
//!
//! Nothing here knows about documents, history or the budget; the `Model`
//! methods in `document/` wrap these in undoable edits and check the budget
//! before the allocations they make.

mod adjust;
mod resample;
mod transform;

pub use adjust::{
    Adjustment, Coverage, black_and_white, brightness_contrast, hue_saturation, invert, map_rect,
    map_rect_masked, sepia,
};
pub use resample::{Resample, box_fit, resample};
pub use transform::{
    Anchor, CanvasFill, Flip, Turn, flipped_h, flipped_v, placed, rotated_180, rotated_ccw,
    rotated_cw,
};

#[cfg(test)]
mod tests;
