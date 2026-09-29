//! The native wall-clock syscall (24, issue #369): UTC time for services.

use core::arch::asm;

/// `wall_time(op, a1)` — the UTC wall clock (see `kernel/src/process/wallsys.rs`).
pub const SYS_WALL_TIME: u64 = 24;

const OP_GET: u64 = 0;
const OP_SET: u64 = 1;

fn wall_time(op: u64, arg: u64) -> u64 {
    let result: u64;
    // Safety: `int 0x80` with syscall 24; no pointers cross the gate.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") SYS_WALL_TIME,
            in("rdi") op,
            in("rsi") arg,
            lateout("rax") result,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    result
}

/// UTC centiseconds since the Unix epoch.
pub fn wall_centis() -> u64 {
    wall_time(OP_GET, 0)
}

/// Step the wall clock to `unix_secs` (UTC). Needs `CAP_SYS_TIME`; returns the
/// negative errno the kernel refused with.
pub fn wall_set(unix_secs: u64) -> Result<(), i64> {
    match wall_time(OP_SET, unix_secs) as i64 {
        0 => Ok(()),
        code => Err(code),
    }
}
