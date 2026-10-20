//! The register BAR as a *read-only* [`intelgfx::Mmio`].
//!
//! The kernel maps a BAR writable (`dev::map_bar` has no read-only flag), so
//! the guarantee that stage G1's `gpud` writes no display register is made
//! here, in two layers a later stage must take apart on purpose:
//!
//! * the type has no write path: [`Mmio::write32`] does nothing but count
//!   the attempt (`refused_writes`, reported on `GPU:DONE`), so even code
//!   that was handed an `Mmio` to program cannot reach the hardware;
//! * a read is performed only for a dword that lies inside a range
//!   `libs/pciprobe` lets `dbgd` read for this exact device (the same
//!   allowlist that keeps read-sensitive registers, GMBUS data and the ELD
//!   port, out), so a register with a side effect on read stays unreachable
//!   until somebody has listed it there.
//!
//! A refused or out-of-range read returns all ones, which `intelgfx::adopt`
//! treats like a dead bus, and is counted (`refused_reads`).

use alloc::vec::Vec;
use intelgfx::Mmio;

/// A mapped register BAR and the ranges reads may touch.
pub(super) struct ReadOnlyBar {
    base: *const u8,
    len: u64,
    allowed: Vec<(u64, u64)>,
    reads: u32,
    pub(super) refused_reads: u32,
    pub(super) refused_writes: u32,
}

impl ReadOnlyBar {
    /// Wrap a mapped BAR of the device `vendor:device`.
    ///
    /// # Safety
    /// `base` must be the start of a live uncached mapping of the device's
    /// register BAR, `len` bytes long, that outlives this value (the claim
    /// holds it until the task ends).
    pub(super) unsafe fn new(base: *const u8, len: u64, vendor: u16, device: u16) -> ReadOnlyBar {
        let allowed = pciprobe::builtin::RANGES
            .iter()
            .filter(|r| r.vendor == vendor && r.device == device && r.bar == 0)
            .map(|r| (r.start, r.end))
            .collect();
        ReadOnlyBar {
            base,
            len,
            allowed,
            reads: 0,
            refused_reads: 0,
            refused_writes: 0,
        }
    }

    /// Dwords read so far.
    pub(super) fn reads(&self) -> u32 {
        self.reads
    }

    fn may_read(&self, offset: u32) -> bool {
        let offset = u64::from(offset);
        offset % 4 == 0
            && offset + 4 <= self.len
            && self
                .allowed
                .iter()
                .any(|&(start, end)| start <= offset && offset + 4 <= end)
    }
}

impl Mmio for ReadOnlyBar {
    fn read32(&mut self, offset: u32) -> u32 {
        if !self.may_read(offset) {
            self.refused_reads = self.refused_reads.saturating_add(1);
            return u32::MAX;
        }
        self.reads = self.reads.saturating_add(1);
        // SAFETY: `may_read` checked that the dword lies inside the live
        // mapping and is aligned; a volatile read of exactly the register's
        // width, as the device needs.
        unsafe { self.base.add(offset as usize).cast::<u32>().read_volatile() }
    }

    fn write32(&mut self, _offset: u32, _value: u32) {
        // G1 is read-only: there is nothing to write with.
        self.refused_writes = self.refused_writes.saturating_add(1);
    }
}
