//! The logical screen (H1 of `docs/real-pc-boot-plan.md`): the part of the
//! firmware's framebuffer the desktop actually uses.
//!
//! Firmware picks the mode, and on a real PC that is the panel's native one:
//! 2560x1440 or 3840x2160 on a desktop monitor. A 4K screen is 31.6 MiB of
//! RGBA, twice the 16 MiB per-process shared-buffer cap and the 16 MiB kernel
//! heap, so `bind` (and the mux's back buffer) would fail and the desktop
//! would never start. The bootloader can only ask for a *minimum* mode, so
//! the kernel caps what it exposes instead: at most [`MAX_WIDTH`] x
//! [`MAX_HEIGHT`], each axis independently, centred in the framebuffer with
//! the rest left black. Every screen-sized buffer (the display grant, the
//! compositor, the mux) is sized from this rectangle, and `present` blits at
//! its offset through a clipped view, so nothing ever writes outside the
//! framebuffer. The boot console keeps the full mode.

/// Widest logical screen exposed to the desktop.
pub const MAX_WIDTH: usize = 1920;
/// Tallest logical screen exposed to the desktop.
pub const MAX_HEIGHT: usize = 1080;

/// Where the logical screen sits in the framebuffer, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Logical {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Logical {
    /// Bytes of one RGBA buffer the size of this screen.
    pub fn rgba_bytes(&self) -> u64 {
        self.width as u64 * self.height as u64 * 4
    }
}

/// The logical screen for a `width` x `height` mode: `min(mode, cap)` per
/// axis, centred (an odd leftover puts the extra pixel on the right/bottom).
pub fn fit(width: usize, height: usize) -> Logical {
    let logical_width = width.min(MAX_WIDTH);
    let logical_height = height.min(MAX_HEIGHT);
    Logical {
        x: (width - logical_width) / 2,
        y: (height - logical_height) / 2,
        width: logical_width,
        height: logical_height,
    }
}
