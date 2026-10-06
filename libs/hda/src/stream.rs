//! One output stream descriptor: reset, programming with a buffer descriptor
//! list, run/stop, the link position, and the completion status.
//!
//! The stream plays a cyclic buffer of `periods` equal periods; the BDL has one
//! entry per period, each asking for a completion interrupt. The control
//! register is 24 bits wide with the status byte above it, so it is always
//! written a byte at a time: a 32-bit write would also write the
//! write-one-to-clear status.

use core::sync::atomic::{fence, Ordering};

use crate::regs::{sd, sdctl, sdsts, Regs};

/// Bytes per buffer descriptor list entry.
pub const BDL_ENTRY: usize = 16;
/// Most periods a stream may have (the BDL holds up to 256; audio periods are
/// few and long).
pub const MAX_PERIODS: usize = 32;
/// Polls of a self-clearing bit before giving up.
const POLLS: u32 = 1000;

/// Write a BDL of `periods` entries of `period` bytes each, starting at bus
/// address `buffer`, every entry asking for a completion interrupt.
///
/// # Safety
/// `bdl` must point at `periods * BDL_ENTRY` writable bytes the controller is
/// not reading (its stream is stopped).
pub unsafe fn write_bdl(bdl: *mut u8, buffer: u64, period: u32, periods: usize) {
    for index in 0..periods {
        let entry = bdl.add(index * BDL_ENTRY);
        let address = buffer + u64::from(period) * index as u64;
        (entry as *mut u64).write_volatile(address);
        (entry.add(8) as *mut u32).write_volatile(period);
        (entry.add(12) as *mut u32).write_volatile(1); // IOC
    }
    fence(Ordering::Release);
}

/// An output stream descriptor at register offset `base`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutStream {
    pub base: u32,
}

impl OutStream {
    fn ctl(&self, regs: &impl Regs) -> u8 {
        regs.read8(self.base + sd::CTL)
    }

    /// Put the descriptor through a stream reset. False if the controller
    /// never acknowledged it.
    pub fn reset(&self, regs: &mut impl Regs, wait: fn()) -> bool {
        let base = self.base + sd::CTL;
        regs.write8(base, self.ctl(regs) & !(sdctl::RUN as u8));
        regs.write8(base, sdctl::SRST as u8);
        let mut polls = 0;
        while self.ctl(regs) & sdctl::SRST as u8 == 0 {
            polls += 1;
            if polls >= POLLS {
                return false;
            }
            wait();
        }
        regs.write8(base, 0);
        polls = 0;
        while self.ctl(regs) & sdctl::SRST as u8 != 0 {
            polls += 1;
            if polls >= POLLS {
                return false;
            }
            wait();
        }
        regs.write8(self.base + sd::STS, sdsts::ALL);
        true
    }

    /// Point the stopped descriptor at its BDL and buffer.
    pub fn program(
        &self,
        regs: &mut impl Regs,
        tag: u8,
        bdl: u64,
        cbl: u32,
        periods: u16,
        format: u16,
    ) {
        regs.write32(self.base + sd::BDPL, bdl as u32);
        regs.write32(self.base + sd::BDPU, (bdl >> 32) as u32);
        regs.write32(self.base + sd::CBL, cbl);
        regs.write16(self.base + sd::LVI, periods - 1);
        regs.write16(self.base + sd::FMT, format);
        regs.write8(self.base + sd::CTL + 2, (tag & 0xF) << 4);
        regs.write8(self.base + sd::CTL, sdctl::IOCE as u8);
    }

    /// Start or stop the DMA engine; true once the controller reports it.
    pub fn run(&self, regs: &mut impl Regs, on: bool, wait: fn()) -> bool {
        let control = self.ctl(regs);
        let run = sdctl::RUN as u8;
        regs.write8(
            self.base + sd::CTL,
            if on { control | run } else { control & !run },
        );
        for _ in 0..POLLS {
            if (self.ctl(regs) & run != 0) == on {
                return true;
            }
            wait();
        }
        false
    }

    /// The link position, in bytes into the cyclic buffer.
    pub fn position(&self, regs: &impl Regs) -> u32 {
        regs.read32(self.base + sd::LPIB)
    }

    /// Read and clear the status bits (a completion, a FIFO or descriptor
    /// error).
    pub fn take_status(&self, regs: &mut impl Regs) -> u8 {
        let status = regs.read8(self.base + sd::STS) & sdsts::ALL;
        if status != 0 {
            regs.write8(self.base + sd::STS, status);
        }
        status
    }
}
