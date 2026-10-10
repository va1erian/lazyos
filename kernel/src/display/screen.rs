//! Recording the screen geometry: the firmware's mode at boot, or the one
//! `display.mode` switched to (split out of `display.rs`, issue #194).

use core::sync::atomic::{AtomicBool, Ordering};

use spin::Mutex;

use super::{logical, Screen, LOGICAL, SCREEN};

/// The firmware's framebuffer mode, `(width, height)`, recorded at boot so
/// `display.max` (arriving later, with `lazyos.cfg`) can re-fit the logical
/// screen over it (issue #717). A `display.mode` switch replaces the
/// framebuffer the firmware chose and never consults the cap
/// ([`init_requested`]), so this keeps the firmware's choice.
static FIRMWARE: Mutex<(usize, usize)> = Mutex::new((0, 0));

/// Whether [`init_requested`] last set the logical screen (a mode `display.mode`
/// asked for, exposed whole): a later `display.max` must not re-fit over it.
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Record the framebuffer geometry at boot (called from `kernel_main`, after
/// the console owns the framebuffer), choose the logical screen and print
/// the `HW:FB:<mode>-><logical>` verdict.
pub fn init(width: usize, height: usize, stride: usize, bytes_per_pixel: usize) {
    *FIRMWARE.lock() = (width, height);
    REQUESTED.store(false, Ordering::Release);
    record(width, height, stride, bytes_per_pixel);
}

/// Choose the logical screen under the cap in force and record it, then
/// print the `HW:FB:` verdict.
fn record(width: usize, height: usize, stride: usize, bytes_per_pixel: usize) {
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
    REQUESTED.store(true, Ordering::Release);
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

/// Re-fit the logical screen over the firmware's mode under the cap now in
/// force (`display.max`, [`modeset`]): the boot recorded the fit with the
/// cap the config had not arrived yet. A no-op when `display.mode` was
/// applied (its mode is exposed whole, never capped). The scale and the
/// mouse bounds read the logical screen after `apply_config`, and the
/// caller re-derives the limits.
pub fn refit() {
    if REQUESTED.load(Ordering::Acquire) {
        return;
    }
    let (width, height) = *FIRMWARE.lock();
    let screen = *SCREEN.lock();
    record(
        width,
        height,
        screen.stride as usize,
        screen.bytes_per_pixel as usize,
    );
}

/// Test hooks: save and restore the recorded firmware mode, which
/// `display::with_mode_for_test` overwrites.
#[cfg(lazyos_tests)]
pub fn firmware_for_test() -> (usize, usize) {
    *FIRMWARE.lock()
}

#[cfg(lazyos_tests)]
pub fn set_firmware_for_test(firmware: (usize, usize)) {
    *FIRMWARE.lock() = firmware;
}
