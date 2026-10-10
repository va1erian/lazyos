//! The logical screen (H1 of `docs/real-pc-boot-plan.md`): the part of the
//! firmware's framebuffer the desktop actually uses.
//!
//! Firmware picks the mode, and on a real PC that is the panel's native one:
//! 2560x1440 or 3840x2160 on a desktop monitor. Originally every buffer was a
//! fixed 16 MiB, so the kernel capped what it exposes: at most
//! [`DEFAULT_WIDTH`] x [`DEFAULT_HEIGHT`] per axis, centred in the
//! framebuffer with the rest left black (`HW:FB:2560x1440->1920x1080`).
//! Limits are now derived from the screen (`crate::limits`), so the cap is a
//! boot-time preference instead of a memory constraint: `display.max=<W>x<H>`
//! in `lazyos.cfg` (`LAZYOS_DISPLAY_MAX` at build time) lifts it, and a
//! 2560x1440 panel then runs a 1280x720 desktop at scale 2 (issue #717).
//! Every screen-sized buffer (the display grant, the compositor, the mux) is
//! sized from this rectangle, and `present` blits at its offset through a
//! clipped view, so nothing ever writes outside the framebuffer. The boot
//! console keeps the full mode.

use spin::Mutex;
use super::modecfg;

/// Widest logical screen exposed to the desktop by default.
pub const DEFAULT_WIDTH: usize = 1920;
/// Tallest logical screen exposed to the desktop by default.
pub const DEFAULT_HEIGHT: usize = 1080;

/// The live cap per axis, `(width, height)`. Written once at boot, before
/// anything sizes itself from the logical screen; read through copies ever
/// after.
static CAP: Mutex<(usize, usize)> =
    Mutex::new((DEFAULT_WIDTH, DEFAULT_HEIGHT));

/// The screen cap currently in force, `(width, height)`.
pub fn cap() -> (usize, usize) {
    *CAP.lock()
}

/// Set the cap, clamped into the range the config parser already accepts
/// (`modecfg`), so a stray value can never widen it past the kernel's mode
/// bounds or below the smallest working screen. Returns what was set.
pub(crate) fn set_cap(width: u32, height: u32) -> (usize, usize) {
    let cap = (
        (width as usize).clamp(modecfg::MIN_WIDTH as usize, modecfg::MAX_WIDTH as usize),
        (height as usize).clamp(modecfg::MIN_HEIGHT as usize, modecfg::MAX_HEIGHT as usize),
    );
    *CAP.lock() = cap;
    cap
}

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
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn rgba_bytes(&self) -> u64 {
        self.width as u64 * self.height as u64 * 4
    }
}

/// The logical screen for a `width` x `height` mode: `min(mode, cap)` per
/// axis, centred (an odd leftover puts the extra pixel on the right/bottom).
pub fn fit(width: usize, height: usize) -> Logical {
    let (max_width, max_height) = cap();
    let logical_width = width.min(max_width);
    let logical_height = height.min(max_height);
    Logical {
        x: (width - logical_width) / 2,
        y: (height - logical_height) / 2,
        width: logical_width,
        height: logical_height,
    }
}
