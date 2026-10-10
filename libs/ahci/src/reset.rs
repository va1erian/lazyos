//! Stopping and starting a port, COMRESET and error recovery (AHCI 1.3.1
//! sections 10.1.2, 10.3.1 and 10.3.2).

use crate::regs::{self, cmd, px, sctl, ssts, tfd};
use crate::{poll, Error, Platform, MS};

/// How long `CR`/`FR` may take to clear after `ST`/`FRE` (spec: 500 ms).
const STOP_NS: u64 = 500 * MS;
/// How long a link may take to come back after COMRESET.
const LINK_NS: u64 = 1000 * MS;
/// How long a device may stay busy after it came up.
const READY_NS: u64 = 1000 * MS;

/// Register access for one port.
#[derive(Clone, Copy)]
pub struct PortRegs<'a> {
    pub platform: &'a dyn Platform,
    base: usize,
}

impl<'a> PortRegs<'a> {
    pub fn new(platform: &'a dyn Platform, index: usize) -> PortRegs<'a> {
        PortRegs {
            platform,
            base: regs::port_base(index),
        }
    }

    pub fn read(&self, offset: usize) -> u32 {
        self.platform.read32(self.base + offset)
    }

    pub fn write(&self, offset: usize, value: u32) {
        self.platform.write32(self.base + offset, value);
    }

    pub fn set_address(&self, low: usize, high: usize, phys: u64) {
        self.write(low, phys as u32);
        self.write(high, (phys >> 32) as u32);
    }

    /// Write ones to `PxSERR` and `PxIS`: forget old errors and events.
    pub fn clear_status(&self) {
        self.write(px::SERR, u32::MAX);
        self.write(px::IS, self.read(px::IS));
    }

    /// A device is present and the link is active.
    pub fn link_up(&self) -> bool {
        let status = self.read(px::SSTS);
        status & ssts::DET_MASK == ssts::DET_PRESENT
            && (status >> ssts::IPM_SHIFT) & ssts::IPM_MASK == ssts::IPM_ACTIVE
    }

    /// `PxTFD` shows neither `BSY` nor `DRQ`.
    pub fn idle(&self) -> bool {
        self.read(px::TFD) & tfd::BUSY_MASK == 0
    }

    pub fn wait_idle(&self) -> bool {
        poll(self.platform, READY_NS, || self.idle())
    }

    /// The port is still processing commands (`PxCMD.CR`).
    pub fn running(&self) -> bool {
        self.read(px::CMD) & cmd::CR != 0
    }

    /// Clear `ST` and wait for `CR`. Command processing is stopped, every
    /// issued slot dropped (`PxCI` clears), and the HBA no longer touches
    /// the data buffers. The FIS receive area is left running.
    pub fn stop_commands(&self) -> bool {
        let command = self.read(px::CMD);
        if command & (cmd::ST | cmd::CR) == 0 {
            return true;
        }
        self.write(px::CMD, command & !cmd::ST);
        poll(self.platform, STOP_NS, || self.read(px::CMD) & cmd::CR == 0)
    }

    /// [`Self::stop_commands`], then clear `FRE` and wait for `FR`.
    pub fn stop(&self) -> bool {
        if !self.stop_commands() {
            return false;
        }
        let command = self.read(px::CMD);
        if command & (cmd::FRE | cmd::FR) == 0 {
            return true;
        }
        self.write(px::CMD, command & !cmd::FRE);
        poll(self.platform, STOP_NS, || self.read(px::CMD) & cmd::FR == 0)
    }

    /// Set `FRE` and then `ST`.
    pub fn start(&self) {
        let command = self.read(px::CMD);
        self.write(px::CMD, command | cmd::FRE);
        self.write(px::CMD, self.read(px::CMD) | cmd::ST);
    }

    /// COMRESET: `DET = 1` for at least a millisecond, then wait for the
    /// link, forget the errors the reset caused and wait for the device.
    pub fn comreset(&self) -> Result<(), Error> {
        let control = self.read(px::SCTL) & !sctl::DET_MASK;
        self.write(px::SCTL, control | sctl::DET_RESET);
        let start = self.platform.now_ns();
        poll(self.platform, 2 * MS, || {
            self.platform.now_ns().saturating_sub(start) >= MS
        });
        self.write(px::SCTL, control);
        if !poll(self.platform, LINK_NS, || self.link_up()) {
            return Err(Error::Timeout);
        }
        self.write(px::SERR, u32::MAX);
        if self.wait_idle() {
            Ok(())
        } else {
            Err(Error::Timeout)
        }
    }

    /// 3.4: stop the port, forget its errors, COMRESET when the device is
    /// still busy, start it again. `Err` means the port is unusable.
    pub fn recover(&self) -> Result<(), Error> {
        if !self.stop_commands() {
            // `ST` will not clear: only a reset can free the port.
            self.comreset()?;
            if !self.stop_commands() {
                return Err(Error::Fatal);
            }
        }
        self.clear_status();
        if !self.idle() || !self.link_up() {
            self.comreset()?;
        }
        self.clear_status();
        self.start();
        Ok(())
    }
}
