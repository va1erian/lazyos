//! Find and claim the GPU: the HD Graphics 620 `devd` named (`dev=<id>`), or
//! the first one in the device list when started by hand.
//!
//! Only the `dev_*` ops every driver uses: claim *without* interrupts (this
//! stage takes none), the command register, `cfg_read` of the identity words
//! and `map_bar(0)`. The kernel lets `_gpu` claim the display class and
//! nothing else (`gpupolicy`).
//!
//! **A claim switches the function's decode and bus mastering off**
//! (`dev::syscall::quiesce`, for every driver: a freshly claimed function
//! must be silent until its driver says otherwise). For a GPU that is also
//! the framebuffer the kernel console draws into (BAR 2 is the aperture), so
//! [`claim`] switches I/O and memory decode back on at once and then bus
//! mastering (which needs the `dma` right, `gpupolicy`): the state Linux
//! shows (`I/O+ Mem+ BusMaster+`, `00-lspci-gpu.txt`) and what the firmware
//! is believed to have left. The firmware's own command word is not
//! observable after the claim; `dbgctl pci-config 00:02.0` on a box without
//! `gpud` shows it. The same quiesce runs when `gpud` dies, which stops the
//! console's writes into the aperture until `init` starts it again: `gpud`
//! therefore never exits while it holds the claim on purpose (`park`).

use user::dev::{self, Row};

/// PCI configuration registers read or written here.
const COMMAND: u64 = 0x04;
const REVISION: u64 = 0x08;
const SUBSYSTEM: u64 = 0x2C;
/// `GMCH_CTRL` (graphics mode select, GTT size), `BDSM` (stolen memory
/// base) and `ASLS` (the OpRegion address): the words the survey reads.
const GMCH_CTRL: u64 = 0x50;
const BDSM: u64 = 0x5C;
const ASLS: u64 = 0xFC;
const COMMAND_IO: u64 = 1 << 0;
const COMMAND_MEMORY: u64 = 1 << 1;
const COMMAND_BUS_MASTER: u64 = 1 << 2;

/// The part this driver adopts.
pub(super) const VENDOR: u16 = 0x8086;
pub(super) const DEVICE: u16 = devmatch::GEN9GFX_DEVICES[0];
/// PCI base class of a display controller.
const CLASS_DISPLAY: u8 = 0x03;
/// The register BAR (`GTTMMADR`): registers in the lower half, the GGTT in
/// the upper.
pub(super) const BAR: usize = 0;
/// The smallest BAR that holds both (the box's is 16 MiB).
pub(super) const MIN_BAR_BYTES: u64 = 0x100_0000;

/// Why a device step failed: the step name and the errno the kernel gave.
pub(super) type StepError = (&'static str, i64);

/// Whether `row` is the GPU this driver adopts.
pub(super) fn is_gpu(row: &Row) -> bool {
    row.flags & dev::row_flag::PCI != 0
        && row.class == CLASS_DISPLAY
        && row.vendor == VENDOR
        && row.device == DEVICE
}

/// The GPU to drive: device `wanted` when `devd` named one (it must be the
/// GPU), else the first one in the list.
pub(super) fn find(wanted: Option<u64>) -> Result<Option<Row>, i64> {
    let mut rows = [[0u64; dev::ROW_WORDS]; dev::MAX_ROWS];
    let total = dev::list(&mut rows)?;
    Ok(rows
        .iter()
        .take(total.min(rows.len()))
        .map(Row::from_words)
        .filter(|row| wanted.is_none_or(|id| row.id == id))
        .find(is_gpu))
}

/// A claimed GPU.
pub(super) struct Claimed {
    pub(super) handle: u64,
    pub(super) row: Row,
}

/// What `claim` found in the command register, for the evidence line.
pub(super) struct Command {
    /// Right after the claim (the kernel's quiesce already ran).
    pub(super) after_claim: u32,
    /// After decode and bus mastering were switched back on.
    pub(super) restored: u32,
    /// Bus mastering could not be switched back on.
    pub(super) bus_master_error: Option<i64>,
}

/// Claim `row` (polled, no interrupt) and restore memory decode and bus
/// mastering, see the module docs.
pub(super) fn claim(row: Row) -> Result<(Claimed, Command), StepError> {
    let handle = dev::claim(row.id).map_err(|e| ("claim", e))?;
    let claimed = Claimed { handle, row };
    let after_claim = dev::cfg_read(handle, COMMAND, 2).map_err(|e| ("pci-command", e))?;
    // Decode first (no DMA right needed): the aperture must answer again.
    let decode = u64::from(after_claim) | COMMAND_IO | COMMAND_MEMORY;
    dev::cfg_write(handle, COMMAND, 2, decode).map_err(|e| ("pci-command", e))?;
    let bus_master_error = dev::cfg_write(handle, COMMAND, 2, decode | COMMAND_BUS_MASTER).err();
    let restored = dev::cfg_read(handle, COMMAND, 2).map_err(|e| ("pci-command", e))?;
    Ok((
        claimed,
        Command {
            after_claim,
            restored,
            bus_master_error,
        },
    ))
}

/// The identity words of the function (`GPU:PCI`).
pub(super) struct Identity {
    pub(super) revision: u8,
    pub(super) subsystem_vendor: u16,
    pub(super) subsystem_device: u16,
    pub(super) gmch_ctrl: u16,
    pub(super) bdsm: u32,
    pub(super) asls: u32,
}

pub(super) fn identity(claimed: &Claimed) -> Result<Identity, StepError> {
    let word = |offset, width| {
        dev::cfg_read(claimed.handle, offset, width).map_err(|e| ("pci-config", e))
    };
    let subsystem = word(SUBSYSTEM, 4)?;
    Ok(Identity {
        revision: word(REVISION, 1)? as u8,
        subsystem_vendor: subsystem as u16,
        subsystem_device: (subsystem >> 16) as u16,
        gmch_ctrl: word(GMCH_CTRL, 2)? as u16,
        bdsm: word(BDSM, 4)?,
        asls: word(ASLS, 4)?,
    })
}

/// Map BAR 0; it must be a memory BAR of at least [`MIN_BAR_BYTES`].
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
