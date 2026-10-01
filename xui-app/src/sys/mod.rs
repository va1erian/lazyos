//! Raw native-syscall shim for the display grant (12), the Messenger fabric
//! (5), the PIT clock (8) and the system-stats snapshot (14).
//!
//! Besides the raw `int 0x80` helpers, this module carries the small
//! libmessenger-based plumbing the display protocol client ([`crate::display`])
//! needs: a direct registry `resolve`, a synchronous `call`, an endpoint
//! `recv`, and `create_pair`.
//!
//! Register convention (see `user/src/sys.rs` in the LazyOS tree): `rax` is the
//! syscall number, arguments in `rdi`/`rsi`/`rdx`, the result in `rax`. `int
//! 0x80` is the native gate and is dispatched by task, not by binary kind, so a
//! static musl program reaches the same code a native `user` program does.
//! `rcx`/`r11` are not preserved by the gate.

mod display;
mod messenger;

pub use display::{
    button, decode_event, display_bind, display_close_buffer, display_create_buffer,
    display_input_poll, display_present, display_unbind, event, key, op, DisplayInfo, RawEvent,
    EVENT_BYTES,
};
pub use messenger::{
    messenger, msg_call, msg_create_pair, msg_op, msg_queued, msg_recv, msg_resolve, msg_send,
    MsgArgs, MsgResult, REGISTRY_TARGET_SELF,
};

use core::arch::asm;

/// `display(op, a1, a2)` — the display device grant.
pub const SYS_DISPLAY: u64 = 12;
/// `messenger(op, args, result)` — the native Messenger fabric.
pub const SYS_MESSENGER: u64 = 5;
/// `clock()` — the PIT tick counter (100 Hz), absolute deadlines.
pub const SYS_CLOCK: u64 = 8;
/// `system_stats(op, a1, a2)` — the read-only system monitor (issue #144).
pub const SYS_SYSTEM_STATS: u64 = 14;
/// `nanosleep(req, rem)` — the Linux ABI's relative sleep.
pub const SYS_NANOSLEEP: u64 = 35;

/// An absolute PIT tick in the past: `recv` treats it as a non-blocking poll
/// (mirrors `user::messenger::EXPIRED_DEADLINE`).
pub const EXPIRED_DEADLINE: u64 = 1;

/// Linux errno values used by the parcel helpers (positive forms).
pub mod errno {
    /// No such file or directory / service.
    pub const ENOENT: i64 = 2;
    /// The receive buffer is too small.
    pub const E2BIG: i64 = 7;
    /// Try again later.
    pub const EAGAIN: i64 = 11;
    /// Invalid argument.
    pub const EINVAL: i64 = 22;
    /// The peer endpoint is gone.
    pub const EPIPE: i64 = 32;
    /// A deadline fired.
    pub const ETIMEDOUT: i64 = 110;
}
/// System-stats op codes, mirroring `kernel/src/sysinfo.rs::op`.
pub mod system_stats_op {
    /// Write the snapshot into the caller's buffer.
    pub const SNAPSHOT: u64 = 0;
    /// Report the snapshot's size in bytes.
    pub const SIZE: u64 = 1;
}
/// One native syscall through the `int 0x80` gate; the raw result register.
fn native(nr: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let code: u64;
    // Safety: `int 0x80` with the native syscall convention. The kernel runs
    // the gate on this task's page table and validates every pointer argument
    // against the caller's address space.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") nr,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}
/// The PIT tick counter (100 Hz). `0` before the first tick.
pub fn clock_ticks() -> u64 {
    native(SYS_CLOCK, 0, 0, 0) as u64
}

/// One `system_stats` syscall; `SIZE`, 0 or a negative errno.
pub fn system_stats(op: u64, a1: u64, a2: u64) -> i64 {
    native(SYS_SYSTEM_STATS, op, a1, a2)
}
/// Sleep for `millis` milliseconds.
///
/// `nanosleep` is a *Linux* syscall, so it goes through the `syscall` gate
/// (like the rest of `std`); the native `int 0x80` dispatcher used by the
/// display ops above has no sleep. A relative timespec is used rather than
/// `std::thread::sleep`, whose absolute `clock_nanosleep` deadline LazyOS
/// currently treats as a duration.
pub fn sleep_millis(millis: u64) {
    let request: [i64; 2] = [(millis / 1000) as i64, ((millis % 1000) * 1_000_000) as i64];
    // Safety: `syscall` with the Linux nanosleep number (35) and a two-`i64`
    // `timespec` the kernel reads from this task's address space.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_NANOSLEEP,
            in("rdi") request.as_ptr() as u64,
            in("rsi") 0u64,
            lateout("rax") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
}
