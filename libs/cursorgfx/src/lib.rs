//! Antialiased, resizable mouse-cursor sprites (docs/cursor-plan.md).
//!
//! Every [`Shape`] is a set of polygons on a 32x32 design grid. [`render`]
//! rasterises them at any physical size with 4x4 subsamples per pixel and
//! non-zero winding, so a 2x desktop gets a real 32 px drawing, never doubled
//! blocks. Pure `no_std` + `alloc` integer maths (no floats), host tested.
//!
//! A sprite is a body filled light with a dark outline drawn outside it (the
//! old 8x8 arrow's colours), premultiplied ARGB (`0xAARRGGBB`).
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod raster;
mod shapes;

use alloc::vec::Vec;

/// The design sizes (pixels at scale 1) a user may pick.
pub const SIZES: [u32; 4] = [16, 24, 32, 48];
/// Rotation frames of the busy spinner.
pub const BUSY_FRAMES: u32 = 8;
/// How many [`Shape`] variants there are.
pub const SHAPE_COUNT: usize = 8;

/// What the pointer looks like. The discriminants are the wire values of the
/// display interface's `CursorShape` enum (`idl/display.midl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Shape {
    #[default]
    Default = 0,
    Busy = 1,
    Text = 2,
    Hand = 3,
    ResizeH = 4,
    ResizeV = 5,
    ResizeNwse = 6,
    ResizeNesw = 7,
}

impl Shape {
    /// Every shape, in wire order.
    pub const ALL: [Shape; SHAPE_COUNT] = [
        Shape::Default,
        Shape::Busy,
        Shape::Text,
        Shape::Hand,
        Shape::ResizeH,
        Shape::ResizeV,
        Shape::ResizeNwse,
        Shape::ResizeNesw,
    ];

    /// The shape a wire value names; `None` for an unknown value.
    pub fn from_wire(value: u32) -> Option<Shape> {
        Shape::ALL.get(value as usize).copied()
    }

    /// The wire value.
    pub fn to_wire(self) -> u32 {
        self as u32
    }

    /// Whether the sprite animates (needs a frame tick).
    pub fn animated(self) -> bool {
        self == Shape::Busy
    }
}

/// A rendered cursor: `pixels` are premultiplied ARGB, row after row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sprite {
    pub w: u32,
    pub h: u32,
    /// The pointer position inside the sprite.
    pub hot_x: u32,
    pub hot_y: u32,
    pub pixels: Vec<u32>,
}

/// Pixels of transparent margin around the design square for a sprite of
/// `px` pixels: room for the outline.
pub fn margin(px: u32) -> u32 {
    raster::outline_radius(clamp_px(px)).div_ceil(raster::SUB) + 1
}

/// The side of the largest sprite (any shape) for `px`: what a save-under
/// buffer must hold.
pub fn max_extent(px: u32) -> u32 {
    clamp_px(px) + 2 * margin(px)
}

fn clamp_px(px: u32) -> u32 {
    px.clamp(8, 256)
}

/// Render `shape` with its 32-unit design square `px` physical pixels wide.
/// `frame` selects the busy spinner's rotation (`frame % BUSY_FRAMES`) and is
/// ignored by every other shape. `px` is clamped to 8..=256.
pub fn render(shape: Shape, px: u32, frame: u32) -> Sprite {
    let px = clamp_px(px);
    let m = margin(px);
    let side = px + 2 * m;
    let layers = shapes::layers(shape, frame % BUSY_FRAMES);
    let mut pixels: Vec<u32> = alloc::vec![0u32; (side * side) as usize];
    for layer in &layers {
        raster::draw_layer(&mut pixels, side, m, px, layer);
    }
    let (hx, hy) = shapes::hotspot(shape);
    Sprite {
        w: side,
        h: side,
        hot_x: m + hx * px / 32,
        hot_y: m + hy * px / 32,
        pixels,
    }
}

#[cfg(test)]
mod tests;
