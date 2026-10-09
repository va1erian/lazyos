//! Read-only introspection: the system-stats snapshot (syscall 14, issue
//! #144; layout in `kernel/src/sysinfo.rs`) and the scheduler task list (13).

use crate::{nr, value};

/// System-stats op codes, `kernel/src/sysinfo.rs::op`.
pub mod system_stats_op {
    /// Write the fixed-layout snapshot into `a1` (capacity `a2` bytes).
    pub const SNAPSHOT: u64 = 0;
    /// Report the snapshot size in bytes.
    pub const SIZE: u64 = 1;
    /// The boot-log ring: a `u64` total, then the newest bytes.
    pub const KLOG: u64 = 2;
    /// The same for what programs wrote to the terminal.
    pub const PROGRAM_LOG: u64 = 3;
}

/// The size in bytes of the snapshot this kernel writes.
pub fn system_stats_size() -> Result<usize, i64> {
    // SAFETY: `SIZE` takes no pointer.
    let code = unsafe { crate::raw::syscall3(nr::SYSTEM_STATS, system_stats_op::SIZE, 0, 0) };
    value(code).map(|size| size as usize)
}

/// Write the snapshot into `buf`; returns the bytes written (`-E2BIG` when
/// `buf` is smaller than [`system_stats_size`]).
pub fn system_stats_snapshot(buf: &mut [u8]) -> Result<usize, i64> {
    // SAFETY: the kernel writes at most `buf.len()` bytes into `buf`.
    let code = unsafe {
        crate::raw::syscall3(
            nr::SYSTEM_STATS,
            system_stats_op::SNAPSHOT,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    value(code).map(|size| size as usize)
}

/// Bytes of the scheduler's task-list snapshot: two header words and 11
/// words for each of the kernel's 256 task slots (`task::introspect`).
pub const TASKS_SNAPSHOT_SIZE: usize = (2 + 256 * 11) * 8;

/// Copy the scheduler's task-list snapshot into `buf`; a buffer smaller than
/// [`TASKS_SNAPSHOT_SIZE`] is refused with `-E2BIG` before the call.
pub fn tasks(buf: &mut [u8]) -> Result<(), i64> {
    if buf.len() < TASKS_SNAPSHOT_SIZE {
        return Err(-crate::errno::E2BIG);
    }
    // SAFETY: syscall 13 has no length argument; the kernel writes exactly
    // `TASKS_SNAPSHOT_SIZE` bytes, which `buf` was just checked to hold.
    let code = unsafe { crate::raw::syscall1(nr::TASKS, buf.as_mut_ptr() as u64) };
    crate::zero(code)
}

/// The boot-log ring (`klog`) into `buf`: a little-endian `u64` total of
/// bytes ever logged, then the newest bytes that fit, oldest first (those
/// bytes are the range `[total - count, total)` of the log). Returns the
/// bytes written, the 8 of the total included.
pub fn klog(buf: &mut [u8]) -> Result<usize, i64> {
    log_ring(system_stats_op::KLOG, buf)
}

/// What programs wrote to the terminal, in the same shape as [`klog`]: the
/// services' own output (`USBD:...`), kept apart from the boot log.
pub fn program_log(buf: &mut [u8]) -> Result<usize, i64> {
    log_ring(system_stats_op::PROGRAM_LOG, buf)
}

fn log_ring(op: u64, buf: &mut [u8]) -> Result<usize, i64> {
    if buf.len() < 8 {
        return Err(-crate::errno::EINVAL);
    }
    // SAFETY: the kernel writes at most `buf.len()` bytes into `buf`.
    let code = unsafe {
        crate::raw::syscall3(
            nr::SYSTEM_STATS,
            op,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    value(code).map(|size| size as usize)
}
