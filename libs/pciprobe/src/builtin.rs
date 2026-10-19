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

/// Intel UHD Graphics, Alder Lake-UP3 GT1 (`8086:46b3`, display version 13:
/// the CHUWI LarkBox X of `docs/compat/alderlake/`), BAR0 (GTTMMADR, 16 MiB):
/// the display registers only, and the global GTT at 8 MiB.
///
/// Offsets are the display 13 map as `i915` names it (its HDMI output is
/// pipe A, DDI B on combo PHY B, DPLL0 under Linux). Nothing of the GT is
/// listed: display reads need no forcewake. The holes are deliberate:
///
/// * `0xC5100..0xC5130`: PCH GMBUS, as on the HD 620;
/// * `0x65050`, `0x65150`, `0x65250` and `0x65350` (8 bytes each):
///   `HSW_AUD_EDID_DATA` of transcoders A to D, the ELD read port;
/// * `0x49000..0x4B000`: pipe CSC and gamma, whose `PREC_PAL_DATA` advances
///   its index register when read;
/// * each scaler's coefficient index/data pairs (`PS_COEF_SET0/1`, scaler
///   base + `0x198..0x1A8`: `0x68198` for scaler 1A, `0x100` per scaler,
///   `0x800` per pipe) and each plane's gamma index/data pairs (plane base +
///   `0x50..0x60`: `0x701D0` for plane 1A, and the post-CSC segment 0 pair
///   at plane base - `0x20`: `0x70160` for plane 1A; `0x100` per plane,
///   `0x1000` per pipe): the firmware leaves their index registers in
///   auto-increment (bit 10), so reading the data dword advances them. The
///   box's dump shows `0x70160` holding the same index as `0x701D8` on every
///   pipe; `0x70260` and the like may also be the previous plane's SAGV
///   watermark in i915's map, and are held out all the same;
/// * the DKL Type-C PHY window (`0x168000`, indexed by `HIP_INDEX`): the HDMI
///   port is a combo PHY, so nothing there is needed.
///
/// Pipes B to D sit behind power wells 2 to 4: read `HSW_PWR_WELL_CTL2`
/// (`0x45404`) first.
const ADLP_GT1: u16 = 0x46B3;

/// The table, sorted by `(vendor, device, bar, start)` and non-overlapping
/// (the tests check both).
pub const RANGES: &[Range] = &[
    r(QEMU, STD_VGA, 2, 0x0000_0000, 0x0000_0400),
    r(QEMU, STD_VGA, 2, 0x0000_0500, 0x0000_0520),
    // Display fuses and chicken bits.
    r(INTEL, ADLP_GT1, 0, 0x0004_2000, 0x0004_3000),
    // DE interrupts, DBUF slices, power wells, DC states, CDCLK, DPLL enables,
    // TRANS_CLK_SEL.
    r(INTEL, ADLP_GT1, 0, 0x0004_4000, 0x0004_7000),
    // DFSM (display fuses).
    r(INTEL, ADLP_GT1, 0, 0x0005_1000, 0x0005_1010),
    // Transcoders A to D (timings, DDI function, DP_TP, infoframes), DDI
    // buffers and AUX channels, PHY_MISC, audio (less the ELD ports).
    r(INTEL, ADLP_GT1, 0, 0x0006_0000, 0x0006_5050),
    r(INTEL, ADLP_GT1, 0, 0x0006_5058, 0x0006_5150),
    r(INTEL, ADLP_GT1, 0, 0x0006_5158, 0x0006_5250),
    r(INTEL, ADLP_GT1, 0, 0x0006_5258, 0x0006_5350),
    r(INTEL, ADLP_GT1, 0, 0x0006_5358, 0x0006_6000),
    // Pipe scalers A to D, less each scaler's coefficient index/data pairs.
    r(INTEL, ADLP_GT1, 0, 0x0006_8000, 0x0006_8198),
    r(INTEL, ADLP_GT1, 0, 0x0006_81A8, 0x0006_8298),
    r(INTEL, ADLP_GT1, 0, 0x0006_82A8, 0x0006_8998),
    r(INTEL, ADLP_GT1, 0, 0x0006_89A8, 0x0006_8A98),
    r(INTEL, ADLP_GT1, 0, 0x0006_8AA8, 0x0006_9198),
    r(INTEL, ADLP_GT1, 0, 0x0006_91A8, 0x0006_9298),
    r(INTEL, ADLP_GT1, 0, 0x0006_92A8, 0x0006_9998),
    r(INTEL, ADLP_GT1, 0, 0x0006_99A8, 0x0006_9A98),
    r(INTEL, ADLP_GT1, 0, 0x0006_9AA8, 0x0006_A000),
    // Combo PHY B.
    r(INTEL, ADLP_GT1, 0, 0x0006_C000, 0x0006_D000),
    // Pipes, planes, cursors, DDB and watermarks, A to D, less each plane's
    // gamma index/data pairs.
    r(INTEL, ADLP_GT1, 0, 0x0007_0000, 0x0007_0160),
    r(INTEL, ADLP_GT1, 0, 0x0007_0168, 0x0007_01D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_01E0, 0x0007_0260),
    r(INTEL, ADLP_GT1, 0, 0x0007_0268, 0x0007_02D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_02E0, 0x0007_0360),
    r(INTEL, ADLP_GT1, 0, 0x0007_0368, 0x0007_03D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_03E0, 0x0007_0460),
    r(INTEL, ADLP_GT1, 0, 0x0007_0468, 0x0007_04D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_04E0, 0x0007_0560),
    r(INTEL, ADLP_GT1, 0, 0x0007_0568, 0x0007_05D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_05E0, 0x0007_1160),
    r(INTEL, ADLP_GT1, 0, 0x0007_1168, 0x0007_11D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_11E0, 0x0007_1260),
    r(INTEL, ADLP_GT1, 0, 0x0007_1268, 0x0007_12D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_12E0, 0x0007_1360),
    r(INTEL, ADLP_GT1, 0, 0x0007_1368, 0x0007_13D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_13E0, 0x0007_1460),
    r(INTEL, ADLP_GT1, 0, 0x0007_1468, 0x0007_14D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_14E0, 0x0007_1560),
    r(INTEL, ADLP_GT1, 0, 0x0007_1568, 0x0007_15D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_15E0, 0x0007_2160),
    r(INTEL, ADLP_GT1, 0, 0x0007_2168, 0x0007_21D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_21E0, 0x0007_2260),
    r(INTEL, ADLP_GT1, 0, 0x0007_2268, 0x0007_22D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_22E0, 0x0007_2360),
    r(INTEL, ADLP_GT1, 0, 0x0007_2368, 0x0007_23D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_23E0, 0x0007_2460),
    r(INTEL, ADLP_GT1, 0, 0x0007_2468, 0x0007_24D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_24E0, 0x0007_2560),
    r(INTEL, ADLP_GT1, 0, 0x0007_2568, 0x0007_25D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_25E0, 0x0007_3160),
    r(INTEL, ADLP_GT1, 0, 0x0007_3168, 0x0007_31D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_31E0, 0x0007_3260),
    r(INTEL, ADLP_GT1, 0, 0x0007_3268, 0x0007_32D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_32E0, 0x0007_3360),
    r(INTEL, ADLP_GT1, 0, 0x0007_3368, 0x0007_33D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_33E0, 0x0007_3460),
    r(INTEL, ADLP_GT1, 0, 0x0007_3468, 0x0007_34D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_34E0, 0x0007_3560),
    r(INTEL, ADLP_GT1, 0, 0x0007_3568, 0x0007_35D0),
    r(INTEL, ADLP_GT1, 0, 0x0007_35E0, 0x0007_4000),
    // DMC control (whether the firmware loaded DMC: HTP, SSP base).
    r(INTEL, ADLP_GT1, 0, 0x0008_F000, 0x0008_F100),
    // PCH display: hotplug, straps, panel power (less GMBUS).
    r(INTEL, ADLP_GT1, 0, 0x000C_0000, 0x000C_5100),
    r(INTEL, ADLP_GT1, 0, 0x000C_5130, 0x000D_0000),
    // DMC DC5/DC6 counters (stops before HIP_INDEX at 0x1010A0).
    r(INTEL, ADLP_GT1, 0, 0x0010_1080, 0x0010_1098),
    // Combo PHY A.
    r(INTEL, ADLP_GT1, 0, 0x0016_2000, 0x0016_3000),
    // DPCLKA_CFGCR0 and the combo and TBT DPLL CFGCRs.
    r(INTEL, ADLP_GT1, 0, 0x0016_4200, 0x0016_4300),
    // The global GTT.
    r(INTEL, ADLP_GT1, 0, 0x0080_0000, 0x0100_0000),
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
