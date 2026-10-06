//! Find and claim the NIC: a virtio-net function or an Intel 8254x, the
//! first one in the device list, or exactly the one `devd` named (`dev=<id>`).
//!
//! Everything here is the same for both cards and uses only the `dev_*` ops
//! every driver uses: claim (with the service endpoint for interrupts, the line
//! shared), the command register for decode and bus mastering, `map_bar`, and
//! `irq_enable` once the card is described. What differs lives in
//! `virtio_card.rs` and `e1000_card.rs`.

use user::dev::{self, Row};
use user::messenger::Endpoint;
use virtio::regs::{pci_device_id, PCI_VENDOR};
use virtio_net::DEVICE_TYPE;

use super::error::Error;

/// PCI command register offset and the bits the driver sets.
const COMMAND: u64 = 4;
const COMMAND_MEMORY: u64 = 1 << 1;
const COMMAND_BUS_MASTER: u64 = 1 << 2;
const COMMAND_INTX_DISABLE: u64 = 1 << 10;

/// QEMU's default `virtio-net-pci` is the *transitional* function (legacy and
/// modern interfaces both present); with `disable-legacy=on` it is `1041`.
/// Either way the driver uses only the modern interface.
const TRANSITIONAL_DEVICE_ID: u16 = 0x1000;

/// Which card a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Virtio,
    /// An 8254x, with its model name.
    E1000(&'static str),
}

/// The card `row` is, if this driver drives it.
pub(super) fn kind(row: &Row) -> Option<Kind> {
    if row.flags & dev::row_flag::PCI == 0 {
        return None;
    }
    if row.vendor == PCI_VENDOR
        && (row.device == pci_device_id(DEVICE_TYPE) || row.device == TRANSITIONAL_DEVICE_ID)
    {
        return Some(Kind::Virtio);
    }
    e1000::model(row.vendor, row.device).map(Kind::E1000)
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

/// A claimed card: its handle, and whether interrupt messages will arrive on
/// the service endpoint (decided by [`arm`]).
pub(super) struct Claimed {
    pub(super) handle: u64,
    pub(super) row: Row,
    with_irq: bool,
    pub(super) irq: bool,
}

/// Claim `row`. `server` is the driver's own service endpoint: the kernel
/// posts interrupt messages into its inbox, so client calls and interrupts
/// arrive in the one receive loop. The line may be shared (with the polled
/// virtio-blk on both QEMU machine types), so the claim opts in to sharing.
/// If the kernel refuses the endpoint, or `allow_irq` is false, the claim is
/// made without one and the driver polls, which is always correct. Memory
/// decode and bus mastering are switched on.
pub(super) fn claim(row: Row, server: &Endpoint, allow_irq: bool) -> Result<Claimed, Error> {
    let with_irq = if allow_irq {
        dev::claim(row.id, Some(server.handle()), true).ok()
    } else {
        None
    };
    let (handle, with_irq) = match with_irq {
        Some(handle) => (handle, true),
        None => (dev::claim(row.id, None, false).map_err(Error::Dev)?, false),
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
        with_irq,
        irq: false,
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

/// Arm the line last, once the card is fully set up: an unroutable line
/// answers `ENOSYS` and the driver polls. The kernel keeps INTx disabled until
/// the claim is armed, so the command register is written again to let the
/// card assert it.
pub(super) fn arm(claimed: &mut Claimed) -> Result<(), Error> {
    claimed.irq = claimed.with_irq && dev::irq_enable(claimed.handle).is_ok();
    if claimed.irq {
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
