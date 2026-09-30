//! The virtio-net card: bring-up, the doorbell, link state and interrupts.
//!
//! Bring-up follows `docs/networking-plan.md` section 5: claim the function,
//! modern transport only, `VERSION_1` required, `MAC` and `STATUS` wanted and
//! nothing else (no offloads, no merged buffers, no control queue). One DMA
//! allocation holds both queues and every packet slot for the driver's
//! lifetime. What happens to frames is the driver core's job
//! (`libs/nicdrv`); this module only connects it to the device.

use alloc::boxed::Box;

use nicdrv::{Doorbell, Engine, PumpOutcome, Queues};
use user::messenger::{Endpoint, Message};
use user::{dev, sys};
use virtio::transport::{Kick, Transport};
use virtio_net::config::{NetConfig, OFF_STATUS};
use virtio_net::settings::{IrqMode, Settings};
use virtio_net::{features, queue as qi, ETH_HEADER, MIN_MTU};

use super::device::{self, Claimed};
use super::dma::Region;
use super::error::Error;

/// Ticks (100 Hz) between reads of the link status when no interrupt says the
/// configuration changed.
const LINK_POLL_TICKS: u64 = 50;

pub(super) struct Card {
    pub(super) claimed: Claimed,
    pub(super) engine: Box<Engine>,
    /// Held for the driver's lifetime; never freed while the device runs.
    _region: Region,
    rx_kick: Kick,
    tx_kick: Kick,
    status_feature: bool,
    next_link_poll: u64,
    /// The queue sizes in use, which may be below the settings when the
    /// device offers less.
    pub(super) queue_sizes: (u16, u16),
    pub(super) mtu: u16,
}

/// Rings the device through the transport.
struct Bell<'a> {
    transport: &'a Transport,
    rx: Kick,
    tx: Kick,
}

impl Doorbell for Bell<'_> {
    fn ring(&mut self, queue: u16) {
        self.transport
            .notify(if queue == qi::RX { self.rx } else { self.tx });
    }
}

/// The largest power of two not above `value` (and at least 2).
fn floor_pow2(value: u16) -> u16 {
    if value < 2 {
        2
    } else {
        1 << value.ilog2()
    }
}

impl Card {
    /// Claim the device, negotiate features, set up the queues and go live.
    /// `server` is the service endpoint interrupts are delivered to.
    pub(super) fn open(server: &Endpoint, settings: &Settings) -> Result<Card, Error> {
        let claimed = device::open(server, settings.irq_mode == IrqMode::Auto)?;
        let transport = &claimed.transport;
        let negotiated = transport.negotiate(features::REQUIRED, features::WANTED)?;
        let config = NetConfig::read(negotiated, |offset, width| {
            transport.device_config(offset, width)
        })?;
        let mac = settings
            .mac_override
            .or(config.usable_mac())
            .ok_or(Error::NoMac)?;
        // The device's own MTU hint, when it gives one, caps the setting.
        let mtu = config.mtu.map_or(settings.mtu, |device_mtu| {
            settings.mtu.min(device_mtu.max(MIN_MTU))
        });

        // Queue sizes: what the settings ask, never more than the device offers.
        let rx_max = transport.queue_max(qi::RX)?;
        let tx_max = transport.queue_max(qi::TX)?;
        if rx_max < 2 || tx_max < 2 {
            return Err(Error::Virtio(virtio::Error::BadQueue));
        }
        let rx_entries = floor_pow2(settings.rx_entries.min(rx_max));
        let tx_entries = floor_pow2(settings.tx_entries.min(tx_max));

        let layout = nicdrv::queues::Layout::new(rx_entries, tx_entries).ok_or(Error::Range)?;
        let region = Region::alloc(claimed.handle, layout.total)?;
        // SAFETY: the region is handed to exactly one `Queues`, and only the
        // queues touch it from here on.
        let mut queues = unsafe { Queues::new(region.block(), rx_entries, tx_entries) }?;
        queues.post_all_rx()?;
        let rx_kick = transport.setup_queue(qi::RX, queues.rx_queue())?;
        let tx_kick = transport.setup_queue(qi::TX, queues.tx_queue())?;
        transport.driver_ok()?;
        // Tell the device the receive buffers are there.
        transport.notify(rx_kick);

        let engine = Box::new(Engine::new(
            queues,
            mac,
            usize::from(mtu) + ETH_HEADER,
            config.link_up(),
        ));
        Ok(Card {
            claimed,
            engine,
            _region: region,
            rx_kick,
            tx_kick,
            status_feature: negotiated & features::STATUS != 0,
            next_link_poll: sys::clock() + LINK_POLL_TICKS,
            queue_sizes: (rx_entries, tx_entries),
            mtu,
        })
    }

    /// Do all pending work (see `Engine::pump`).
    pub(super) fn pump(&mut self) -> Result<PumpOutcome, Error> {
        let mut bell = Bell {
            transport: &self.claimed.transport,
            rx: self.rx_kick,
            tx: self.tx_kick,
        };
        Ok(self.engine.pump(&mut bell)?)
    }

    /// Whether `message` is a genuine interrupt for this device: only the
    /// kernel (slot 0) may send one, and its body must name a device.
    pub(super) fn is_interrupt(message: &Message) -> bool {
        message.sender == 0 && dev::parse_irq_body(&message.parcel.body).is_some()
    }

    /// Acknowledge one interrupt. Reading the ISR status deasserts the level
    /// interrupt before the kernel is told to unmask the line; a set
    /// configuration-change bit refreshes the link state.
    pub(super) fn handle_interrupt(&mut self) {
        self.engine.count_interrupt();
        let isr = self.claimed.transport.isr_status();
        if isr & 2 != 0 {
            self.refresh_link();
        }
        let _ = dev::irq_ack(self.claimed.handle);
    }

    /// Re-read the link status from the device configuration.
    fn refresh_link(&mut self) -> bool {
        if !self.status_feature {
            return false;
        }
        match self.claimed.transport.device_config(OFF_STATUS, 2) {
            Ok(status) => self
                .engine
                .set_link(status as u16 & virtio_net::config::S_LINK_UP != 0),
            Err(_) => false,
        }
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
