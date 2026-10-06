//! The HDA controller's register file (High Definition Audio Specification
//! 1.0a, section 3.3) and the [`Regs`] accessor everything goes through.
//!
//! Registers are 8, 16 or 32 bits wide at fixed offsets in BAR 0; the stream
//! descriptors repeat every 0x20 bytes from 0x80, input streams first, then
//! output, then bidirectional.

/// Global capabilities: output/input/bidirectional stream counts, 64-bit OK.
pub const GCAP: u32 = 0x00;
/// Global control: `CRST` takes the link out of reset.
pub const GCTL: u32 = 0x08;
/// State change status: bit `n` set when codec `n` is present.
pub const STATESTS: u32 = 0x0E;
/// Interrupt control and status.
pub const INTCTL: u32 = 0x20;
pub const INTSTS: u32 = 0x24;
/// Command output ring buffer: base, write pointer, read pointer, control,
/// size.
pub const CORBLBASE: u32 = 0x40;
pub const CORBUBASE: u32 = 0x44;
pub const CORBWP: u32 = 0x48;
pub const CORBRP: u32 = 0x4A;
pub const CORBCTL: u32 = 0x4C;
pub const CORBSIZE: u32 = 0x4E;
/// Response input ring buffer: base, write pointer, interrupt count, control,
/// status, size.
pub const RIRBLBASE: u32 = 0x50;
pub const RIRBUBASE: u32 = 0x54;
pub const RIRBWP: u32 = 0x58;
pub const RINTCNT: u32 = 0x5A;
pub const RIRBCTL: u32 = 0x5C;
pub const RIRBSTS: u32 = 0x5D;
pub const RIRBSIZE: u32 = 0x5E;
/// First stream descriptor, and the stride between them.
pub const SD_BASE: u32 = 0x80;
pub const SD_STRIDE: u32 = 0x20;
/// Stream descriptor registers, relative to the descriptor.
pub mod sd {
    /// Control (24 bits: bytes 0..=2) and status (byte 3).
    pub const CTL: u32 = 0x00;
    pub const STS: u32 = 0x03;
    /// Link position in the cyclic buffer, in bytes.
    pub const LPIB: u32 = 0x04;
    /// Cyclic buffer length in bytes.
    pub const CBL: u32 = 0x08;
    /// Last valid index of the buffer descriptor list.
    pub const LVI: u32 = 0x0C;
    /// Stream format.
    pub const FMT: u32 = 0x12;
    /// Buffer descriptor list base.
    pub const BDPL: u32 = 0x18;
    pub const BDPU: u32 = 0x1C;
}

/// The smallest register BAR this driver accepts: the global registers and
/// at least a handful of stream descriptors.
pub const MIN_BAR_BYTES: u64 = 0x200;

/// `GCTL` bits.
pub mod gctl {
    /// Controller reset: 0 holds the link in reset, 1 runs it.
    pub const CRST: u32 = 1 << 0;
}

/// `INTCTL` bits.
pub mod intctl {
    /// Global interrupt enable.
    pub const GIE: u32 = 1 << 31;
    /// Controller (CORB/RIRB) interrupt enable.
    pub const CIE: u32 = 1 << 30;
}

/// `CORBCTL` / `RIRBCTL` bits.
pub mod ring {
    /// CORB DMA run.
    pub const CORB_RUN: u8 = 1 << 1;
    /// RIRB DMA enable.
    pub const RIRB_DMAEN: u8 = 1 << 1;
    /// Response interrupt control: raise `RIRBSTS.RINTFL` every `RINTCNT`
    /// responses. A controller may stop taking commands until that flag is
    /// cleared (QEMU's does), so the driver sets it and clears the flag.
    pub const RIRB_RINTCTL: u8 = 1 << 0;
    /// `RIRBSTS`: response interrupt flag and overrun, write one to clear.
    pub const RIRBSTS_ALL: u8 = 0b101;
    /// Write-pointer / read-pointer reset bit (`CORBRP`, `RIRBWP`).
    pub const PTR_RESET: u16 = 1 << 15;
    /// `CORBSIZE`/`RIRBSIZE` capability bit for 256 entries, and the size
    /// code selecting it.
    pub const CAP_256: u8 = 1 << 6;
    pub const SIZE_256: u8 = 0b10;
}

/// Stream descriptor control bits (byte 0) and the stream tag field.
pub mod sdctl {
    /// Stream reset.
    pub const SRST: u32 = 1 << 0;
    /// DMA run.
    pub const RUN: u32 = 1 << 1;
    /// Interrupt on completion enable.
    pub const IOCE: u32 = 1 << 2;
    /// Stream number (tag) the codec's converter listens for: bits 23..20.
    pub const TAG_SHIFT: u32 = 20;
}

/// Stream descriptor status bits (write 1 to clear).
pub mod sdsts {
    /// Buffer completion interrupt status.
    pub const BCIS: u8 = 1 << 2;
    /// FIFO error, descriptor error.
    pub const FIFOE: u8 = 1 << 3;
    pub const DESE: u8 = 1 << 4;
    pub const ALL: u8 = BCIS | FIFOE | DESE;
}

/// Byte-granular register access. The driver's implementation is the mapped
/// BAR ([`Mmio`]); the host tests implement it with a model of a controller.
pub trait Regs {
    fn read8(&self, offset: u32) -> u8;
    fn read16(&self, offset: u32) -> u16;
    fn read32(&self, offset: u32) -> u32;
    fn write8(&mut self, offset: u32, value: u8);
    fn write16(&mut self, offset: u32, value: u16);
    fn write32(&mut self, offset: u32, value: u32);
}

/// The memory BAR as the driver mapped it.
pub struct Mmio {
    base: *mut u8,
    len: u64,
}

impl Mmio {
    /// Wrap a mapped BAR.
    ///
    /// # Safety
    /// `base` must be the start of a live uncached mapping of the controller's
    /// register BAR, at least `len` bytes, that outlives this value.
    pub unsafe fn new(base: *mut u8, len: u64) -> Option<Mmio> {
        (len >= MIN_BAR_BYTES).then_some(Mmio { base, len })
    }

    /// A pointer to `width` bytes at `offset`. Offsets come from the constants
    /// above and stream descriptors the controller reported, checked against
    /// the BAR before any access.
    fn at(&self, offset: u32, width: u32) -> *mut u8 {
        assert!(u64::from(offset) + u64::from(width) <= self.len && offset.is_multiple_of(width));
        // SAFETY: inside the mapping (checked above).
        unsafe { self.base.add(offset as usize) }
    }
}

impl Regs for Mmio {
    fn read8(&self, offset: u32) -> u8 {
        // SAFETY: `at` returns an aligned pointer inside the live mapping.
        unsafe { self.at(offset, 1).read_volatile() }
    }

    fn read16(&self, offset: u32) -> u16 {
        // SAFETY: as in `read8`.
        unsafe { (self.at(offset, 2) as *const u16).read_volatile() }
    }

    fn read32(&self, offset: u32) -> u32 {
        // SAFETY: as in `read8`.
        unsafe { (self.at(offset, 4) as *const u32).read_volatile() }
    }

    fn write8(&mut self, offset: u32, value: u8) {
        // SAFETY: as in `read8`.
        unsafe { self.at(offset, 1).write_volatile(value) }
    }

    fn write16(&mut self, offset: u32, value: u16) {
        // SAFETY: as in `read8`.
        unsafe { (self.at(offset, 2) as *mut u16).write_volatile(value) }
    }

    fn write32(&mut self, offset: u32, value: u32) {
        // SAFETY: as in `read8`.
        unsafe { (self.at(offset, 4) as *mut u32).write_volatile(value) }
    }
}
