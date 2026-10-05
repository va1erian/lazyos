//! Native syscall 21: `power(op, arg)`, the last step of an orderly shutdown
//! (docs/shutdown.md) and of `reboot` (issue #6).
//!
//! Stopping the machine is system administration, so the caller must hold
//! `CAP_SYS_ADMIN` (the same gate as binding the display grant); anyone else
//! gets `-EPERM` and nothing happens. The gate is checked before the operation
//! is even decoded, so an unprivileged task cannot probe which ops exist.
//!
//! `init` is the only caller in a running system: it quiesces userspace first
//! and calls [`REBOOT`] or [`SHUTDOWN`] last. Before it starts it arms the
//! [`watchdog`] with [`ARM_WATCHDOG`], so a supervisor that hangs or dies
//! half-way through still ends with synced filesystems and a stopped machine.

use crate::arch::io::{inb, outb, outw};
use crate::ipc::credentials::{self, CAP_SYS_ADMIN};
use crate::task;

#[path = "power_watchdog.rs"]
pub mod watchdog;

/// `op` values.
pub const REBOOT: u64 = 0;
pub const SHUTDOWN: u64 = 1;
/// Arm the shutdown watchdog: `arg` is the stop ([`REBOOT`] or [`SHUTDOWN`])
/// the kernel forces if the machine is still running
/// [`watchdog::TIMEOUT_TICKS`] later. One-way: there is no disarm, and a
/// second arm never postpones the first deadline.
pub const ARM_WATCHDOG: u64 = 2;

const EPERM: i64 = 1;
const EINVAL: i64 = 22;

/// Why a power request was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    NotPermitted,
    BadOp,
}

/// Validate a request: `Ok(op)` when the caller may do it.
pub fn authorize(op: u64, arg: u64) -> Result<u64, Refusal> {
    if !credentials::of(task::current()).has_cap(CAP_SYS_ADMIN) {
        return Err(Refusal::NotPermitted);
    }
    match op {
        REBOOT | SHUTDOWN => Ok(op),
        ARM_WATCHDOG if is_stop(arg) => Ok(op),
        _ => Err(Refusal::BadOp),
    }
}

/// Whether `op` stops the machine (what the watchdog may force).
pub fn is_stop(op: u64) -> bool {
    op == REBOOT || op == SHUTDOWN
}

/// The syscall body. Returns on refusal and for [`ARM_WATCHDOG`]; a permitted
/// stop never returns (the machine resets, powers off, or halts).
pub fn dispatch(op: u64, arg: u64) -> u64 {
    match authorize(op, arg) {
        Ok(ARM_WATCHDOG) => {
            watchdog::arm(arg, task::ticks());
            0
        }
        // The in-kernel suite proves the gate only: acting would end the VM.
        #[cfg(lazyos_tests)]
        Ok(_) => 0,
        #[cfg(not(lazyos_tests))]
        Ok(op) => stop(op),
        Err(Refusal::NotPermitted) => (-EPERM) as u64,
        Err(Refusal::BadOp) => (-EINVAL) as u64,
    }
}

/// Sync and stop the machine with `op` ([`REBOOT`] or anything else for
/// power-off).
#[cfg_attr(lazyos_tests, allow(dead_code))] // the suite proves the gate only
pub(crate) fn stop(op: u64) -> ! {
    if op == REBOOT {
        reboot()
    } else {
        shutdown()
    }
}

/// Flush every filesystem before the machine stops, so a clean shutdown leaves
/// a consistent volume (the ext2 data volume is marked clean here). Runs with
/// interrupts still on: the block drivers and the mount lock expect to be
/// scheduled normally. A failure is logged, never fatal -- the stop proceeds,
/// and the volume stays dirty so the next mount knows.
#[cfg_attr(lazyos_tests, allow(dead_code))] // the suite proves the gate only
fn sync_filesystems() {
    // Everything but the kernel task and the caller should be gone by now:
    // `init` stops userspace first. A survivor may still be writing, so say so
    // (the sync itself is still safe: each write is atomic under the VFS lock).
    let busy = task::MAX_TASKS.saturating_sub(task::free_slots() + 2);
    if busy > 0 {
        crate::serial_println!("power: warning: {} other task slot(s) in use at sync", busy);
    }
    match crate::fs::sync_all() {
        Ok(()) => crate::serial_println!("power: filesystems synced"),
        Err(error) => crate::serial_println!("power: sync failed: {}", error.message()),
    }
    // NVMe controllers write their caches back and say when power may go
    // (docs/nvme-install-plan.md N1); nothing writes after this.
    crate::block::nvme::shutdown_all();
}

/// The 8042 status port and its input-buffer-full bit.
const I8042_STATUS: u16 = 0x64;
const I8042_INPUT_FULL: u8 = 1 << 1;
/// How many status reads to wait for the 8042 to take a command.
const I8042_SPINS: u32 = 100_000;

#[cfg_attr(lazyos_tests, allow(dead_code))] // the suite proves the gate only
fn reboot() -> ! {
    crate::serial_println!("power: reboot requested");
    crate::serial::flush();
    sync_filesystems();
    x86_64::instructions::interrupts::disable();
    // SAFETY: port I/O on the 8042 controller. Waiting for its input buffer
    // to drain, then 0xFE to the command port, pulses the CPU reset line: the
    // standard PC reset, only reached by a CAP_SYS_ADMIN caller (or the
    // watchdog acting for one).
    unsafe {
        for _ in 0..I8042_SPINS {
            if inb(I8042_STATUS) & I8042_INPUT_FULL == 0 {
                break;
            }
        }
        outb(I8042_STATUS, 0xFE);
    }
    settle();
    // No 8042 (a legacy-free machine) or it ignored us: a triple fault resets
    // every x86 CPU.
    crate::serial_println!("power: 8042 reset ignored; forcing a triple fault");
    triple_fault()
}

/// Reset the CPU by faulting with no usable IDT: the #BP cannot be delivered,
/// the resulting #DF cannot either, and the third fault is a shutdown cycle,
/// which the chipset turns into a reset.
#[cfg_attr(lazyos_tests, allow(dead_code))] // the suite proves the gate only
fn triple_fault() -> ! {
    use x86_64::structures::DescriptorTablePointer;
    use x86_64::VirtAddr;
    let empty = DescriptorTablePointer {
        limit: 0,
        base: VirtAddr::new(0),
    };
    // SAFETY: interrupts are off and the machine is being reset on purpose:
    // loading an empty IDT and raising an exception is the reset itself. The
    // pointer is a valid (if empty) descriptor; nothing runs afterwards.
    unsafe {
        x86_64::instructions::tables::lidt(&empty);
        core::arch::asm!("int3", options(nomem, nostack));
    }
    crate::halt()
}

/// Reads of the POST diagnostic port to wait out a stop request (each is a
/// bus cycle of about a microsecond on hardware, a VM exit under a hypervisor).
const SETTLE_READS: u32 = 500_000;

/// Give a reset or power-off request time to take effect before falling back.
/// A hypervisor acts on the port write asynchronously: without this pause the
/// fallback (and its log line) would run before the VM had stopped.
#[cfg_attr(lazyos_tests, allow(dead_code))] // the suite proves the gate only
fn settle() {
    for _ in 0..SETTLE_READS {
        // SAFETY: port 0x80 is the POST diagnostic port; reading it has no
        // effect beyond the bus cycle.
        unsafe { inb(0x80) };
    }
}

#[cfg_attr(lazyos_tests, allow(dead_code))] // the suite proves the gate only
fn shutdown() -> ! {
    crate::serial_println!("power: shutdown requested");
    crate::serial::flush();
    sync_filesystems();
    x86_64::instructions::interrupts::disable();
    // SAFETY: these ports only exist on virtual machines: 0x604 is QEMU's ACPI
    // PM1a control (SLP_TYP=S5|SLP_EN) and 0xB004 the older Bochs/QEMU one; on
    // hardware without them the writes are ignored and we fall through to
    // halting. Only reached by a CAP_SYS_ADMIN caller (or the watchdog).
    unsafe {
        outw(0x604, 0x2000);
        outw(0xB004, 0x2000);
    }
    settle();
    crate::serial_println!("power: no ACPI power-off; it is now safe to turn the machine off");
    crate::halt()
}
