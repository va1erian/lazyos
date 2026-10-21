//! The register BAR as a *gated* [`intelgfx::Mmio`]: read-only until a grant
//! is opened, and then writable only inside the grant.
//!
//! The kernel maps a BAR writable (`dev::map_bar` has no read-only flag), so
//! the guarantee that `gpud` writes only what a stage needs is made here, in
//! layers a later stage must take apart on purpose:
//!
//! * a new [`Bar`] has no write path: [`Mmio::write32`] does nothing but
//!   count the attempt (`refused_writes`, reported on `GPU:DONE`), so even
//!   code that was handed an `Mmio` to program cannot reach the hardware;
//! * [`Bar::with_audio_writes`] (stage G2, started with `audio=1`) opens the
//!   one grant there is, for the length of one closure: the audio registers
//!   `intelgfx::audio::may_write` lists, which are not a mode set. A write anywhere else is still refused
//!   and counted;
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
pub(super) struct Bar {
    base: *const u8,
    len: u64,
    allowed: Vec<(u64, u64)>,
    reads: u32,
    writes: u32,
    audio_writes: bool,
    pub(super) refused_reads: u32,
    pub(super) refused_writes: u32,
}

impl Bar {
    /// Wrap a mapped BAR of the device `vendor:device`.
    ///
    /// # Safety
    /// `base` must be the start of a live uncached mapping of the device's
    /// register BAR, `len` bytes long, that outlives this value (the claim
    /// holds it until the task ends).
    pub(super) unsafe fn new(base: *const u8, len: u64, vendor: u16, device: u16) -> Bar {
        let allowed = pciprobe::builtin::RANGES
            .iter()
            .filter(|r| r.vendor == vendor && r.device == device && r.bar == 0)
            .map(|r| (r.start, r.end))
            .collect();
        Bar {
            base,
            len,
            allowed,
            reads: 0,
            writes: 0,
            audio_writes: false,
            refused_reads: 0,
            refused_writes: 0,
        }
    }

    /// Dwords read so far.
    pub(super) fn reads(&self) -> u32 {
        self.reads
    }

    /// Dwords written so far.
    pub(super) fn writes(&self) -> u32 {
        self.writes
    }

    /// Run `f` with the audio-register write grant open (stage G2), and close
    /// it again however `f` ends: the grant never outlives the one operation
    /// that needs it, because this `Bar` goes on serving reads for the life of
    /// the service. Nothing but the audio registers becomes writable.
    pub(super) fn with_audio_writes<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.audio_writes = true;
        let result = f(self);
        self.audio_writes = false;
        result
    }

    fn may_write(&self, offset: u32) -> bool {
        self.audio_writes && intelgfx::audio::may_write(offset) && u64::from(offset) + 4 <= self.len
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

impl Mmio for Bar {
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

    fn write32(&mut self, offset: u32, value: u32) {
        if !self.may_write(offset) {
            self.refused_writes = self.refused_writes.saturating_add(1);
            return;
        }
        self.writes = self.writes.saturating_add(1);
        // SAFETY: `may_write` checked that the dword is inside the live
        // mapping, aligned, and one of the audio registers the grant names;
        // a volatile write of exactly the register's width.
        unsafe {
            self.base
                .cast_mut()
                .add(offset as usize)
                .cast::<u32>()
                .write_volatile(value)
        }
    }
}
