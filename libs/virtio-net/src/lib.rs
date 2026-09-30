//! The virtio-net (device type 1) wire definitions, as pure `no_std` logic.
//!
//! Feature bits, the device configuration layout, the 12-byte packet header,
//! the frame-length policy and the driver's clamped settings live here, so the
//! `netdrv` driver only feeds bytes through virtqueues and host tests check the
//! layouts against virtio 1.2 section 5.1.
//!
//! Everything a device returns is untrusted: parsers bounds-check and return
//! `None` or an error instead of indexing (`docs/networking-plan.md` section 5).

#![no_std]

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod config;
pub mod features;
pub mod frame;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod hdr;
pub mod settings;

/// Virtio device type of the network device.
pub const DEVICE_TYPE: u16 = 1;

/// Virtqueue indices without multiqueue: receive, transmit. (The control
/// queue, when negotiated, follows at `2 * max_virtqueue_pairs`; this driver
/// does not negotiate it.)
pub mod queue {
    pub const RX: u16 = 0;
    pub const TX: u16 = 1;
}

/// Bytes of an Ethernet header: destination, source, EtherType.
pub const ETH_HEADER: usize = 14;
/// The largest MTU this driver runs: the classic Ethernet payload. A frame is
/// then at most [`MAX_FRAME`] bytes, which with the 12-byte packet header fits a
/// 2048-byte slot and a `framering` slot.
pub const MAX_MTU: u16 = 1500;
/// The smallest MTU a setting may ask for (RFC 791: every host accepts 576).
pub const MIN_MTU: u16 = 576;
/// Largest complete frame at [`MAX_MTU`].
pub const MAX_FRAME: usize = MAX_MTU as usize + ETH_HEADER;
/// Bytes of one DMA slot: the packet header plus a frame, rounded up so a slot
/// is a fixed, aligned stride.
pub const SLOT_BYTES: usize = 2048;

const _: () = assert!(hdr::HDR_LEN + MAX_FRAME <= SLOT_BYTES);
