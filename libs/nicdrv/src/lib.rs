//! The logic of the virtio-net driver (`netdrv`), as host-testable `no_std`
//! code.
//!
//! The driver binary owns everything that touches the machine: claiming the PCI
//! function, mapping BARs, allocating DMA, Messenger and the syscalls. This
//! crate owns everything a bug in could hurt a neighbour, and so is tested
//! against a hostile device and a hostile client on the host:
//!
//! * [`queues`]: the receive and transmit virtqueues over one DMA block, with a
//!   fixed slot per descriptor, so every buffer the device sees is driver
//!   owned and every completion is bounds-checked;
//! * [`engine`]: the attached client (one at a time), its frame rings, the
//!   frame-length policy, the receive filter and the statistics, pumped from
//!   the driver's loop;
//! * [`arp`]: the ARP request the driver's self-test sends and the reply it
//!   recognises (test traffic, never used to parse anything else);
//! * [`rings`]: the trait a card implements for the engine, so the same
//!   engine drives virtio-net and the Intel 8254x (`libs/e1000`).
//!
//! Nothing here allocates, takes a syscall or trusts a length: the device and
//! the client are both hostile (`docs/networking-plan.md` section 5).

#![no_std]

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod arp;
pub mod engine;
pub mod queues;
pub mod rings;
pub mod stats;
#[cfg(any(test, feature = "fuzz"))]
pub mod testdev;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;

#[cfg(test)]
mod tests;

pub use engine::{Engine, PumpOutcome};
pub use queues::{DmaBlock, Queues, TxError};
pub use rings::{NicRings, RxError};
pub use stats::Stats;

/// Rings the driver can be told to kick: the virtio queue indices.
pub trait Doorbell {
    /// Tell the device new buffers are available on virtqueue `queue`.
    fn ring(&mut self, queue: u16);
}

/// Why the driver cannot continue: the device broke its contract. The binary
/// logs it and exits, and `init` restarts the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fatal {
    /// A virtqueue operation failed in a way only a misbehaving device causes
    /// (an unknown used id, a used index that ran ahead of the driver).
    Device(virtio::Error),
    /// A non-virtio card broke its descriptor contract (a completion on a
    /// descriptor the driver never posted, a head that ran past the tail).
    Hardware(&'static str),
    /// The DMA block or queue sizes do not fit the layout (a driver bug).
    Layout,
}

impl From<virtio::Error> for Fatal {
    fn from(error: virtio::Error) -> Fatal {
        Fatal::Device(error)
    }
}
