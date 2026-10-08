//! Find and claim the sound card: a modern virtio-sound function or an Intel
//! High Definition Audio controller, the first one in the device list, or
//! exactly the one `devd` named (`dev=<id>`).
//!
//! Both cards go through the same `dev_*` ops: claim with an interrupt
//! channel, the command register for decode and bus mastering, `map_bar`,
//! and `irq_enable` once the card is described. What differs lives in
//! `virtio_device.rs`/`virtio_card.rs` and `hda_card.rs`.

use user::dev::{self, Row};
use user::messenger::Endpoint;
use user::sys;
use virtio::regs::{pci_device_id, PCI_VENDOR};
use virtio_snd::DEVICE_TYPE;

use super::error::Error;

/// PCI command register offset and the bits the driver sets.
const COMMAND: u64 = 4;
const COMMAND_MEMORY: u64 = 1 << 1;
const COMMAND_BUS_MASTER: u64 = 1 << 2;
const COMMAND_INTX_DISABLE: u64 = 1 << 10;

/// Which card a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Virtio,
    Hda,
}

/// The card `row` is, if this driver drives it.
pub(super) fn kind(row: &Row) -> Option<Kind> {
    if row.flags & dev::row_flag::PCI == 0 {
        return None;
    }
    if row.vendor == PCI_VENDOR && row.device == pci_device_id(DEVICE_TYPE) {
        return Some(Kind::Virtio);
    }
    hda::is_controller(row.class, row.subclass).then_some(Kind::Hda)
}

/// The card to drive: device `wanted` when `devd` named one (it must be a
/// card this driver knows), else the first one in the list.
pub(super) fn find(wanted: Option<u64>) -> Result<(Row, Kind), Error> {
    let mut rows = [[0u64; dev::ROW_WORDS]; dev::MAX_ROWS];
    let total = dev::list(&mut rows).map_err(Error::Dev)?;
    rows.iter()
        .take(total.min(rows.len()))
        .map(Row::from_words)
        .filter(|row| wanted.is_none_or(|id| row.id == id))
        .find_map(|row| kind(&row).map(|kind| (row, kind)))
        .ok_or(Error::NoDevice)
}

/// A claimed card.
pub(super) struct Claimed {
    pub(super) handle: u64,
    pub(super) row: Row,
    /// The channel side the claim named for interrupts, until [`arm`] knows
    /// whether the line can be delivered.
    pending: Option<Endpoint>,
    /// Where the kernel delivers this card's interrupt messages, when the line
    /// is routable and armed; `None` means the driver polls alone.
    pub(super) irq: Option<Endpoint>,
    /// How interrupts arrive once [`arm`] succeeded.
    pub(super) mode: Option<dev::IrqMode>,
}

/// Claim `row` and switch on memory decode and bus mastering. Interrupts
/// arrive as kernel messages on the channel the kernel makes for the claim
/// (issue #496), where the driver reads them. If the kernel refuses it the
/// claim is retried without one and the driver polls, which is always
/// correct, just a tick slower. `shared` opts in to sharing the interrupt
/// line.
pub(super) fn claim(row: Row, shared: bool) -> Result<Claimed, Error> {
    let (handle, pending) = match dev::claim_with_irq(row.id, shared) {
        Ok((handle, channel)) => (handle, Some(Endpoint::from_raw(channel))),
        Err(_) => (dev::claim(row.id).map_err(Error::Dev)?, None),
    };
    let command = dev::cfg_read(handle, COMMAND, 2).map_err(Error::Dev)?;
    dev::cfg_write(
        handle,
        COMMAND,
        2,
        u64::from(command) | COMMAND_MEMORY | COMMAND_BUS_MASTER,
    )
    .map_err(Error::Dev)?;
    Ok(Claimed {
        handle,
        row,
        pending,
        irq: None,
        mode: None,
    })
}

/// Map memory BAR `bar` of the claimed card; it must be at least `min` bytes.
pub(super) fn map(claimed: &Claimed, bar: usize, min: u64) -> Result<*mut u8, Error> {
    // Present (bit 0) and memory (bit 1 clear), and big enough.
    if bar >= 6 || claimed.row.bar_meta[bar] & 0b11 != 0b01 || claimed.row.bar_len[bar] < min {
        return Err(Error::Range);
    }
    dev::map_bar(claimed.handle, bar as u64).map_err(Error::Dev)
}

/// Arm the line once the card is described: an unroutable line answers
/// `ENOSYS` and the driver polls. On INTx the kernel keeps INTx disabled
/// until the claim is armed, so the command register is written again to let
/// the card assert it; on MSI or MSI-X the kernel programmed the message.
pub(super) fn arm(claimed: &mut Claimed) -> Result<(), Error> {
    let pending = claimed.pending.take();
    claimed.mode = pending
        .as_ref()
        .and_then(|_| dev::irq_enable(claimed.handle).ok());
    claimed.irq = pending.filter(|_| claimed.mode.is_some());
    if let Some(mode) = claimed.mode {
        sys::write_str(&alloc::format!("SNDD:IRQ:{mode:?}\n"));
    }
    if claimed.mode == Some(dev::IrqMode::Intx) {
        let command = dev::cfg_read(claimed.handle, COMMAND, 2).map_err(Error::Dev)?;
        dev::cfg_write(
            claimed.handle,
            COMMAND,
            2,
            u64::from(command) & !COMMAND_INTX_DISABLE,
        )
        .map_err(Error::Dev)?;
    }
    Ok(())
}
