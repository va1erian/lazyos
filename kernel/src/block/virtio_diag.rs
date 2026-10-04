//! Failure reporting for the virtio-blk driver.
//!
//! The filesystems collapse every `BlockError` into one errno, so this is the
//! only place the real cause (timeout or device status) is visible.

use crate::arch::io::inb;
use core::sync::atomic::{AtomicU32, Ordering};

/// How many reports go to serial; later ones stay silent so a dying device
/// cannot flood the log.
const MAX_LOGS: u32 = 8;
/// Legacy virtio device-status register offset from the I/O BAR.
const DEVICE_STATUS: u16 = 18;

/// Report a failed request. `detail` is the status byte for a device error.
pub fn log(io: u16, write: bool, lba: u64, bytes: usize, what: &str, detail: u64) {
    static LOGGED: AtomicU32 = AtomicU32::new(0);
    if LOGGED.fetch_add(1, Ordering::Relaxed) >= MAX_LOGS {
        return;
    }
    // Safety: a read of the status register of the virtio window we drive.
    let device = unsafe { inb(io + DEVICE_STATUS) };
    serial_println!(
        "virtio-blk: io {:#x} {} lba {} len {}: {} ({:#x}, device status {:#x})",
        io,
        if write { "write" } else { "read" },
        lba,
        bytes,
        what,
        detail,
        device
    );
}
