//! AHCI (Serial ATA AHCI 1.3.1) for the kernel's block driver
//! (`docs/ahci-plan.md`, phase A1).
//!
//! Pure `no_std` logic over a [`Platform`] the kernel supplies (the mapped
//! ABAR, physical memory for the command structures, a clock), so the whole
//! driver, from BIOS handoff to a vectored write, runs under host `cargo
//! test` against a model HBA (`src/tests/model.rs`).
//!
//! * [`regs`]: HBA and port registers and bits.
//! * [`fis`]: the Register Host-to-Device FIS and the ATA opcodes used.
//! * [`cmd`]: command headers, PRDT entries and the planner that cuts a
//!   caller's scattered buffers into commands.
//! * [`identify`]: IDENTIFY DEVICE parsing and the refusals.
//! * [`hba`]: handoff, `GHC.AE`, and port discovery and bring-up.
//! * [`reset`]: stopping and starting a port, COMRESET, recovery.
//! * [`port`]: IDENTIFY, polled reads, writes, flushes and standby, with up
//!   to [`MAX_SLOTS`] commands of a transfer in flight.
//!
//! Everything the HBA or the disk reports (registers, the received PRDBC,
//! IDENTIFY words) is untrusted, and every wait is bounded: a port that does
//! not answer is stopped and, if it keeps failing, detached; it is never
//! waited on forever.

#![no_std]

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod cmd;
pub mod fis;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod hba;
pub mod identify;
pub mod port;
pub mod regs;
pub mod reset;
#[cfg(test)]
mod tests;

pub use cmd::MAX_SLOTS;
pub use hba::{Hba, Skip};
pub use identify::Disk;
pub use port::{Op, Port, PortPages};

/// What the driver needs from the machine: the HBA's registers, the
/// physical memory it reads and writes, and a clock.
///
/// Register offsets are from the start of ABAR.
pub trait Platform {
    /// Read the 32-bit register at `offset`.
    fn read32(&self, offset: usize) -> u32;
    /// Write the 32-bit register at `offset`.
    fn write32(&self, offset: usize, value: u32);
    /// Copy `buf.len()` bytes of physical memory at `phys` into `buf`.
    fn read_mem(&self, phys: u64, buf: &mut [u8]);
    /// Copy `data` to physical memory at `phys`.
    fn write_mem(&self, phys: u64, data: &[u8]);
    /// A monotonic clock in nanoseconds.
    fn now_ns(&self) -> u64;
    /// Pause between two polls of a bring-up wait.
    fn relax(&self) {}
}

/// Why the driver refused or failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A state the port or HBA was asked for did not arrive in time.
    Timeout,
    /// The HBA or port is in a state the driver cannot recover from.
    Fatal,
    /// Outside what this driver supports; the reason is for the log.
    Unsupported(&'static str),
    /// The disk reported an error (`PxTFD`: status, error).
    TaskFile { status: u8, error: u8 },
    /// The HBA reported a bus or interface error (`PxIS`).
    Bus(u32),
    /// The HBA moved a different number of bytes than the command asked.
    ShortTransfer,
    /// A buffer page has no physical address.
    Unmapped,
    /// A buffer cannot be described to the HBA (odd address or length, above
    /// 4 GiB on a 32-bit HBA): the caller bounces it.
    Misaligned,
    /// The range lies outside the disk, or is not whole sectors.
    Bounds,
    /// The port was detached after repeated failures.
    Detached,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Timeout => write!(f, "timed out"),
            Error::Fatal => write!(f, "unrecoverable port state"),
            Error::Unsupported(why) => write!(f, "unsupported: {why}"),
            Error::TaskFile { status, error } => {
                write!(f, "disk error (status {status:#04x}, error {error:#04x})")
            }
            Error::Bus(is) => write!(f, "bus error (PxIS {is:#010x})"),
            Error::ShortTransfer => write!(f, "short transfer"),
            Error::Unmapped => write!(f, "unmapped buffer"),
            Error::Misaligned => write!(f, "buffer not describable to the HBA"),
            Error::Bounds => write!(f, "out of range"),
            Error::Detached => write!(f, "port detached"),
        }
    }
}

/// Poll `ready` until it holds or `timeout_ns` passes; bounded even when
/// the clock stands still.
pub(crate) fn poll(
    platform: &dyn Platform,
    timeout_ns: u64,
    mut ready: impl FnMut() -> bool,
) -> bool {
    const BACKSTOP: u64 = 20_000_000;
    let start = platform.now_ns();
    for _ in 0..BACKSTOP {
        if ready() {
            return true;
        }
        if platform.now_ns().saturating_sub(start) >= timeout_ns {
            return ready();
        }
        platform.relax();
    }
    ready()
}

pub(crate) const MS: u64 = 1_000_000;
