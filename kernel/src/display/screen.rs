//! Recording the screen geometry: the firmware's mode at boot, or the one
//! `display.mode` switched to (split out of `display.rs`, issue #194).

use super::{logical, Screen, LOGICAL, SCREEN};

/// Record the framebuffer geometry at boot (called from `kernel_main`, after
/// the console owns the framebuffer), choose the logical screen and print
/// the `HW:FB:<mode>-><logical>` verdict.
pub fn init(width: usize, height: usize, stride: usize, bytes_per_pixel: usize) {
    let fitted = logical::fit(width, height);
    *LOGICAL.lock() = fitted;
    *SCREEN.lock() = Screen {
        width: fitted.width as u64,
        height: fitted.height as u64,
        stride: stride as u64,
        bytes_per_pixel: bytes_per_pixel as u64,
    };
    serial_println!(
        "HW:FB:{}x{}->{}x{} at {},{} stride {} bpp {}",
        width,
        height,
        fitted.width,
        fitted.height,
        fitted.x,
        fitted.y,
        stride,
        bytes_per_pixel
    );
}

/// Record a mode the user asked for (`display.mode`, [`modeset`]): unlike
/// the firmware's choice in [`init`], it is exposed whole, never reduced to
/// the logical cap. The switch already checked it against the adapter's
/// video memory, and the limits are re-derived from it.
pub fn init_requested(width: usize, height: usize, stride: usize, bytes_per_pixel: usize) {
    *LOGICAL.lock() = logical::Logical {
        x: 0,
        y: 0,
        width,
        height,
    };
    *SCREEN.lock() = Screen {
        width: width as u64,
        height: height as u64,
        stride: stride as u64,
        bytes_per_pixel: bytes_per_pixel as u64,
    };
    serial_println!("HW:FB:{width}x{height}->{width}x{height} at 0,0 stride {stride} bpp {bytes_per_pixel} (display.mode)");
}
