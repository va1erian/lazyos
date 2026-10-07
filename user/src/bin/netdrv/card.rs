//! The card: whichever NIC was claimed, its engine, interrupts and link state.
//!
//! Bring-up is per card (`virtio_card.rs`, `e1000_card.rs`); both hand back
//! the same [`Brought`]: the engine's rings over one DMA region allocated for
//! the driver's lifetime, the station address and the link. From then on the
//! engine (`libs/nicdrv`) moves the frames and this module only connects it
//! to the device: the doorbell, interrupt causes and the link.

use alloc::boxed::Box;

use nicdrv::{Engine, PumpOutcome};
use user::messenger::{Endpoint, Message};
use user::{dev, sys};
use virtio_net::settings::{IrqMode, Settings};
use virtio_net::ETH_HEADER;

use super::device::{self, Claimed, Kind};
use super::dma::Region;
use super::e1000_card;
use super::error::Error;
use super::rings::AnyRings;
use super::virtio_card::{self, Virtio};

/// Ticks (100 Hz) between reads of the link status when no interrupt says it
/// changed.
const LINK_POLL_TICKS: u64 = 50;

/// What a back end's bring-up hands the card.
pub(super) struct Brought {
    pub(super) rings: AnyRings,
    pub(super) region: Region,
    pub(super) mac: [u8; 6],
    pub(super) mtu: u16,
    pub(super) link: bool,
    pub(super) queue_sizes: (u16, u16),
}

/// The per-card state the engine does not hold.
enum Backend {
    Virtio(Virtio),
    E1000,
}

/// A doorbell for cards whose ring tails are written by the rings
/// themselves (the 8254x).
struct NoBell;

impl nicdrv::Doorbell for NoBell {
    fn ring(&mut self, _queue: u16) {}
}

pub(super) struct Card {
    pub(super) claimed: Claimed,
    pub(super) engine: Box<Engine<AnyRings>>,
    backend: Backend,
    /// Held for the driver's lifetime; never freed while the device runs.
    _region: Region,
    next_link_poll: u64,
    /// The ring sizes in use, which may differ from the settings.
    pub(super) queue_sizes: (u16, u16),
    pub(super) mtu: u16,
    /// What the card is, for the log (`virtio-net`, `e1000 82540EM`).
    pub(super) model: &'static str,
    /// The card name in the link topic (`system/net/{nic}/link`).
    pub(super) name: &'static str,
}

impl Card {
    /// Claim the card `row` (found by [`device::find`]), bring it up and go
    /// live. `server` is the service endpoint interrupts go to.
    pub(super) fn open(
        server: &Endpoint,
        settings: &Settings,
        row: user::dev::Row,
        kind: Kind,
    ) -> Result<Card, Error> {
        let mut claimed = device::claim(row, server, settings.irq_mode == IrqMode::Auto)?;
        let (backend, brought, model, name) = match kind {
            Kind::Virtio => {
                let (virtio, brought) = virtio_card::open(&claimed, settings)?;
                (
                    Backend::Virtio(virtio),
                    brought,
                    "virtio-net",
                    "virtio-net0",
                )
            }
            Kind::E1000(model) => {
                let brought = e1000_card::open(&claimed, settings)?;
                (Backend::E1000, brought, model, "e1000-0")
            }
        };
        device::arm(&mut claimed)?;
        if let (Some(user::dev::IrqMode::MsiX), Backend::Virtio(virtio)) = (claimed.mode, &backend)
        {
            virtio.use_msix()?;
        }
        if let Some(mode) = claimed.mode {
            sys::write_str(&alloc::format!("NETDRV:IRQ:{mode:?}\n"));
        }
        let engine = Box::new(Engine::new(
            brought.rings,
            brought.mac,
            usize::from(brought.mtu) + ETH_HEADER,
            brought.link,
        ));
        Ok(Card {
            claimed,
            engine,
            backend,
            _region: brought.region,
            next_link_poll: sys::clock() + LINK_POLL_TICKS,
            queue_sizes: brought.queue_sizes,
            mtu: brought.mtu,
            model,
            name,
        })
    }

    /// Do all pending work (see `Engine::pump`).
    pub(super) fn pump(&mut self) -> Result<PumpOutcome, Error> {
        Ok(match &self.backend {
            Backend::Virtio(virtio) => self.engine.pump(&mut virtio.bell())?,
            Backend::E1000 => self.engine.pump(&mut NoBell)?,
        })
    }

    /// Whether `message` is a genuine interrupt for this device: only the
    /// kernel (slot 0) may send one, and its body must name a device.
    pub(super) fn is_interrupt(message: &Message) -> bool {
        message.sender == 0 && dev::parse_irq_body(&message.parcel.body).is_some()
    }

    /// Acknowledge one interrupt. Reading the card's cause register deasserts
    /// the level interrupt before the kernel is told to unmask the line; a
    /// link change refreshes the link state.
    pub(super) fn handle_interrupt(&mut self) {
        self.engine.count_interrupt();
        let link_changed = match &self.backend {
            Backend::Virtio(virtio) => virtio.take_config_change(),
            Backend::E1000 => match self.engine.queues_mut() {
                AnyRings::E1000(rings) => {
                    e1000::setup::take_causes(rings.regs_mut()) & e1000::regs::int::LSC != 0
                }
                AnyRings::Virtio(_) => false,
            },
        };
        if link_changed {
            self.refresh_link();
        }
        let _ = dev::irq_ack(self.claimed.handle);
    }

    /// Re-read the link status from the device.
    fn refresh_link(&mut self) -> bool {
        let link = match &self.backend {
            Backend::Virtio(virtio) => virtio.link(),
            Backend::E1000 => match self.engine.queues() {
                AnyRings::E1000(rings) => Some(e1000::setup::link_up(rings.regs())),
                AnyRings::Virtio(_) => None,
            },
        };
        link.is_some_and(|up| self.engine.set_link(up))
    }

    /// Poll the link status when its time has come; returns whether it changed.
    pub(super) fn poll_link(&mut self, now: u64) -> bool {
        if now < self.next_link_poll {
            return false;
        }
        self.next_link_poll = now + LINK_POLL_TICKS;
        self.refresh_link()
    }

    /// Whether the interrupt line is armed.
    pub(super) fn irq_armed(&self) -> bool {
        self.claimed.irq
    }
}
