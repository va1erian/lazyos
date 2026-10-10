//! Receive and transmit descriptors: 16 bytes each, in driver-owned DMA
//! memory, the same format for both rings.
//!
//! ```text
//!  0: opts1  bit 31 OWN, 30 EOR, 29 FS, 28 LS; the length in the low bits
//!  4: opts2  VLAN and checksum fields (the driver leaves it zero)
//!  8: buffer bus address, 64 bits
//! ```
//!
//! `OWN` set means the device owns the descriptor; the device clears it when
//! it is done (a received frame is in the buffer, a transmitted one is sent).
//! `EOR` marks the last descriptor of the ring, where the device wraps.
//!
//! Descriptors are read and written with volatile accesses through raw
//! pointers: the device writes them back concurrently, so no reference to
//! descriptor memory is ever formed. `OWN` is always the last word written
//! when handing a descriptor over, behind a release fence, so the device never
//! sees half a descriptor.

use core::ptr;
use core::sync::atomic::{fence, Ordering};

/// Bytes per descriptor.
pub const DESC_BYTES: usize = 16;

/// The device owns the descriptor.
pub const OWN: u32 = 1 << 31;
/// End of ring: the device wraps to the first descriptor after this one.
pub const EOR: u32 = 1 << 30;
/// First segment of a frame.
pub const FS: u32 = 1 << 29;
/// Last segment of a frame.
pub const LS: u32 = 1 << 28;

/// Bits of a received descriptor's `opts1` that carry the frame length (which
/// includes the 4-byte FCS the chip does not strip).
pub const RX_LEN_MASK: u32 = 0x3FFF;
/// Bits of a transmit descriptor's `opts1` that carry the length.
pub const TX_LEN_MASK: u32 = 0xFFFF;

/// Receive error bits in `opts1`: receive watchdog timeout, the summary
/// error, runt, CRC.
pub mod rx_err {
    pub const CRC: u32 = 1 << 19;
    pub const RUNT: u32 = 1 << 20;
    pub const RES: u32 = 1 << 21;
    pub const RWT: u32 = 1 << 22;
    /// Every bit the driver refuses a frame for.
    pub const ANY: u32 = CRC | RUNT | RES | RWT;
}

/// Bytes of Ethernet FCS the chip leaves on every received frame and the
/// descriptor length counts.
pub const FCS_BYTES: usize = 4;

/// Read `opts1` of the descriptor at `desc`.
///
/// # Safety
/// `desc` must point at 16 readable bytes.
pub unsafe fn opts1(desc: *const u8) -> u32 {
    ptr::read_volatile(desc as *const u32)
}

/// Give the receive descriptor at `desc` to the device with a buffer of
/// `size` bytes at `bus`; `last` marks the end of the ring.
///
/// # Safety
/// `desc` must point at 16 writable bytes the device does not own.
pub unsafe fn rx_post(desc: *mut u8, bus: u64, size: u32, last: bool) {
    ptr::write_volatile(desc.add(4) as *mut u32, 0);
    ptr::write_volatile(desc.add(8) as *mut u64, bus);
    fence(Ordering::Release);
    let eor = if last { EOR } else { 0 };
    ptr::write_volatile(desc as *mut u32, OWN | eor | (size & RX_LEN_MASK));
    fence(Ordering::Release);
}

/// Hand the transmit descriptor at `desc` to the device for one whole frame
/// of `len` bytes at `bus`; `last` marks the end of the ring.
///
/// # Safety
/// `desc` must point at 16 writable bytes the device does not own.
pub unsafe fn tx_post(desc: *mut u8, bus: u64, len: u16, last: bool) {
    ptr::write_volatile(desc.add(4) as *mut u32, 0);
    ptr::write_volatile(desc.add(8) as *mut u64, bus);
    fence(Ordering::Release);
    let eor = if last { EOR } else { 0 };
    ptr::write_volatile(
        desc as *mut u32,
        OWN | eor | FS | LS | (u32::from(len) & TX_LEN_MASK),
    );
    fence(Ordering::Release);
}
