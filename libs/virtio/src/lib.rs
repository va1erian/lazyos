//! Modern (virtio 1.x) virtio-PCI transport for LazyOS userspace drivers.
//!
//! The crate is pure `no_std` logic over raw memory the *caller* mapped: the
//! driver claims a device through syscall 23, maps its BARs and DMA buffers, and
//! hands this crate the resulting pointers. Nothing here touches a syscall, so
//! the capability parser, register accessors and the split virtqueue are all
//! covered by host `cargo test`.
//!
//! * [`caps`] parses the vendor-specific PCI capabilities that locate the
//!   common, notify, ISR and device configuration structures.
//! * [`transport`] wraps the mapped structures: feature negotiation, status,
//!   queue setup and notification.
//! * [`queue`] is a split virtqueue (descriptor table, available and used
//!   rings) with a fixed-capacity free list, so it needs no allocator.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod caps;
pub mod queue;
pub mod regs;
pub mod transport;

/// Errors from device setup and queue operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A required PCI capability is absent or malformed.
    MissingCapability(&'static str),
    /// The device does not offer a feature the driver requires.
    FeatureUnsupported,
    /// The device cleared `FEATURES_OK` after negotiation.
    FeaturesRejected,
    /// The device did not finish a reset in time.
    ResetTimeout,
    /// The device reports a queue that is absent or unusable.
    BadQueue,
    /// A queue has no free descriptors for the request.
    QueueFull,
    /// A request had no buffers, or more than the queue can chain.
    BadRequest,
    /// The device set `NEEDS_RESET` or `FAILED`.
    DeviceError,
}
