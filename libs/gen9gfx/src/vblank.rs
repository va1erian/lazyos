//! Stage G1's "observe" half, read-only: the pipe's frame counter and scan
//! line (where the beam is), and the display engine's per-pipe interrupt
//! registers (whether a FIFO underrun was latched).
//!
//! Nothing here writes a register or takes an interrupt. The frame counter
//! counts every vblank whether or not anybody asked for vblank interrupts, so
//! two reads some time apart give the refresh rate the monitor really runs
//! at, which is G1's exit criterion ("vblank ticks at the monitor's rate"),
//! and the scan line is what a presenter that wants no tear needs to know
//! (G5 flips at vblank; before them, a copy can avoid the beam).
//!
//! The pipe's `PIPEDSL` and `PIPE_FRMCOUNT` offsets are from i915's
//! `i915_reg.h` (`PIPEDSL(pipe)` 0x70000, `PIPE_FRMCOUNT_G4X(pipe)` 0x70040,
//! same 0x1000 stride as every pipe register) and are **to confirm** on the
//! box: the Linux dump does not name them. The DE pipe interrupt registers are
//! **CONFIRMED** (`GEN8_DE_PIPE_{ISR,IMR,IIR,IER}0..2` at 0x44400, stride 0x10,
//! in `docs/compat/kabylake/gpu/linux/01-intel_reg-dump.txt`).

use crate::regs::pipe_base;
use crate::Mmio;

/// `PIPEDSL`: the display scan line, offset inside the pipe block.
pub const PIPEDSL: u32 = 0x00;
/// Bits 12:0 of `PIPEDSL` are the line (Gen9 `DSL_LINEMASK_GEN3`).
pub const DSL_LINE_MASK: u32 = 0x1FFF;
/// `PIPE_FRMCOUNT`: 32-bit count of frames (vblank starts), inside the pipe
/// block.
pub const PIPE_FRMCOUNT: u32 = 0x40;

/// `GEN8_DE_PIPE_ISR(pipe)`, CONFIRMED; IMR, IIR and IER follow at +4, +8,
/// +0xC.
pub const fn de_pipe_isr(pipe: u32) -> u32 {
    0x4_4400 + pipe * 0x10
}

/// Bits of the DE pipe interrupt registers this stage reads (i915's
/// `GEN8_PIPE_*`).
pub mod de_pipe {
    pub const VBLANK: u32 = 1 << 0;
    pub const FIFO_UNDERRUN: u32 = 1 << 31;
}

/// Where the beam of one pipe was at one read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Beam {
    /// Frames since the pipe was enabled (wraps at 2^32).
    pub frame: u32,
    /// The scan line, 0 at the top of the active area.
    pub line: u32,
}

impl Beam {
    /// Read the beam of `pipe` (0 = A). The counter is read on both sides of
    /// the scan line so a frame boundary between the two reads cannot pair a
    /// new frame's number with the old frame's last line: when the counter
    /// moved, the line is read again with the new frame.
    pub fn read(mmio: &mut impl Mmio, pipe: u32) -> Beam {
        let base = pipe_base(pipe);
        let mut frame = mmio.read32(base + PIPE_FRMCOUNT);
        let mut line = mmio.read32(base + PIPEDSL) & DSL_LINE_MASK;
        let after = mmio.read32(base + PIPE_FRMCOUNT);
        if after != frame {
            frame = after;
            line = mmio.read32(base + PIPEDSL) & DSL_LINE_MASK;
        }
        Beam { frame, line }
    }
}

/// The DE pipe interrupt registers of one pipe, raw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PipeIrq {
    pub isr: u32,
    pub imr: u32,
    pub iir: u32,
    pub ier: u32,
}

impl PipeIrq {
    /// Read the four registers of `pipe`. IIR is only read, never cleared:
    /// a bit latched before `gpud` started stays for the next reader.
    pub fn read(mmio: &mut impl Mmio, pipe: u32) -> PipeIrq {
        let isr = de_pipe_isr(pipe);
        PipeIrq {
            isr: mmio.read32(isr),
            imr: mmio.read32(isr + 4),
            iir: mmio.read32(isr + 8),
            ier: mmio.read32(isr + 0xC),
        }
    }

    /// What can be said about FIFO underruns from these values.
    pub fn underrun(&self) -> Underrun {
        if self.iir & de_pipe::FIFO_UNDERRUN != 0 {
            Underrun::Latched
        } else if self.imr & de_pipe::FIFO_UNDERRUN != 0 {
            // A masked event never reaches IIR: no news is no evidence.
            Underrun::Unobserved
        } else {
            Underrun::None
        }
    }
}

/// FIFO underrun state of a pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Underrun {
    /// Unmasked and nothing latched.
    None,
    /// IIR holds an underrun nobody has cleared.
    Latched,
    /// The event is masked in IMR, so IIR cannot tell.
    Unobserved,
}

/// The refresh rate two beam reads `elapsed_ns` apart imply, in millihertz;
/// `None` when the interval or the frame count is too small to say anything
/// (fewer than 2 frames, or under a millisecond). The counter is assumed to
/// have wrapped at most once.
pub fn measured_millihz(first: Beam, last: Beam, elapsed_ns: u64, vtotal: u32) -> Option<u32> {
    if elapsed_ns < 1_000_000 {
        return None;
    }
    let frames = u64::from(last.frame.wrapping_sub(first.frame));
    if frames < 2 {
        return None;
    }
    // Count the partial frames at both ends from the scan lines when the
    // vertical total is known: a whole frame count over 1 s is only good to
    // one frame in sixty, the lines make it good to one line in a frame.
    let scaled = match u64::from(vtotal) {
        0 => frames * 1_000_000,
        total => {
            let first_line = u64::from(first.line).min(total);
            let last_line = u64::from(last.line).min(total);
            (frames * total + last_line - first_line) * 1_000_000 / total
        }
    };
    // frames per ns * 1e9 * 1000 = millihertz.
    let millihz = scaled * 1_000_000 / elapsed_ns;
    u32::try_from(millihz).ok()
}

/// Whether a measured rate agrees with the one the timing registers imply,
/// within `tolerance_ppm` parts per million (both in millihertz).
pub fn agrees(measured: u32, expected: u32, tolerance_ppm: u32) -> bool {
    if expected == 0 {
        return false;
    }
    let diff = u64::from(measured.abs_diff(expected));
    diff * 1_000_000 <= u64::from(expected) * u64::from(tolerance_ppm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mmio::FakeMmio;

    #[test]
    fn a_beam_read_never_mixes_two_frames() {
        // The counter moves between the first two reads: the line is read
        // again with the new frame.
        struct Ticking {
            reads: u32,
        }
        impl Mmio for Ticking {
            fn read32(&mut self, offset: u32) -> u32 {
                self.reads += 1;
                match (offset, self.reads) {
                    (0x70040, 1) => 10,
                    (0x70000, 2) => 1499,
                    (0x70040, _) => 11,
                    (0x70000, _) => 0x8000_0002,
                    _ => 0,
                }
            }
            fn write32(&mut self, _: u32, _: u32) {
                unreachable!("read-only")
            }
        }
        let beam = Beam::read(&mut Ticking { reads: 0 }, 0);
        assert_eq!(beam, Beam { frame: 11, line: 2 });
    }

    #[test]
    fn a_beam_read_of_pipe_b_uses_its_block_and_writes_nothing() {
        let mut m = FakeMmio::default();
        m.regs.insert(0x71040, 7);
        m.regs.insert(0x71000, 0xFFFF_E123);
        assert_eq!(
            Beam::read(&mut m, 1),
            Beam {
                frame: 7,
                line: 0x123
            }
        );
        assert!(m.log.iter().all(|&(_, _, write)| !write));
    }

    #[test]
    fn underrun_reads_the_confirmed_registers() {
        let mut m = FakeMmio::default();
        // The box under i915: pipe A's IMR leaves bit 31 unmasked.
        m.regs.insert(0x44404, 0x6fff_f07f);
        m.regs.insert(0x4440C, 0x9000_0f89);
        let irq = PipeIrq::read(&mut m, 0);
        assert_eq!(irq.underrun(), Underrun::None);
        m.regs.insert(0x44408, de_pipe::FIFO_UNDERRUN);
        assert_eq!(PipeIrq::read(&mut m, 0).underrun(), Underrun::Latched);
        // Pipe B under i915 masks it.
        m.regs.insert(0x44414, 0xefff_f07f);
        assert_eq!(PipeIrq::read(&mut m, 1).underrun(), Underrun::Unobserved);
        assert!(m.log.iter().all(|&(_, _, write)| !write));
    }

    #[test]
    fn the_box_timings_measure_to_their_refresh() {
        // 2560x1440 at 241.5 MHz: htotal 2720, vtotal 1481, 59.951 Hz.
        let expected = (241_500u64 * 1_000_000 / (2720 * 1481)) as u32;
        let frame_ns = 1_000_000_000_000u64 / u64::from(expected);
        // 120 frames and 300 lines later.
        let elapsed = frame_ns * 120 + frame_ns * 300 / 1481;
        let first = Beam {
            frame: u32::MAX - 5,
            line: 100,
        };
        let last = Beam {
            frame: first.frame.wrapping_add(120),
            line: 400,
        };
        let measured = measured_millihz(first, last, elapsed, 1481).unwrap();
        assert!(agrees(measured, expected, 100), "{measured} vs {expected}");
        // Without the line count it is still within a frame of 120.
        let rough = measured_millihz(first, last, elapsed, 0).unwrap();
        assert!(agrees(rough, expected, 1_000_000 / 100), "{rough}");
        // A 70 Hz mode is not mistaken for it.
        assert!(!agrees(70_000, expected, 1000));
    }

    #[test]
    fn too_little_to_measure_is_none() {
        let a = Beam { frame: 5, line: 0 };
        assert_eq!(measured_millihz(a, a, 1_000_000_000, 1481), None);
        assert_eq!(
            measured_millihz(a, Beam { frame: 9, line: 0 }, 10, 1481),
            None
        );
        assert!(!agrees(60_000, 0, 1_000_000));
    }
}
