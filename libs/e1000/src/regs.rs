//! The 8254x register file the driver uses (Intel 8254x Family of Gigabit
//! Ethernet Controllers Software Developer's Manual, section 13), and the
//! [`Regs`] accessor the rest of the crate goes through.
//!
//! Only the registers the legacy-descriptor driver needs are named: no
//! offloads, no VLAN filter, no flow control, no statistics block.

/// Device control.
pub const CTRL: u32 = 0x0000;
/// Device status (read only).
pub const STATUS: u32 = 0x0008;
/// EEPROM read (82540/82545-style layout).
pub const EERD: u32 = 0x0014;
/// Interrupt cause read; reading clears it and deasserts the line.
pub const ICR: u32 = 0x00C0;
/// Interrupt mask set.
pub const IMS: u32 = 0x00D0;
/// Interrupt mask clear.
pub const IMC: u32 = 0x00D8;
/// Receive control.
pub const RCTL: u32 = 0x0100;
/// Transmit control.
pub const TCTL: u32 = 0x0400;
/// Transmit inter-packet gap.
pub const TIPG: u32 = 0x0410;
/// Receive descriptor base (low, high), length, head and tail.
pub const RDBAL: u32 = 0x2800;
pub const RDBAH: u32 = 0x2804;
pub const RDLEN: u32 = 0x2808;
pub const RDH: u32 = 0x2810;
pub const RDT: u32 = 0x2818;
/// Transmit descriptor base (low, high), length, head and tail.
pub const TDBAL: u32 = 0x3800;
pub const TDBAH: u32 = 0x3804;
pub const TDLEN: u32 = 0x3808;
pub const TDH: u32 = 0x3810;
pub const TDT: u32 = 0x3818;
/// Multicast table array: 128 words.
pub const MTA: u32 = 0x5200;
pub const MTA_WORDS: u32 = 128;
/// Receive address 0 (low four bytes, then two bytes and the valid bit).
pub const RAL0: u32 = 0x5400;
pub const RAH0: u32 = 0x5404;

/// The highest register offset the driver touches, plus its width: a mapped
/// BAR smaller than this cannot be an 8254x.
pub const MIN_BAR_BYTES: u64 = 0x5408;

/// `CTRL` bits.
pub mod ctrl {
    /// Auto-speed detection.
    pub const ASDE: u32 = 1 << 5;
    /// Set link up.
    pub const SLU: u32 = 1 << 6;
    /// Device reset; self-clearing.
    pub const RST: u32 = 1 << 26;
    /// PHY reset.
    pub const PHY_RST: u32 = 1 << 31;
}

/// `STATUS` bits.
pub mod status {
    /// Link up.
    pub const LU: u32 = 1 << 1;
}

/// Interrupt cause bits (`ICR`, `IMS`, `IMC`).
pub mod int {
    /// A transmit descriptor was written back.
    pub const TXDW: u32 = 1 << 0;
    /// Link status change.
    pub const LSC: u32 = 1 << 2;
    /// Receive descriptors below the minimum threshold.
    pub const RXDMT0: u32 = 1 << 4;
    /// Receiver overrun.
    pub const RXO: u32 = 1 << 6;
    /// Receive timer: a frame was written back.
    pub const RXT0: u32 = 1 << 7;
    /// What the driver listens for.
    pub const WANTED: u32 = TXDW | LSC | RXDMT0 | RXO | RXT0;
    /// Every cause.
    pub const ALL: u32 = u32::MAX;
}

/// `RCTL` bits.
pub mod rctl {
    /// Receiver enable.
    pub const EN: u32 = 1 << 1;
    /// Unicast promiscuous.
    pub const UPE: u32 = 1 << 3;
    /// Multicast promiscuous.
    pub const MPE: u32 = 1 << 4;
    /// Accept broadcast.
    pub const BAM: u32 = 1 << 15;
    /// Strip the Ethernet CRC.
    pub const SECRC: u32 = 1 << 26;
    // Buffer size 2048 is `BSIZE = 00` with `BSEX = 0`: no bits to set.
}

/// `TCTL` bits and fields.
pub mod tctl {
    /// Transmitter enable.
    pub const EN: u32 = 1 << 1;
    /// Pad short packets.
    pub const PSP: u32 = 1 << 3;
    /// Collision threshold (the manual's recommended 0x0F).
    pub const CT: u32 = 0x0F << 4;
    /// Collision distance for full duplex (0x40).
    pub const COLD: u32 = 0x40 << 12;
}

/// The inter-packet gap the manual recommends for IEEE 802.3 copper:
/// IPGT 10, IPGR1 8, IPGR2 6.
pub const TIPG_COPPER: u32 = 10 | 8 << 10 | 6 << 20;

/// `EERD` fields (the 82540/82545/82546 layout QEMU models).
pub mod eerd {
    pub const START: u32 = 1 << 0;
    pub const DONE: u32 = 1 << 4;
    pub const ADDR_SHIFT: u32 = 8;
    pub const DATA_SHIFT: u32 = 16;
}

/// `RAH` valid bit.
pub const RAH_AV: u32 = 1 << 31;

/// 32-bit register access. The driver's implementation is the mapped BAR
/// ([`Mmio`]); the host tests implement it with a model of the device.
pub trait Regs {
    fn read(&self, offset: u32) -> u32;
    fn write(&mut self, offset: u32, value: u32);
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
    /// `base` must be the start of a live uncached mapping of the device's
    /// register BAR, at least `len` bytes, that outlives this value.
    pub unsafe fn new(base: *mut u8, len: u64) -> Option<Mmio> {
        (len >= MIN_BAR_BYTES).then_some(Mmio { base, len })
    }

    fn at(&self, offset: u32) -> *mut u32 {
        // Every offset the crate uses is a named constant below
        // `MIN_BAR_BYTES`, so this never fires; it guards a future mistake.
        assert!(u64::from(offset) + 4 <= self.len && offset.is_multiple_of(4));
        // SAFETY: inside the mapping (checked above), 4-byte aligned.
        unsafe { self.base.add(offset as usize) as *mut u32 }
    }
}

impl Regs for Mmio {
    fn read(&self, offset: u32) -> u32 {
        // SAFETY: `at` returns an aligned pointer inside the live mapping.
        unsafe { self.at(offset).read_volatile() }
    }

    fn write(&mut self, offset: u32, value: u32) {
        // SAFETY: as in `read`.
        unsafe { self.at(offset).write_volatile(value) }
    }
}
