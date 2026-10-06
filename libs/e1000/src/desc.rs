//! Legacy receive and transmit descriptors (8254x manual, sections 3.2.3 and
//! 3.3.3): 16 bytes each, in driver-owned DMA memory.
//!
//! Descriptors are read and written with volatile accesses through raw
//! pointers: the device writes them back concurrently, so no reference to
//! descriptor memory is ever formed.

use core::ptr;

/// Bytes per descriptor.
pub const DESC_BYTES: usize = 16;

/// Receive status bits.
pub mod rx_status {
    /// Descriptor done: the device wrote this descriptor back.
    pub const DD: u8 = 1 << 0;
    /// End of packet: the last descriptor of a frame.
    pub const EOP: u8 = 1 << 1;
}

/// Receive error bits worth refusing a frame for: CRC or alignment, symbol,
/// sequence and carrier-extension errors.
pub const RX_ERRORS: u8 = 0b1010_0111;

/// Transmit command bits.
pub mod tx_cmd {
    /// End of packet.
    pub const EOP: u8 = 1 << 0;
    /// Insert the Ethernet CRC.
    pub const IFCS: u8 = 1 << 1;
    /// Report status: write `DD` back when done.
    pub const RS: u8 = 1 << 3;
}

/// Transmit status: descriptor done.
pub const TX_DD: u8 = 1 << 0;

/// What the device wrote back into a receive descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxDone {
    pub length: u16,
    pub status: u8,
    pub errors: u8,
}

/// Read the write-back fields of the receive descriptor at `desc`.
///
/// # Safety
/// `desc` must point at 16 readable bytes.
pub unsafe fn rx_read(desc: *const u8) -> RxDone {
    RxDone {
        length: ptr::read_volatile(desc.add(8) as *const u16),
        status: ptr::read_volatile(desc.add(12)),
        errors: ptr::read_volatile(desc.add(13)),
    }
}

/// Give the receive descriptor at `desc` to the device with buffer `bus`
/// (status cleared, so a stale `DD` cannot be mistaken for a new frame).
///
/// # Safety
/// `desc` must point at 16 writable bytes the device does not own.
pub unsafe fn rx_post(desc: *mut u8, bus: u64) {
    ptr::write_volatile(desc as *mut u64, bus);
    ptr::write_volatile(desc.add(8) as *mut u64, 0);
}

/// Fill the transmit descriptor at `desc` for one whole frame.
///
/// # Safety
/// `desc` must point at 16 writable bytes the device does not own.
pub unsafe fn tx_post(desc: *mut u8, bus: u64, len: u16) {
    ptr::write_volatile(desc as *mut u64, bus);
    ptr::write_volatile(desc.add(8) as *mut u16, len);
    ptr::write_volatile(desc.add(10), 0); // CSO
    ptr::write_volatile(desc.add(11), tx_cmd::EOP | tx_cmd::IFCS | tx_cmd::RS);
    ptr::write_volatile(desc.add(12), 0); // status
    ptr::write_volatile(desc.add(13), 0); // CSS
    ptr::write_volatile(desc.add(14) as *mut u16, 0); // special
}

/// The status byte of the transmit descriptor at `desc`.
///
/// # Safety
/// `desc` must point at 16 readable bytes.
pub unsafe fn tx_status(desc: *const u8) -> u8 {
    ptr::read_volatile(desc.add(12))
}
