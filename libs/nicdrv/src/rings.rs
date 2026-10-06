//! What the engine needs from a network card: a receive path and a transmit
//! path over driver-owned DMA slots.
//!
//! The engine ([`crate::engine`]) owns the client side (rings, frame policy,
//! receive filter, statistics) and is the same for every card. A card's
//! descriptor format, completion rules and doorbell live behind this trait:
//! the split virtqueues of virtio-net ([`crate::Queues`]) and the legacy
//! descriptor rings of the Intel 8254x (`libs/e1000`) both implement it, which
//! is the driver-plan's D7 proof that the core is not virtio-shaped.
//!
//! Every implementation keeps the same promises the engine relies on: the
//! device only ever sees driver-owned memory at driver-chosen addresses,
//! everything it reports back is bounds-checked before use, and a received
//! frame is copied out of its slot before anyone looks at it.

pub use virtio_net::frame::RxError;

use crate::queues::TxError;
use crate::Fatal;

/// The device half of a NIC driver, as the engine pumps it.
pub trait NicRings {
    /// Hand every completed receive buffer to `deliver` as the frame it holds
    /// (a private copy, without any device header), or why it holds none;
    /// return each slot to the device. `max_frame` is the MTU plus the
    /// Ethernet header. Returns the completions handled, at most one ring.
    fn poll_frames(
        &mut self,
        max_frame: usize,
        deliver: impl FnMut(Result<&[u8], RxError>),
    ) -> Result<u32, Fatal>;

    /// Free transmit slots.
    fn tx_free(&self) -> u16;

    /// Queue one frame (already length-checked by the engine); it is copied
    /// into a driver-owned slot and never truncated.
    fn tx_send(&mut self, frame: &[u8]) -> Result<(), TxError>;

    /// Take back the transmit slots the device has finished with.
    fn reap_tx(&mut self) -> Result<u16, Fatal>;

    /// Receive buffers the device currently holds (diagnostics and tests).
    fn rx_in_flight(&self) -> u16;
}
