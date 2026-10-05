//! NVMe (NVM Express 1.4, PCIe transport) for the kernel's block driver
//! (`docs/nvme-install-plan.md`, phase N1).
//!
//! Pure `no_std` logic over a [`Platform`] the kernel supplies (the mapped
//! BAR0, physical memory for the queues, a clock), so the whole driver, from
//! controller reset to a vectored write, runs under host `cargo test` against
//! a model controller (`src/tests/model.rs`).
//!
//! * [`regs`]: controller registers (`CAP`, `CC`, `CSTS`, `AQA`, ...) and the
//!   doorbell stride.
//! * [`cmd`]: the 64-byte submission entry and the 16-byte completion entry.
//! * [`identify`]: the Identify Controller and Identify Namespace pages.
//! * [`prp`]: cutting a caller's scattered buffers into commands whose data
//!   pointers obey the PRP rules.
//! * [`queue`]: a submission/completion queue pair with its phase bit.
//! * [`controller`]: bring-up (reset, admin queue, Identify, one I/O queue
//!   pair), polled reads, writes and flushes, and the shutdown notification.
//!
//! Everything the controller writes (completions, Identify pages, register
//! values) is untrusted: a completion naming a command that is not in flight
//! is dropped, an Identify page that does not add up refuses the namespace,
//! and every wait is bounded, so a broken controller is detached, never
//! waited on forever.

#![no_std]

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod cmd;
pub mod controller;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod identify;
pub mod prp;
pub mod queue;
pub mod regs;
#[cfg(test)]
mod tests;

pub use controller::{Controller, Op, Pages, MAX_INFLIGHT};
pub use identify::{ControllerInfo, Namespace};

/// The memory page size the driver programs (`CC.MPS = 0`): PRP entries and
/// queues are laid out in 4 KiB pages.
pub const PAGE: u64 = 4096;

/// What the driver needs from the machine: the controller's registers, the
/// physical memory the controller reads and writes, and a clock.
///
/// Register offsets are from the start of BAR0. Memory accesses name
/// physical addresses the caller handed in ([`Pages`] and the translated data
/// buffers); the kernel reaches them through its physical-memory map.
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
    /// The controller did not reach the state it was asked for in time
    /// (`CSTS.RDY`, `CSTS.SHST`, or a command's completion).
    Timeout,
    /// The controller reported a fatal status (`CSTS.CFS`).
    Fatal,
    /// The controller or namespace is outside what this driver supports; the
    /// reason is for the log.
    Unsupported(&'static str),
    /// A command completed with a non-zero status (type, code).
    Status { sct: u8, sc: u8 },
    /// A buffer page has no physical address.
    Unmapped,
    /// A buffer cannot be described with PRP entries (not dword aligned, or
    /// less than one block before a break).
    Misaligned,
    /// The range lies outside the namespace, or is not a whole number of
    /// blocks.
    Bounds,
    /// The controller was detached after an earlier failure.
    Detached,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Timeout => write!(f, "timed out"),
            Error::Fatal => write!(f, "controller fatal status"),
            Error::Unsupported(why) => write!(f, "unsupported: {why}"),
            Error::Status { sct, sc } => write!(f, "status type {sct:#x} code {sc:#04x}"),
            Error::Unmapped => write!(f, "unmapped buffer"),
            Error::Misaligned => write!(f, "buffer not PRP-describable"),
            Error::Bounds => write!(f, "out of range"),
            Error::Detached => write!(f, "controller detached"),
        }
    }
}
