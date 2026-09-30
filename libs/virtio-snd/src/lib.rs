//! The virtio-sound (device type 25) wire protocol and PCM parameter selection,
//! as pure `no_std` logic.
//!
//! Requests are built into fixed byte arrays and replies are parsed from byte
//! slices, so nothing here needs an allocator, a device or a syscall: the
//! `sndd` driver feeds these bytes through virtqueues, and host tests check
//! them against the layouts in virtio 1.2 section 5.14.
//!
//! Everything a device returns is untrusted; parsers bounds-check and return
//! `None` instead of indexing.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod params;
pub mod wire;

/// Virtio device type of the sound device.
pub const DEVICE_TYPE: u16 = 25;

/// Virtqueue indices.
pub mod queue {
    pub const CONTROL: u16 = 0;
    pub const EVENT: u16 = 1;
    pub const TX: u16 = 2;
    pub const RX: u16 = 3;
}
