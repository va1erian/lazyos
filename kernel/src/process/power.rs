//! Native syscall 21: `power(op)`, the shell's `reboot` and `shutdown` (issue #6).
//!
//! Stopping the machine is system administration, so the caller must hold
//! `CAP_SYS_ADMIN` (the same gate as binding the display grant); anyone else
//! gets `-EPERM` and nothing happens. The gate is checked before the operation
//! is even decoded, so an unprivileged task cannot probe which ops exist.

use crate::arch::io::{outb, outw};
use crate::ipc::credentials::{self, CAP_SYS_ADMIN};
use crate::task;

/// `op` values.
pub const REBOOT: u64 = 0;
pub const SHUTDOWN: u64 = 1;

const EPERM: i64 = 1;
const EINVAL: i64 = 22;

/// Why a power request was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    NotPermitted,
    BadOp,
}

/// Validate a request: `Ok(op)` when the caller may do it.
pub fn authorize(op: u64) -> Result<u64, Refusal> {
    if !credentials::of(task::current()).has_cap(CAP_SYS_ADMIN) {
        return Err(Refusal::NotPermitted);
    }
    match op {
        REBOOT | SHUTDOWN => Ok(op),
        _ => Err(Refusal::BadOp),
    }
}

/// The syscall body. Returns only on refusal; a permitted request never
/// returns (the machine resets, powers off, or halts).
pub fn dispatch(op: u64) -> u64 {
    match authorize(op) {
        // The in-kernel suite proves the gate only: acting would end the VM.
        #[cfg(lazyos_tests)]
        Ok(_) => 0,
        #[cfg(not(lazyos_tests))]
        Ok(REBOOT) => reboot(),
        #[cfg(not(lazyos_tests))]
        Ok(_) => shutdown(),
        Err(Refusal::NotPermitted) => (-EPERM) as u64,
        Err(Refusal::BadOp) => (-EINVAL) as u64,
    }
}

#[cfg_attr(lazyos_tests, allow(dead_code))] // the suite proves the gate only
fn reboot() -> ! {
    crate::serial_println!("power: reboot requested");
    x86_64::instructions::interrupts::disable();
    // SAFETY: 0xFE to the 8042 command port pulses the CPU reset line. It is
    // the standard PC reset and only reached by a CAP_SYS_ADMIN caller.
    unsafe { outb(0x64, 0xFE) };
    crate::halt()
}

#[cfg_attr(lazyos_tests, allow(dead_code))] // the suite proves the gate only
fn shutdown() -> ! {
    crate::serial_println!("power: shutdown requested");
    x86_64::instructions::interrupts::disable();
    // SAFETY: these ports only exist on virtual machines: 0x604 is QEMU's ACPI
    // PM1a control (SLP_TYP=S5|SLP_EN) and 0xB004 the older Bochs/QEMU one; on
    // hardware without them the writes are ignored and we fall through to
    // halting. Only reached by a CAP_SYS_ADMIN caller.
    unsafe {
        outw(0x604, 0x2000);
        outw(0xB004, 0x2000);
    }
    crate::halt()
}
