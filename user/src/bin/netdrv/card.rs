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
use super::rtl8168_card;
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
    Rtl8168(rtl8168_card::State),
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
}

impl Card {
    /// Claim the card `row` (found by [`device::find`]), bring it up and go
    /// live.
    pub(super) fn open(
        settings: &Settings,
        row: user::dev::Row,
        kind: Kind,
    ) -> Result<Card, Error> {
        let mut claimed = device::claim(row, settings.irq_mode == IrqMode::Auto)?;
        let (backend, brought, model) = match kind {
            Kind::Virtio => {
                let (virtio, brought) = virtio_card::open(&claimed, settings)?;
                (Backend::Virtio(virtio), brought, "virtio-net")
            }
            Kind::E1000(model) => {
                let brought = e1000_card::open(&claimed, settings)?;
                (Backend::E1000, brought, model)
            }
            Kind::Rtl8168(model) => {
                let (brought, state) = rtl8168_card::open(&claimed, settings)?;
                (Backend::Rtl8168(state), brought, model)
            }
        };
        device::arm(&mut claimed)?;
        // The Realtek chip's causes are unmasked only now that the line is
        // armed, and only if it is: when polling, nothing would acknowledge
        // a cause, which would hold a shared INTx line asserted.
        let mut brought = brought;
        if let (true, Backend::Rtl8168(_), AnyRings::Rtl8168(rings)) =
            (claimed.irq, &backend, &mut brought.rings)
        {
            rtl8168_card::enable(rings);
        }
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
        })
    }

    /// Do all pending work (see `Engine::pump`).
    pub(super) fn pump(&mut self) -> Result<PumpOutcome, Error> {
        Ok(match &mut self.backend {
            Backend::Virtio(virtio) => self.engine.pump(&mut virtio.bell())?,
            Backend::E1000 => self.engine.pump(&mut NoBell)?,
            Backend::Rtl8168(state) => {
                // A fault noticed in the interrupt handler (a gone device, a
                // PCI error) and a stuck transmit queue end the driver; the
                // restart's soft reset is the recovery.
                if let AnyRings::Rtl8168(rings) = self.engine.queues() {
                    state.check(rings, self.engine.link())?;
                }
                self.engine.pump(&mut NoBell)?
            }
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
        let link_changed = match &mut self.backend {
            Backend::Virtio(virtio) => virtio.take_config_change(),
            Backend::E1000 => match self.engine.queues_mut() {
                AnyRings::E1000(rings) => {
                    e1000::setup::take_causes(rings.regs_mut()) & e1000::regs::int::LSC != 0
                }
                AnyRings::Virtio(_) | AnyRings::Rtl8168(_) => false,
            },
            Backend::Rtl8168(state) => match self.engine.queues_mut() {
                AnyRings::Rtl8168(rings) => {
                    state.note_causes(rtl8168::setup::take_causes(rings.regs_mut()))
                }
                _ => false,
            },
        };
        if link_changed {
            self.refresh_link();
        }
        let _ = dev::irq_ack(self.claimed.handle);
    }

    /// Re-read the link status from the device.
    fn refresh_link(&mut self) -> bool {
        let link = match &mut self.backend {
            Backend::Virtio(virtio) => virtio.link(),
            Backend::E1000 => match self.engine.queues() {
                AnyRings::E1000(rings) => Some(e1000::setup::link_up(rings.regs())),
                AnyRings::Virtio(_) | AnyRings::Rtl8168(_) => None,
            },
            Backend::Rtl8168(state) => match self.engine.queues() {
                AnyRings::Rtl8168(rings) => {
                    let link = rtl8168::setup::link(rings.regs());
                    if link.is_none() {
                        state.gone();
                    }
                    link.map(|link| link.up)
                }
                _ => None,
            },
        };
        let changed = link.is_some_and(|up| self.engine.set_link(up));
        if changed {
            // A change of link on this chip is where a cold start differs from
            // Linux's: keep its registers on the log (rtl8168 plan section 5).
            if let (Backend::Rtl8168(_), AnyRings::Rtl8168(rings)) =
                (&self.backend, self.engine.queues())
            {
                rtl8168_card::report_link("link-change", rings.regs());
            }
        }
        changed
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

    /// The channel interrupt messages arrive on, while the line is armed.
    pub(super) fn irq_channel(&self) -> Option<Endpoint> {
        self.claimed.irq_channel.filter(|_| self.claimed.irq)
    }

    /// Handle every interrupt message already queued, without waiting.
    pub(super) fn drain_interrupts(&mut self) {
        let Some(channel) = self.irq_channel() else {
            return;
        };
        let mut buffer = [0u8; 256];
        while let Ok(Some(message)) = channel.poll_recv_with(&mut buffer) {
            if Card::is_interrupt(&message) {
                self.handle_interrupt();
            }
        }
    }
}

impl Drop for Card {
    /// No DMA may run after the driver's memory is gone: stop the Realtek
    /// chip before `_region` is released (fields drop after this body).
    fn drop(&mut self) {
        if let (Backend::Rtl8168(_), AnyRings::Rtl8168(rings)) =
            (&self.backend, self.engine.queues_mut())
        {
            rtl8168_card::shutdown(rings);
        }
    }
}
