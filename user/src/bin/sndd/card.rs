//! The card: whichever sound device was claimed, behind one interface.
//!
//! The stream and session code (`stream.rs`, `session.rs`) speak to a card
//! that describes its streams, takes stream parameters, is prepared, started,
//! stopped and released, accepts periods and completes them. virtio-sound
//! does exactly that over its virtqueues (`virtio_card.rs`); an Intel HDA
//! controller plays a cyclic buffer and presents the same model
//! (`hda_card.rs`).

use alloc::boxed::Box;
use alloc::vec::Vec;

use virtio_snd::wire::PcmInfo;

use super::device::{self, Kind};
use super::dma::Region;
use super::error::Error;
use super::hda_card::HdaCard;
use super::virtio_card::VirtioCard;

/// Transmit slots (periods in flight) a stream may use.
pub(super) const MAX_SLOTS: usize = 8;

#[derive(Clone, Copy)]
pub(super) enum StreamOp {
    Prepare,
    Start,
    Stop,
    Release,
}

/// Boxed: the two cards differ in size by kilobytes, and there is one per driver.
pub(super) enum Card {
    Virtio(Box<VirtioCard>),
    Hda(Box<HdaCard>),
}

/// Forward a call to whichever card this is.
macro_rules! each {
    ($self:ident, $card:ident => $call:expr) => {
        match $self {
            Card::Virtio($card) => $call,
            Card::Hda($card) => $call,
        }
    };
}

impl Card {
    /// Claim the card (`dev=<id>` from `devd`, else the first one) and go live.
    pub(super) fn open(wanted: Option<u64>) -> Result<Card, Error> {
        let (row, kind) = device::find(wanted)?;
        Ok(match kind {
            Kind::Virtio => Card::Virtio(Box::new(VirtioCard::open(row)?)),
            Kind::Hda => Card::Hda(Box::new(HdaCard::open(row)?)),
        })
    }

    /// What the card is, for the log.
    pub(super) fn model(&self) -> &'static str {
        match self {
            Card::Virtio(_) => "virtio-sound",
            Card::Hda(_) => "intel-hda",
        }
    }

    /// The `{card}` of its `system/audio/{card}/event` topic (issue #453).
    pub(super) fn event_name(&self) -> &'static str {
        match self {
            Card::Virtio(_) => user::audio_events::VIRTIO_CARD,
            Card::Hda(_) => user::audio_events::HDA_CARD,
        }
    }

    /// Streams the device reports.
    pub(super) fn streams(&self) -> u32 {
        match self {
            Card::Virtio(card) => card.streams,
            Card::Hda(_) => 1,
        }
    }

    pub(super) fn pcm_infos(&mut self) -> Result<Vec<PcmInfo>, Error> {
        each!(self, card => card.pcm_infos())
    }

    pub(super) fn take_staging(&mut self) -> Result<Region, Error> {
        each!(self, card => card.take_staging())
    }

    pub(super) fn give_back(&mut self, region: Region) {
        each!(self, card => card.give_back(region))
    }

    pub(super) fn set_params(
        &mut self,
        stream: u32,
        buffer_bytes: u32,
        period_bytes: u32,
        channels: u8,
        format: u8,
        rate: u8,
    ) -> Result<(), Error> {
        each!(self, card => card.set_params(stream, buffer_bytes, period_bytes, channels, format, rate))
    }

    pub(super) fn stream_op(&mut self, op: StreamOp, stream: u32) -> Result<(), Error> {
        each!(self, card => card.stream_op(op, stream))
    }

    pub(super) fn submit(
        &mut self,
        stream: u32,
        ring: &Region,
        slot: usize,
        period_bytes: usize,
        len: usize,
    ) -> Result<(), Error> {
        each!(self, card => card.submit(stream, ring, slot, period_bytes, len))
    }

    pub(super) fn reap(&mut self) -> Result<Option<(usize, bool)>, Error> {
        each!(self, card => card.reap())
    }

    /// Wait briefly for the device: an interrupt if the line is armed, at most
    /// one tick either way.
    pub(super) fn idle(&mut self) {
        each!(self, card => card.wait_event())
    }

    /// Service any interrupt message already queued, without waiting.
    pub(super) fn service_irq(&mut self) {
        each!(self, card => card.service_irq())
    }

    /// Whether the interrupt line is armed, and how many interrupts arrived.
    pub(super) fn irq_report(&self) -> (bool, u64) {
        each!(self, card => card.irq_report())
    }
}
