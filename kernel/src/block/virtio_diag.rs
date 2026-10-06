//! Failure reporting for the virtio-blk driver.
//!
//! The filesystems collapse every `BlockError` into one errno, so this is the
//! only place the real cause (timeout or device status) is visible.

use core::sync::atomic::{AtomicU32, Ordering};

use super::virtio::DeviceRegs;

/// How many reports go to serial; later ones stay silent so a dying device
/// cannot flood the log.
const MAX_LOGS: u32 = 8;
/// Report a failed request. `detail` is the status byte for a device error.
pub fn log(regs: &DeviceRegs, write: bool, lba: u64, bytes: usize, what: &str, detail: u64) {
    static LOGGED: AtomicU32 = AtomicU32::new(0);
    if LOGGED.fetch_add(1, Ordering::Relaxed) >= MAX_LOGS {
        return;
    }
    let device = regs.status();
    serial_println!(
        "virtio-blk: {} {} lba {} len {}: {} ({:#x}, device status {:#x})",
        regs.describe(),
        if write { "write" } else { "read" },
        lba,
        bytes,
        what,
        detail,
        device
    );
}
