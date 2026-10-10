//! The engine's rings for whichever card this driver claimed: virtio-net's
//! split virtqueues or the 8254x's descriptor rings. A plain enum, so the
//! engine stays one monomorphic type and the service code is card-blind.

use alloc::boxed::Box;

use e1000::{Mmio, Rings};
use nicdrv::{Fatal, NicRings, Queues, RxError, TxError};
use rtl8168::{Mmio as RtlMmio, Rings as RtlRings};

pub(super) enum AnyRings {
    // Boxed: the two differ in size by kilobytes, and there is one per driver.
    Virtio(Box<Queues>),
    E1000(Box<Rings<Mmio>>),
    Rtl8168(Box<RtlRings<RtlMmio>>),
}

impl NicRings for AnyRings {
    fn poll_frames(
        &mut self,
        max_frame: usize,
        deliver: impl FnMut(Result<&[u8], RxError>),
    ) -> Result<u32, Fatal> {
        match self {
            AnyRings::Virtio(queues) => NicRings::poll_frames(&mut **queues, max_frame, deliver),
            AnyRings::E1000(rings) => rings.poll_frames(max_frame, deliver),
            AnyRings::Rtl8168(rings) => rings.poll_frames(max_frame, deliver),
        }
    }

    fn tx_free(&self) -> u16 {
        match self {
            AnyRings::Virtio(queues) => NicRings::tx_free(&**queues),
            AnyRings::E1000(rings) => rings.tx_free(),
            AnyRings::Rtl8168(rings) => rings.tx_free(),
        }
    }

    fn tx_send(&mut self, frame: &[u8]) -> Result<(), TxError> {
        match self {
            AnyRings::Virtio(queues) => NicRings::tx_send(&mut **queues, frame),
            AnyRings::E1000(rings) => rings.tx_send(frame),
            AnyRings::Rtl8168(rings) => rings.tx_send(frame),
        }
    }

    fn reap_tx(&mut self) -> Result<u16, Fatal> {
        match self {
            AnyRings::Virtio(queues) => NicRings::reap_tx(&mut **queues),
            AnyRings::E1000(rings) => rings.reap_tx(),
            AnyRings::Rtl8168(rings) => rings.reap_tx(),
        }
    }

    fn rx_in_flight(&self) -> u16 {
        match self {
            AnyRings::Virtio(queues) => NicRings::rx_in_flight(&**queues),
            AnyRings::E1000(rings) => rings.rx_in_flight(),
            AnyRings::Rtl8168(rings) => rings.rx_in_flight(),
        }
    }
}
