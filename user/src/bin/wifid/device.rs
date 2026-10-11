//! Find and claim the Wi-Fi card: the RTL8822CE `devd` named (`dev=<id>`),
//! or the first one in the device list when started by hand.
//!
//! Only the `dev_*` ops every driver uses: claim with interrupts on the
//! channel the kernel makes, the command register for memory decode and bus
//! mastering, `map_bar(2)`, and `irq_enable` (MSI: the card offers one
//! 64-bit-capable vector, docs/compat/amd-nuc). The kernel lets `_wifi`
//! claim the wireless class and nothing else (`netpolicy`).

use user::dev::{self, Row};
use user::messenger::{Endpoint, Message};

/// PCI configuration registers read or written here.
const COMMAND: u64 = 0x04;
const REVISION: u64 = 0x08;
const SUBSYSTEM: u64 = 0x2C;
const COMMAND_MEMORY: u64 = 1 << 1;
const COMMAND_BUS_MASTER: u64 = 1 << 2;
const COMMAND_INTX_DISABLE: u64 = 1 << 10;

/// The register BAR (upstream `rtw_pci_io_mapping`: BAR 2, 64 KiB).
pub(super) const BAR: usize = 2;
/// The smallest register BAR this driver maps: every MAC, BB and RF page
/// the chip has is inside the first 64 KiB.
pub(super) const MIN_BAR_BYTES: u64 = 0x1_0000;

/// Why a device step failed: the step name (`WIFID:FAIL:<step>`) and the
/// errno the kernel gave.
pub(super) type StepError = (&'static str, i64);

/// The model name of `row`, if this driver drives it.
pub(super) fn model(row: &Row) -> Option<&'static str> {
    if row.flags & dev::row_flag::PCI == 0 {
        return None;
    }
    rtw88::model(row.vendor, row.device)
}

/// The card to drive: device `wanted` when `devd` named one (it must be a
/// card this driver knows), else the first one in the list.
pub(super) fn find(wanted: Option<u64>) -> Result<Option<(Row, &'static str)>, i64> {
    let mut rows = [[0u64; dev::ROW_WORDS]; dev::MAX_ROWS];
    let total = dev::list(&mut rows)?;
    Ok(rows
        .iter()
        .take(total.min(rows.len()))
        .map(Row::from_words)
        .filter(|row| wanted.is_none_or(|id| row.id == id))
        .find_map(|row| model(&row).map(|model| (row, model))))
}

/// A claimed card.
pub(super) struct Claimed {
    pub(super) handle: u64,
    pub(super) row: Row,
    /// The channel the kernel posts interrupt messages to.
    pub(super) irq_channel: Option<Endpoint>,
    /// How interrupts arrive once [`arm`] succeeded.
    pub(super) mode: Option<dev::IrqMode>,
}

/// Claim `row` with an interrupt channel (sharing allowed: MSI is the card's
/// own vector, and a legacy line may be shared), falling back to a polled
/// claim, and switch on memory decode and bus mastering.
pub(super) fn claim(row: Row) -> Result<Claimed, StepError> {
    let (handle, irq_channel) = match dev::claim_with_irq(row.id, true) {
        Ok((handle, channel)) => (handle, Some(Endpoint::from_raw(channel))),
        Err(_) => (dev::claim(row.id).map_err(|e| ("claim", e))?, None),
    };
    let command = dev::cfg_read(handle, COMMAND, 2).map_err(|e| ("pci-command", e))?;
    dev::cfg_write(
        handle,
        COMMAND,
        2,
        u64::from(command) | COMMAND_MEMORY | COMMAND_BUS_MASTER,
    )
    .map_err(|e| ("pci-command", e))?;
    Ok(Claimed {
        handle,
        row,
        irq_channel,
        mode: None,
    })
}

/// `(revision, subsystem vendor, subsystem device)` from configuration space:
/// the subsystem names the module's maker, which decides its RFE type.
pub(super) fn identity(claimed: &Claimed) -> Result<(u8, u16, u16), i64> {
    let revision = dev::cfg_read(claimed.handle, REVISION, 1)? as u8;
    let subsystem = dev::cfg_read(claimed.handle, SUBSYSTEM, 4)?;
    Ok((revision, subsystem as u16, (subsystem >> 16) as u16))
}

/// Map BAR 2; it must be a memory BAR of at least [`MIN_BAR_BYTES`].
/// Returns the mapping and its length.
pub(super) fn map(claimed: &Claimed) -> Result<(*mut u8, u64), StepError> {
    let (meta, len) = (claimed.row.bar_meta[BAR], claimed.row.bar_len[BAR]);
    // Present (bit 0) and memory (bit 1 clear).
    if meta & 0b11 != 0b01 || len < MIN_BAR_BYTES {
        return Err(("map", dev::errno::ENODEV));
    }
    let base = dev::map_bar(claimed.handle, BAR as u64).map_err(|e| ("map", e))?;
    Ok((base, len))
}

/// Arm the interrupt last, once the card is described. `None`: polling only
/// (no channel, or `ENOSYS`: neither a line nor MSI). On INTx the command
/// register is written again so the card may assert it.
pub(super) fn arm(claimed: &mut Claimed) -> Result<Option<dev::IrqMode>, StepError> {
    claimed.mode = match claimed.irq_channel {
        Some(_) => dev::irq_enable(claimed.handle).ok(),
        None => None,
    };
    if claimed.mode == Some(dev::IrqMode::Intx) {
        let command = dev::cfg_read(claimed.handle, COMMAND, 2).map_err(|e| ("irq", e))?;
        dev::cfg_write(
            claimed.handle,
            COMMAND,
            2,
            u64::from(command) & !COMMAND_INTX_DISABLE,
        )
        .map_err(|e| ("irq", e))?;
    }
    Ok(claimed.mode)
}

/// Whether `message` is a kernel interrupt notification for the claim
/// (sent by slot 0, the kernel, and well formed).
pub(super) fn is_interrupt(message: &Message) -> bool {
    message.sender == 0 && dev::parse_irq_body(&message.parcel.body).is_some()
}

/// The text of an interrupt mode, for evidence lines.
pub(super) fn mode_name(mode: Option<dev::IrqMode>) -> &'static str {
    match mode {
        Some(dev::IrqMode::Intx) => "intx",
        Some(dev::IrqMode::Msi) => "msi",
        Some(dev::IrqMode::MsiX) => "msix",
        None => "polling",
    }
}
