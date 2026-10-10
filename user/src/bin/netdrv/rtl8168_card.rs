//! The Realtek RTL8111H / RTL8168H back end (docs/rtl8168-driver-plan.md):
//! BAR 2 is the register file, one DMA block holds both descriptor rings and
//! every packet slot, and the transmit doorbell is written by the rings
//! themselves (`libs/rtl8168`). Nothing here needs a `dev_*` op the other
//! back ends do not use.
//!
//! The chip has no QEMU model, so the bring-up reports what it found on the
//! serial log: `NETDRV:RTL8168` (revision, PHY id, link) and `NETDRV:REGS`
//! (the first 256 bytes of registers, to compare with Linux's dump of the
//! same chip, `tools/net/rtl8168/`).

use alloc::boxed::Box;
use alloc::format;

use rtl8168::regs::{int, DUMP_BYTES, MIN_BAR_BYTES};
use rtl8168::{phy, phy_541, setup, Mmio, Regs, Rings, SetupError, TxWatchdog};
use user::sys;
use virtio_net::settings::Settings;

use super::card::Brought;
use super::device::{self, Claimed};
use super::dma::Region;
use super::error::Error;
use super::rings::AnyRings;

/// The register BAR (BAR 0 is I/O space, BAR 4 the MSI-X table).
const BAR: usize = 2;
/// No MTU above the standard one: the chip is told to receive 1528-byte
/// frames at most, and v1 has no jumbo frames.
const MAX_MTU: u16 = 1500;

/// What the driver tracks beyond the rings: the transmit watchdog and a
/// fault noticed where an error cannot be returned (the interrupt handler),
/// raised by the next [`State::check`].
pub(super) struct State {
    watchdog: TxWatchdog,
    fault: Option<&'static str>,
}

impl State {
    fn new() -> State {
        State {
            watchdog: TxWatchdog::new(sys::clock()),
            fault: None,
        }
    }

    /// Record the interrupt causes just read; whether the link changed.
    pub(super) fn note_causes(&mut self, causes: u16) -> bool {
        if causes == u16::MAX {
            self.fault = Some("device gone (all-ones interrupt status)");
        } else if causes & int::SYS_ERR != 0 {
            self.fault = Some("PCI system error");
        }
        causes != u16::MAX && causes & int::LINK_CHANGE != 0
    }

    /// The device is gone: its link register reads all ones.
    pub(super) fn gone(&mut self) {
        self.fault = Some("device gone (all-ones link status)");
    }

    /// Raise a recorded fault, then judge the transmit queue.
    pub(super) fn check(&mut self, rings: &Rings<Mmio>, link: bool) -> Result<(), Error> {
        if let Some(why) = self.fault {
            return Err(Error::Fatal(nicdrv::Fatal::Hardware(why)));
        }
        self.watchdog
            .check(
                sys::clock(),
                rings.tx_in_flight(),
                rings.tx_reaped_total(),
                link,
            )
            .map_err(Error::Fatal)
    }
}

/// A legal ring size near what the settings ask.
fn entries(wanted: u16) -> u16 {
    let clamped = wanted.clamp(rtl8168::rings::MIN_ENTRIES, rtl8168::rings::MAX_ENTRIES);
    1 << clamped.ilog2()
}

fn setup_error(error: SetupError) -> Error {
    match error {
        SetupError::Unsupported { xid } => Error::Unsupported(format!(
            "RTL8168 family revision XID {xid:#05x}; only XID 0x541 (RTL8111H) is supported"
        )),
        other => Error::Rtl(other),
    }
}

/// Print the first 256 register bytes, 32 to a line, for `label`.
pub(super) fn dump_regs(label: &str, regs: &impl Regs) {
    let mut bytes = [0u8; DUMP_BYTES];
    setup::dump(regs, &mut bytes);
    for (line, chunk) in bytes.chunks(32).enumerate() {
        let mut text = format!("NETDRV:REGS {label} {:02x}:", line * 32);
        for byte in chunk {
            text.push_str(&format!(" {byte:02x}"));
        }
        text.push('\n');
        sys::write_str(&text);
    }
}

/// Report the link the chip sees, with the register dump for `label`.
pub(super) fn report_link(label: &str, regs: &impl Regs) {
    match setup::link(regs) {
        Some(link) => sys::write_str(&format!(
            "NETDRV:RTL8168:LINK {label} up={} mbps={} full_duplex={}\n",
            link.up, link.mbps, link.full_duplex
        )),
        None => sys::write_str(&format!("NETDRV:RTL8168:LINK {label} unreadable\n")),
    }
    dump_regs(label, regs);
}

/// Identify the chip, reset it, read its address, start autonegotiation, set
/// the rings up and go live. Interrupts stay masked until [`enable`] (after
/// the line is armed).
pub(super) fn open(claimed: &Claimed, settings: &Settings) -> Result<(Brought, State), Error> {
    let base = device::map(claimed, BAR, MIN_BAR_BYTES)?;
    // SAFETY: `device::map` mapped the whole BAR, at least `MIN_BAR_BYTES`
    // long, and the mapping lives until the claim ends with the task.
    let mut regs = unsafe { Mmio::new(base, claimed.row.bar_len[BAR]) }.ok_or(Error::Range)?;
    let xid = setup::identify(&regs).map_err(setup_error)?;
    setup::reset(&mut regs, sys::nap).map_err(Error::Rtl)?;
    let mac = match settings.mac_override {
        Some(mac) => mac,
        None => setup::mac(&regs).map_err(Error::Rtl)?,
    };
    let mtu = settings.mtu.min(MAX_MTU);
    let (rx_entries, tx_entries) = (entries(settings.rx_entries), entries(settings.tx_entries));
    let layout = rtl8168::Layout::new(rx_entries, tx_entries).ok_or(Error::Range)?;
    let region = Region::alloc(claimed.handle, layout.total)?;
    // SAFETY: the region is handed to exactly one `Rings`, which alone touches
    // it from here on, and `regs` is the chip the region was allocated for.
    let mut rings = unsafe { Rings::new(regs, region.block(), rx_entries, tx_entries) }?;
    let phy_id = phy::start_autoneg(rings.regs_mut(), sys::nap).map_err(Error::Rtl)?;
    phy_541::apply(rings.regs_mut(), sys::nap).map_err(Error::Rtl)?;
    sys::write_str(&format!(
        "NETDRV:RTL8168 xid={xid:#05x} phy_id={phy_id:#010x}\n"
    ));
    report_link("open", rings.regs());
    let link = setup::link(rings.regs()).is_some_and(|link| link.up);
    let brought = Brought {
        rings: AnyRings::Rtl8168(Box::new(rings)),
        region,
        mac,
        mtu,
        link,
        queue_sizes: (rx_entries, tx_entries),
    };
    Ok((brought, State::new()))
}

/// Unmask the interrupt causes, once the line is armed.
pub(super) fn enable(rings: &mut Rings<Mmio>) {
    setup::enable_interrupts(rings.regs_mut());
}

/// Stop the chip's DMA before the driver's memory goes away.
pub(super) fn shutdown(rings: &mut Rings<Mmio>) {
    let _ = setup::shutdown(rings.regs_mut(), sys::nap);
}
