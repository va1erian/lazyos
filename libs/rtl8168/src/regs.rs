//! The register file the driver uses, and the [`Regs`] accessor the rest of
//! the crate goes through.
//!
//! No datasheet is public. The offsets and bits below are the ones every open
//! driver for the family agrees on, written from an understanding of what each
//! register does (docs/rtl8168-driver-plan.md section 3.1). They are checked
//! against the box with `tools/net/rtl8168/` (a register dump from Linux, and
//! the driver's own `NETDRV:REGS` lines), not trusted.
//!
//! Registers are 8, 16 or 32 bits wide, and the width matters: `IntrMask` and
//! `IntrStatus` sit side by side and `IntrStatus` is write-one-to-clear, so a
//! 32-bit write meant for the mask would also acknowledge causes.

/// Station address, bytes 0..=3 (read as a dword), then bytes 4..=5.
pub const IDR0: u32 = 0x00;
pub const IDR4: u32 = 0x04;
/// Multicast hash filter, 8 bytes.
pub const MAR0: u32 = 0x08;
/// Transmit normal-priority descriptor ring base (low, high).
pub const TNPDS_LO: u32 = 0x20;
pub const TNPDS_HI: u32 = 0x24;
/// Command: reset, receive enable, transmit enable (8 bit).
pub const CHIP_CMD: u32 = 0x37;
/// Transmit doorbell (8 bit).
pub const TX_POLL: u32 = 0x38;
/// Interrupt mask (16 bit).
pub const INTR_MASK: u32 = 0x3C;
/// Interrupt status (16 bit, write one to clear).
pub const INTR_STATUS: u32 = 0x3E;
/// Transmit configuration (32 bit): DMA burst, gap, and the XID.
pub const TX_CONFIG: u32 = 0x40;
/// Receive configuration (32 bit).
pub const RX_CONFIG: u32 = 0x44;
/// Config-register write lock (8 bit).
pub const CFG9346: u32 = 0x50;
/// PHY register access over the MAC (32 bit).
pub const PHYAR: u32 = 0x60;
/// Link, speed, duplex (8 bit).
pub const PHY_STATUS: u32 = 0x6C;
/// Largest frame received (16 bit).
pub const RX_MAX_SIZE: u32 = 0xDA;
/// C+ command: descriptor mode, checksum and VLAN offloads (16 bit).
pub const CPLUS_CMD: u32 = 0xE0;
/// Receive descriptor ring base (low, high).
pub const RDSAR_LO: u32 = 0xE4;
pub const RDSAR_HI: u32 = 0xE8;
/// Transmit size limit (8 bit, units of 128 bytes).
pub const MAX_TX_PACKET: u32 = 0xEC;

/// Bytes of register space the driver reads or writes; a mapped BAR smaller
/// than this cannot be this chip.
pub const MIN_BAR_BYTES: u64 = 0x100;
/// The register block a diagnosis dump covers (the chip's first 256 bytes).
pub const DUMP_BYTES: usize = 256;

/// `ChipCmd` bits.
pub mod cmd {
    /// Soft reset; self-clearing.
    pub const RESET: u8 = 1 << 4;
    /// Receiver enable.
    pub const RX_ENABLE: u8 = 1 << 3;
    /// Transmitter enable.
    pub const TX_ENABLE: u8 = 1 << 2;
}

/// `TxPoll` bit: poll the normal-priority transmit queue.
pub const TX_POLL_NPQ: u8 = 1 << 6;

/// Interrupt causes (`IntrStatus`, `IntrMask`).
pub mod int {
    pub const RX_OK: u16 = 1 << 0;
    pub const RX_ERR: u16 = 1 << 1;
    pub const TX_OK: u16 = 1 << 2;
    pub const TX_ERR: u16 = 1 << 3;
    /// The receive descriptor ring ran out of buffers.
    pub const RX_UNAVAIL: u16 = 1 << 4;
    pub const LINK_CHANGE: u16 = 1 << 5;
    pub const RX_FIFO_OVER: u16 = 1 << 6;
    pub const TX_UNAVAIL: u16 = 1 << 7;
    /// A PCI error the chip reports; the driver restarts.
    pub const SYS_ERR: u16 = 1 << 15;
    /// What the driver listens for.
    pub const WANTED: u16 =
        RX_OK | RX_ERR | TX_OK | TX_ERR | RX_UNAVAIL | LINK_CHANGE | RX_FIFO_OVER | SYS_ERR;
    /// Causes that mean frames were lost but the chip carries on.
    pub const LOSS: u16 = RX_UNAVAIL | RX_FIFO_OVER | RX_ERR;
}

/// `Cfg9346` values: writes to the config registers are accepted only while
/// unlocked.
pub mod cfg9346 {
    pub const UNLOCK: u8 = 0xC0;
    pub const LOCK: u8 = 0x00;
}

/// `TxConfig`: the XID field and the fields the driver sets.
pub mod tx_config {
    /// The bits (after shifting right by [`XID_SHIFT`]) that identify the
    /// chip revision: the chip scatters them over two groups.
    pub const XID_MASK: u32 = 0xFCF;
    pub const XID_SHIFT: u32 = 20;
    /// Unlimited DMA burst.
    pub const DMA_BURST: u32 = 7 << 8;
    /// The standard inter-frame gap (96 bit times).
    pub const INTER_FRAME_GAP: u32 = 3 << 24;
}

/// `RxConfig` bits.
pub mod rx_config {
    pub const ACCEPT_ALL_PHYS: u32 = 1 << 0;
    pub const ACCEPT_MY_PHYS: u32 = 1 << 1;
    pub const ACCEPT_MULTICAST: u32 = 1 << 2;
    pub const ACCEPT_BROADCAST: u32 = 1 << 3;
    pub const ACCEPT_RUNT: u32 = 1 << 4;
    pub const ACCEPT_ERR: u32 = 1 << 5;
    /// Unlimited DMA burst.
    pub const DMA_BURST: u32 = 7 << 8;
    /// No receive FIFO threshold: start the DMA when a whole frame is in.
    pub const FIFO_THRESHOLD: u32 = 7 << 13;
}

/// `CPlusCmd` bits the driver clears: no receive checksum offload, no VLAN
/// tag stripping, so the frame the device wrote is the frame on the wire.
pub mod cplus {
    pub const RX_VLAN: u16 = 1 << 6;
    pub const RX_CHECKSUM: u16 = 1 << 5;
}

/// `PHYstatus` bits.
pub mod phy_status {
    pub const FULL_DUPLEX: u8 = 1 << 0;
    pub const LINK: u8 = 1 << 1;
    pub const SPEED_10: u8 = 1 << 2;
    pub const SPEED_100: u8 = 1 << 3;
    pub const SPEED_1000: u8 = 1 << 4;
}

/// `PHYAR` fields: a flag bit that means "done" for a read and "busy" for a
/// write, the register number and 16 bits of data.
pub mod phyar {
    pub const FLAG: u32 = 1 << 31;
    pub const REG_SHIFT: u32 = 16;
    pub const REG_MASK: u32 = 0x1F;
    pub const DATA_MASK: u32 = 0xFFFF;
}

/// The largest frame the chip is told to receive: a standard frame (1518
/// bytes with its FCS) plus a VLAN tag, rounded to a multiple of 8. Less than
/// a 2048-byte slot, so a frame never spills over a descriptor.
pub const RX_MAX_FRAME: u16 = 1528;
/// `MaxTxPacketSize` for 8064 bytes, the largest the register takes.
pub const MAX_TX_UNITS: u8 = 0x3F;

/// Register access at the width the chip defines. The driver's
/// implementation is the mapped BAR ([`Mmio`]); the host tests implement it
/// with a model of the device.
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
    /// `base` must be the start of a live uncached mapping of the device's
    /// register BAR, at least `len` bytes, that outlives this value.
    pub unsafe fn new(base: *mut u8, len: u64) -> Option<Mmio> {
        (len >= MIN_BAR_BYTES).then_some(Mmio { base, len })
    }

    fn at<T>(&self, offset: u32) -> *mut T {
        let width = core::mem::size_of::<T>() as u64;
        // Every offset the crate uses is a named constant below
        // `MIN_BAR_BYTES` and naturally aligned, so this never fires; it
        // guards a future mistake from becoming a stray device access.
        assert!(u64::from(offset) + width <= self.len && u64::from(offset) % width == 0);
        // SAFETY: inside the mapping (checked above), naturally aligned.
        unsafe { self.base.add(offset as usize) as *mut T }
    }
}

impl Regs for Mmio {
    fn read8(&self, offset: u32) -> u8 {
        // SAFETY: `at` returns an aligned pointer inside the live mapping.
        unsafe { self.at::<u8>(offset).read_volatile() }
    }

    fn read16(&self, offset: u32) -> u16 {
        // SAFETY: as in `read8`.
        unsafe { self.at::<u16>(offset).read_volatile() }
    }

    fn read32(&self, offset: u32) -> u32 {
        // SAFETY: as in `read8`.
        unsafe { self.at::<u32>(offset).read_volatile() }
    }

    fn write8(&mut self, offset: u32, value: u8) {
        // SAFETY: as in `read8`.
        unsafe { self.at::<u8>(offset).write_volatile(value) }
    }

    fn write16(&mut self, offset: u32, value: u16) {
        // SAFETY: as in `read8`.
        unsafe { self.at::<u16>(offset).write_volatile(value) }
    }

    fn write32(&mut self, offset: u32, value: u32) {
        // SAFETY: as in `read8`.
        unsafe { self.at::<u32>(offset).write_volatile(value) }
    }
}
