//! The Intel 8254x back end (issue #497): BAR 0 is the register file, one DMA
//! block holds both descriptor rings and every packet slot, and the ring tails
//! are the doorbell (`libs/e1000`). Nothing here needs a `dev_*` op the
//! virtio back end does not use.

use alloc::boxed::Box;

use e1000::regs::MIN_BAR_BYTES;
use e1000::{setup, Mmio, Rings};
use user::sys;
use virtio_net::settings::Settings;

use super::card::Brought;
use super::device::{self, Claimed};
use super::dma::Region;
use super::error::Error;
use super::rings::AnyRings;

/// The register BAR.
const BAR: usize = 0;
/// No MTU above the standard one: the card is not told to accept long
/// packets (`RCTL.LPE` stays clear), so it would drop them anyway.
const MAX_MTU: u16 = 1500;

/// A legal ring size near what the settings ask.
fn entries(wanted: u16) -> u16 {
    let clamped = wanted.clamp(e1000::rings::MIN_ENTRIES, e1000::rings::MAX_ENTRIES);
    1 << clamped.ilog2()
}

/// Reset the card, read its address, set the rings up and go live.
pub(super) fn open(claimed: &Claimed, settings: &Settings) -> Result<Brought, Error> {
    let base = device::map(claimed, BAR, MIN_BAR_BYTES)?;
    // SAFETY: `device::map` mapped the whole BAR, at least `MIN_BAR_BYTES`
    // long, and the mapping lives until the claim ends with the task.
    let mut regs = unsafe { Mmio::new(base, claimed.row.bar_len[BAR]) }.ok_or(Error::Range)?;
    setup::reset(&mut regs, sys::nap).map_err(Error::Setup)?;
    let mac = match settings.mac_override {
        Some(mac) => mac,
        None => setup::mac(&mut regs, sys::nap).map_err(Error::Setup)?,
    };
    let mtu = settings.mtu.min(MAX_MTU);
    let (rx_entries, tx_entries) = (entries(settings.rx_entries), entries(settings.tx_entries));
    let layout = e1000::Layout::new(rx_entries, tx_entries).ok_or(Error::Range)?;
    let region = Region::alloc(claimed.handle, layout.total)?;
    // SAFETY: the region is handed to exactly one `Rings`, which alone touches
    // it from here on, and `regs` is the card the region was allocated for.
    let mut rings = unsafe { Rings::new(regs, region.block(), rx_entries, tx_entries) }?;
    let link = setup::link_up(rings.regs());
    setup::enable_interrupts(rings.regs_mut());
    Ok(Brought {
        rings: AnyRings::E1000(Box::new(rings)),
        region,
        mac,
        mtu,
        link,
        queue_sizes: (rx_entries, tx_entries),
    })
}
