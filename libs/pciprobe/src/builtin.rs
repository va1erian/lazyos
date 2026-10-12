//! The ranges every build allows with `diag.dbg.mmio=1`: devices whose
//! registers were checked, as `(vendor, device, bar, start, end)` with `end`
//! exclusive.
//!
//! An MMIO read is only ever answered from inside one of these (or a range
//! the box's own `lazyos.cfg` adds, [`crate::Policy`]). That is an
//! allowlist on purpose: a register that clears or advances on read (a FIFO
//! data port, a status bit that acknowledges, an index register that
//! auto-increments) is a hole between the ranges, so a new register is
//! refused until somebody has read what it does. Holes carry a comment.
//!
//! Nothing here is a driver: it says which bytes may be *looked at*, and
//! every range is read-only for `dbgd` regardless.

use crate::Range;

const fn r(vendor: u16, device: u16, bar: u8, start: u64, end: u64) -> Range {
    Range {
        vendor,
        device,
        bar,
        start,
        end,
    }
}

/// QEMU's standard VGA (`-vga std`, Bochs VBE): the test target.
///
/// BAR2 is the 4 KiB register window. `0x000..0x400` is the EDID blob and
/// `0x500..0x520` the VBE dispi registers (the id at 0x500 reads 0xB0C0
/// to 0xB0C5). The VGA I/O ports mapped at `0x400..0x420` stay out: reading
/// input status 1 at `0x41a` resets the attribute controller's flip-flop.
const STD_VGA: u16 = 0x1111;
const QEMU: u16 = 0x1234;
// BAR0 (the 16 MiB framebuffer) is deliberately not listed: the display
// path maps it write-combining, and an uncached alias of the same frames
// is a memory-type alias the SDM calls unsupported (docs/dbgd-plan.md).

/// Intel HD Graphics 620 (Kaby Lake GT2, `8086:5916`), BAR0 (GTTMMADR):
/// registers in the low 8 MiB, the global GTT above.
///
/// Offsets are the Gen9 register map as `i915` names it; each block was
/// chosen as plain, idempotent reads (to confirm against a Linux dump, which
/// is what stage G0 does). The holes are deliberate:
///
/// * `0xC5100..0xC5130`: PCH GMBUS (the data register, `GMBUS3`, pops a
///   byte from the I2C FIFO when read);
/// * `0x65050`, `0x65150` and `0x65250` (8 bytes each): `HSW_AUD_EDID_DATA`
///   of transcoders A, B and C, the ELD read port (an indexed register that
///   advances on each read).
///
/// The pipe B/C, transcoder and audio blocks sit behind display power wells:
/// read `HSW_PWR_WELL_CTL` (`0x45400`) first, because a gated well may not
/// return cleanly.
const INTEL: u16 = 0x8086;
const HD620: u16 = 0x5916;

/// The table, sorted by `(vendor, device, bar, start)` and non-overlapping
/// (the tests check both).
pub const RANGES: &[Range] = &[
    r(QEMU, STD_VGA, 2, 0x0000_0000, 0x0000_0400),
    r(QEMU, STD_VGA, 2, 0x0000_0500, 0x0000_0520),
    // Forcewake acknowledge bits.
    r(INTEL, HD620, 0, 0x0000_0D00, 0x0000_0E00),
    // Render ring and engine state.
    r(INTEL, HD620, 0, 0x0000_2000, 0x0000_3000),
    // Forcewake request, RC6 and RPS control and status.
    r(INTEL, HD620, 0, 0x0000_A000, 0x0000_B000),
    // Video, video enhancement and blitter rings.
    r(INTEL, HD620, 0, 0x0001_2000, 0x0001_3000),
    r(INTEL, HD620, 0, 0x0001_A000, 0x0001_B000),
    r(INTEL, HD620, 0, 0x0002_2000, 0x0002_3000),
    // Display engine: fuses and straps, interrupt registers, power wells,
    // DBUF and CDCLK.
    r(INTEL, HD620, 0, 0x0004_2000, 0x0004_3000),
    r(INTEL, HD620, 0, 0x0004_4000, 0x0004_7000),
    // Transcoder timings, DDI buffers, AUX channel, DPLLs, pipe CRC, audio
    // configuration (less the ELD port), pipes and planes.
    r(INTEL, HD620, 0, 0x0006_0000, 0x0006_5050),
    r(INTEL, HD620, 0, 0x0006_5058, 0x0006_5150),
    r(INTEL, HD620, 0, 0x0006_5158, 0x0006_5250),
    r(INTEL, HD620, 0, 0x0006_5258, 0x0008_0000),
    // PCH display: south interrupt registers, clock gating, LCPLL (less
    // GMBUS).
    r(INTEL, HD620, 0, 0x000C_0000, 0x000C_5100),
    r(INTEL, HD620, 0, 0x000C_5130, 0x000D_0000),
    // GT FIFO debug and the thread and core status registers.
    r(INTEL, HD620, 0, 0x0012_0000, 0x0012_1000),
    r(INTEL, HD620, 0, 0x0013_8000, 0x0013_9000),
    // The global GTT (page-table entries; what the firmware mapped).
    r(INTEL, HD620, 0, 0x0080_0000, 0x0100_0000),
];
