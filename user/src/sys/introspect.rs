//! Read-only introspection syscalls: the native Messenger surface (syscall 5),
//! the scheduler task list (syscall 13) and the system-stats snapshot
//! (syscall 14).

use core::arch::asm;

use super::{SYS_MESSENGER, SYS_SYSTEM_STATS, SYS_TASKS};

/// Copy a [`crate::messenger::TaskSnapshot`] scheduler snapshot into `buf`,
/// a writable buffer of at least `crate::messenger::TaskSnapshot::SIZE` bytes.
/// Returns 0 on success or a negative errno (mirrors `sys_quota`'s shape).
pub fn tasks(buf: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 13 and a valid writable buffer pointer.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_TASKS,
            in("rdi") buf,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}

/// Invoke the native Messenger syscall: `op` selects the operation, `args`
/// and `result` are user addresses of the fixed-size blocks defined in
/// [`crate::messenger`]. Returns 0 on success or a negative errno.
pub fn messenger(op: u64, args: u64, result: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 5; the kernel validates both pointers
    // against this task's address space before touching them.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_MESSENGER,
            in("rdi") op,
            in("rsi") args,
            in("rdx") result,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}

// ---------------------------------------------------------------------------
// System statistics snapshot (issue #144)
// ---------------------------------------------------------------------------

/// System-stats op codes, mirroring `kernel/src/sysinfo.rs`.
pub mod system_stats_op {
    /// Write the fixed-layout snapshot into `a1` (capacity `a2` bytes).
    pub const SNAPSHOT: u64 = 0;
    /// Report the snapshot size in bytes.
    pub const SIZE: u64 = 1;
}

/// Invoke the native system-stats syscall. Returns `SIZE` on a successful
/// snapshot, or a negative errno.
pub fn system_stats(op: u64, a1: u64, a2: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with syscall 14; the kernel copies the snapshot into
    // the caller's buffer under the native syscall buffer convention.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_SYSTEM_STATS,
            in("rdi") op,
            in("rsi") a1,
            in("rdx") a2,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}
