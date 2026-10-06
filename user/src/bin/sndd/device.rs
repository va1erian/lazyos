//! Find and claim the sound card: a modern virtio-sound function or an Intel
//! High Definition Audio controller, the first one in the device list, or
//! exactly the one `devd` named (`dev=<id>`).
//!
//! Both cards go through the same `dev_*` ops: claim with an interrupt
//! endpoint, the command register for decode and bus mastering, `map_bar`,
//! and `irq_enable` once the card is described. What differs lives in
//! `virtio_device.rs`/`virtio_card.rs` and `hda_card.rs`.

use user::dev::{self, Row};
use user::messenger::{self, Endpoint};
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
}

/// Claim `row` and switch on memory decode and bus mastering. Interrupts
/// arrive as kernel messages in the inbox of the channel side named at claim
/// time, and that same side is where the driver reads them; the other side
/// stays open (unused) so the channel never reports a dead peer. If the
/// kernel refuses the endpoint the claim is retried without one and the
/// driver polls, which is always correct, just a tick slower. `shared` opts
/// in to sharing the interrupt line.
pub(super) fn claim(row: Row, shared: bool) -> Result<Claimed, Error> {
    let (irq_side, _peer) =
        messenger::create_pair().map_err(|_| Error::Messenger("irq channel"))?;
    let (handle, pending) = match dev::claim(row.id, Some(irq_side.handle()), shared) {
        Ok(handle) => (handle, Some(irq_side)),
        Err(_) => (dev::claim(row.id, None, false).map_err(Error::Dev)?, None),
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
/// `ENOSYS` and the driver polls. The kernel keeps INTx disabled until the
/// claim is armed, so the command register is written again to let the card
/// assert it.
pub(super) fn arm(claimed: &mut Claimed) -> Result<(), Error> {
    claimed.irq = claimed
        .pending
        .take()
        .filter(|_| dev::irq_enable(claimed.handle).is_ok());
    if claimed.irq.is_some() {
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
