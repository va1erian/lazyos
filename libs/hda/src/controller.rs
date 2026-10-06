//! The controller: link reset, the codec it found, and the CORB/RIRB command
//! rings every verb travels through (HDA specification sections 4.4 and 3.3).
//!
//! The rings live in driver-owned DMA memory. A command is written at the next
//! CORB entry and the write pointer moved; the codec's answer appears in the
//! RIRB, which the driver reads up to the controller's write pointer.
//! Unsolicited responses (a jack event) are skipped. Polling is bounded: a
//! controller that never answers costs a timeout, never a hang.

use core::sync::atomic::{fence, Ordering};

use crate::regs::{gctl, ring, Regs, CORBCTL, CORBLBASE, CORBRP, CORBSIZE, CORBUBASE, CORBWP};
use crate::regs::{GCAP, GCTL, RINTCNT, RIRBCTL, RIRBLBASE, RIRBSIZE, RIRBSTS, RIRBUBASE};
use crate::regs::{RIRBWP, SD_BASE, SD_STRIDE, STATESTS};
use crate::verbs::{VerbError, Verbs};

/// Ring entries (the controller must offer 256; every one this driver knows
/// does).
pub const ENTRIES: u16 = 256;
/// Bytes of command and response ring memory, and where the response ring
/// starts in it (both 128-byte aligned).
pub const CORB_BYTES: usize = ENTRIES as usize * 4;
pub const RIRB_OFFSET: usize = CORB_BYTES;
pub const RING_BYTES: usize = RIRB_OFFSET + ENTRIES as usize * 8;
/// The response's extended word: unsolicited flag.
const UNSOLICITED: u32 = 1 << 4;
/// Register polls spent spinning before each `wait` (a codec answers in
/// microseconds), and waits before giving up.
const SPINS: u32 = 64;
const WAITS: u32 = 50;

/// Why the controller could not be brought up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerError {
    /// `GCTL.CRST` never settled.
    ResetTimeout,
    /// No codec reported its presence on the link.
    NoCodec,
    /// The controller cannot offer 256-entry command rings.
    RingSize,
    /// The controller has no output stream.
    NoOutputStream,
}

/// What `GCAP` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub inputs: u8,
    pub outputs: u8,
    pub bidirectional: u8,
}

/// The ring memory: one physically contiguous block of [`RING_BYTES`].
#[derive(Clone, Copy, Debug)]
pub struct RingMemory {
    pub va: *mut u8,
    pub bus: u64,
}

/// The controller with its command rings running.
pub struct Controller<R: Regs> {
    regs: R,
    rings: RingMemory,
    corb_wp: u16,
    rirb_rp: u16,
    /// The response write pointer when responses were last taken.
    rirb_seen: u16,
    wait: fn(),
    /// The address of the codec the driver talks to.
    pub codec: u8,
    pub caps: Caps,
}

/// Poll `done` up to a bounded number of times, waiting in between.
fn settle(mut done: impl FnMut() -> bool, wait: fn()) -> bool {
    for attempt in 0..SPINS + WAITS {
        if done() {
            return true;
        }
        if attempt >= SPINS {
            wait();
        }
    }
    done()
}

impl<R: Regs> Controller<R> {
    /// Reset the link, find the first codec, and start the command rings in
    /// `rings`. `wait` naps between register polls.
    ///
    /// # Safety
    /// `rings.va` must point at [`RING_BYTES`] of memory, 128-byte aligned,
    /// that the controller reaches at `rings.bus` and nobody else touches
    /// while the controller lives.
    pub unsafe fn new(mut regs: R, rings: RingMemory, wait: fn()) -> Result<Self, ControllerError> {
        // Stop any ring DMA a firmware left running before the reset.
        regs.write8(CORBCTL, 0);
        regs.write8(RIRBCTL, 0);
        regs.write32(GCTL, regs.read32(GCTL) & !gctl::CRST);
        if !settle(|| regs.read32(GCTL) & gctl::CRST == 0, wait) {
            return Err(ControllerError::ResetTimeout);
        }
        regs.write32(GCTL, regs.read32(GCTL) | gctl::CRST);
        if !settle(|| regs.read32(GCTL) & gctl::CRST != 0, wait) {
            return Err(ControllerError::ResetTimeout);
        }
        // Codecs need 521 us after the reset to announce themselves.
        wait();
        let present = regs.read16(STATESTS);
        let codec = (0..15u8)
            .find(|n| present >> n & 1 == 1)
            .ok_or(ControllerError::NoCodec)?;
        let gcap = regs.read16(GCAP);
        let caps = Caps {
            outputs: ((gcap >> 12) & 0xF) as u8,
            inputs: ((gcap >> 8) & 0xF) as u8,
            bidirectional: ((gcap >> 3) & 0x1F) as u8,
        };
        if caps.outputs == 0 {
            return Err(ControllerError::NoOutputStream);
        }
        let mut controller = Controller {
            regs,
            rings,
            corb_wp: 0,
            rirb_rp: 0,
            rirb_seen: 0,
            wait,
            codec,
            caps,
        };
        controller.start_rings()?;
        Ok(controller)
    }

    fn start_rings(&mut self) -> Result<(), ControllerError> {
        let regs = &mut self.regs;
        if regs.read8(CORBSIZE) & ring::CAP_256 == 0 || regs.read8(RIRBSIZE) & ring::CAP_256 == 0 {
            return Err(ControllerError::RingSize);
        }
        // SAFETY: the ring memory is the controller's (`new`'s contract) and
        // its DMA is stopped.
        unsafe { core::ptr::write_bytes(self.rings.va, 0, RING_BYTES) };
        let (corb, rirb) = (self.rings.bus, self.rings.bus + RIRB_OFFSET as u64);
        regs.write32(CORBLBASE, corb as u32);
        regs.write32(CORBUBASE, (corb >> 32) as u32);
        regs.write8(CORBSIZE, ring::SIZE_256);
        regs.write32(RIRBLBASE, rirb as u32);
        regs.write32(RIRBUBASE, (rirb >> 32) as u32);
        regs.write8(RIRBSIZE, ring::SIZE_256);
        // Reset the read pointer. Some controllers never latch the bit, so,
        // like other drivers, wait a bounded time each way and go on.
        regs.write16(CORBRP, ring::PTR_RESET);
        let wait = self.wait;
        settle(|| regs.read16(CORBRP) & ring::PTR_RESET != 0, wait);
        regs.write16(CORBRP, 0);
        settle(|| regs.read16(CORBRP) & ring::PTR_RESET == 0, wait);
        regs.write16(CORBWP, 0);
        regs.write16(RIRBWP, ring::PTR_RESET);
        regs.write16(RINTCNT, 1);
        regs.write8(RIRBSTS, ring::RIRBSTS_ALL);
        regs.write8(CORBCTL, ring::CORB_RUN);
        regs.write8(RIRBCTL, ring::RIRB_DMAEN | ring::RIRB_RINTCTL);
        self.corb_wp = 0;
        self.rirb_rp = 0;
        self.rirb_seen = 0;
        Ok(())
    }

    pub fn regs(&self) -> &R {
        &self.regs
    }

    pub fn regs_mut(&mut self) -> &mut R {
        &mut self.regs
    }

    /// Register offset of output stream descriptor `index` (output streams
    /// follow the input ones), if the controller has it.
    pub fn output_stream(&self, index: u8) -> Option<u32> {
        (index < self.caps.outputs)
            .then(|| SD_BASE + SD_STRIDE * u32::from(self.caps.inputs + index))
    }

    /// The `(response, extended)` pair at RIRB entry `index`.
    fn response(&self, index: u16) -> (u32, u32) {
        let at = RIRB_OFFSET + usize::from(index % ENTRIES) * 8;
        // SAFETY: `at + 8 <= RING_BYTES`; the controller writes the entry and
        // we only read it, volatile, as untrusted data.
        unsafe {
            let entry = self.rings.va.add(at) as *const u32;
            (entry.read_volatile(), entry.add(1).read_volatile())
        }
    }
}

impl<R: Regs> Verbs for Controller<R> {
    fn send(&mut self, command: u32) -> Result<u32, VerbError> {
        let wp = (self.corb_wp + 1) % ENTRIES;
        // SAFETY: `wp < ENTRIES`, so the entry is inside the CORB; the
        // controller only reads entries up to the write pointer, which has not
        // moved yet.
        unsafe { (self.rings.va.add(usize::from(wp) * 4) as *mut u32).write_volatile(command) };
        fence(Ordering::Release);
        self.regs.write16(CORBWP, wp);
        self.corb_wp = wp;
        for attempt in 0..SPINS + WAITS {
            let written = self.regs.read16(RIRBWP) % ENTRIES;
            let mut solicited = None;
            while self.rirb_rp != written && solicited.is_none() {
                self.rirb_rp = (self.rirb_rp + 1) % ENTRIES;
                fence(Ordering::Acquire);
                let (response, extended) = self.response(self.rirb_rp);
                if extended & UNSOLICITED == 0 {
                    solicited = Some(response);
                }
            }
            if written != self.rirb_seen {
                // Responses were taken: clear the response flag, and write
                // the unchanged write pointer again, so a controller that
                // paused the command ring at the response count takes the
                // command still waiting (a no-op for one that did not).
                self.rirb_seen = written;
                self.regs.write8(RIRBSTS, ring::RIRBSTS_ALL);
                self.regs.write16(CORBWP, self.corb_wp);
            }
            if let Some(response) = solicited {
                return Ok(response);
            }
            if attempt >= SPINS {
                (self.wait)();
            }
        }
        Err(VerbError::Timeout)
    }
}
