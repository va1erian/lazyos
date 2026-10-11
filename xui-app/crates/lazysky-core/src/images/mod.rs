//! Pictures (docs/lazysky-plan.md §7.5): fetched, decoded under hard limits,
//! cover-cropped to the box the UI asked for and handed back as RGBA, all on
//! the image threads. The UI never decodes or scales (R1, R4).

mod crop;
mod decode;
mod pool;

pub use crop::{circle_mask, cover, fit};
pub use decode::{decode_limited, MAX_PIXELS, MAX_SIDE};
pub(crate) use pool::{ImagePool, THREADS};

/// How a picture fills its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shape {
    /// Scaled to cover the box and centre-cropped (an image in a post).
    Cover,
    /// The same, then masked to a circle (an avatar).
    Circle,
    /// Scaled to fit inside the box, whole, keeping its proportions (the
    /// viewer): the bitmap may be smaller than the box in one direction.
    Fit,
}

/// What the UI wants: this URL, at exactly this many pixels.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageKey {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub shape: Shape,
}

/// A decoded picture at display size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Straight (non-premultiplied) RGBA, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}
