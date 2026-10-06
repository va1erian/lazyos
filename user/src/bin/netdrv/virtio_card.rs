//! The virtio-net back end: the modern transport located through the PCI
//! capabilities, `VERSION_1` required, `MAC` and `STATUS` wanted and nothing
//! else (no offloads, no merged buffers, no control queue), and one DMA block
//! for both virtqueues and every packet slot (`docs/networking-plan.md`
//! section 5).

use core::ptr;

use nicdrv::{Doorbell, Queues};
use user::dev;
use virtio::caps::{self, Location};
use virtio::transport::{Kick, Transport};
use virtio_net::config::{NetConfig, OFF_STATUS};
use virtio_net::settings::Settings;
use virtio_net::{features, queue as qi, MIN_MTU};

use super::card::Brought;
use super::device::{self, Claimed};
use super::dma::Region;
use super::error::Error;
use super::rings::AnyRings;

/// What the card keeps of the transport once it runs.
pub(super) struct Virtio {
    transport: Transport,
    rx_kick: Kick,
    tx_kick: Kick,
    status_feature: bool,
}

/// Rings the device through the transport.
pub(super) struct Bell<'a> {
    virtio: &'a Virtio,
}

impl Doorbell for Bell<'_> {
    fn ring(&mut self, queue: u16) {
        let kick = if queue == qi::RX {
            self.virtio.rx_kick
        } else {
            self.virtio.tx_kick
        };
        self.virtio.transport.notify(kick);
    }
}

impl Virtio {
    pub(super) fn bell(&self) -> Bell<'_> {
        Bell { virtio: self }
    }

    /// Read the interrupt status (which deasserts the level interrupt); true
    /// when the configuration (the link) changed.
    pub(super) fn take_config_change(&self) -> bool {
        self.transport.isr_status() & 2 != 0
    }

    /// The link status, if the device reports one.
    pub(super) fn link(&self) -> Option<bool> {
        if !self.status_feature {
            return None;
        }
        self.transport
            .device_config(OFF_STATUS, 2)
            .ok()
            .map(|status| status as u16 & virtio_net::config::S_LINK_UP != 0)
    }
}

/// Map `location`'s BAR (once) and return a pointer to the structure inside
/// it. The structure must lie fully inside a memory BAR of the row.
fn locate(
    claimed: &Claimed,
    bases: &mut [*mut u8; 6],
    location: Location,
) -> Result<*mut u8, Error> {
    let bar = usize::from(location.bar);
    let end = u64::from(location.end().ok_or(Error::Range)?);
    if bases.get(bar).ok_or(Error::Range)?.is_null() {
        bases[bar] = device::map(claimed, bar, end)?;
    } else if end > claimed.row.bar_len[bar] {
        return Err(Error::Range);
    }
    // SAFETY: `end <= bar_len` (checked by `device::map` or above), and the
    // kernel mapped the whole BAR.
    Ok(unsafe { bases[bar].add(location.offset as usize) })
}

/// Build the transport from the virtio capabilities in config space.
fn transport(claimed: &Claimed) -> Result<Transport, Error> {
    let handle = claimed.handle;
    let caps = caps::parse(|offset, width| {
        dev::cfg_read(handle, u64::from(offset), u64::from(width)).unwrap_or(0)
    })?;
    let mut bases = [ptr::null_mut::<u8>(); 6];
    let common = locate(claimed, &mut bases, caps.common)?;
    let notify = locate(claimed, &mut bases, caps.notify)?;
    let isr = locate(claimed, &mut bases, caps.isr)?;
    let (device, device_len) = match caps.device {
        Some(location) => (locate(claimed, &mut bases, location)?, location.length),
        None => (ptr::null_mut(), 0),
    };
    if caps.common.length < virtio::regs::common::LEN as u32 {
        return Err(Error::Range);
    }
    // SAFETY: every pointer was bounds-checked against its BAR above and the
    // mappings live until the claim is released with the task.
    Ok(unsafe {
        Transport::new(
            common,
            notify,
            caps.notify.length,
            caps.notify_multiplier,
            isr,
            device,
            device_len,
        )
    })
}

/// The largest power of two not above `value` (and at least 2).
fn floor_pow2(value: u16) -> u16 {
    if value < 2 {
        2
    } else {
        1 << value.ilog2()
    }
}

/// Negotiate, set the queues up over one DMA block and go live.
pub(super) fn open(claimed: &Claimed, settings: &Settings) -> Result<(Virtio, Brought), Error> {
    let transport = transport(claimed)?;
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
    let virtio = Virtio {
        transport,
        rx_kick,
        tx_kick,
        status_feature: negotiated & features::STATUS != 0,
    };
    let link = virtio.link().unwrap_or(config.link_up());
    Ok((
        virtio,
        Brought {
            rings: AnyRings::Virtio(queues),
            region,
            mac,
            mtu,
            link,
            queue_sizes: (rx_entries, tx_entries),
        },
    ))
}
