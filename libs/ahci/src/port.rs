//! One port with an ATA disk: IDENTIFY, polled DMA reads and writes with up
//! to [`MAX_SLOTS`] commands in flight, flush and standby (AHCI 1.3.1
//! section 5, ACS-3 section 7).
//!
//! A non-NCQ command stops the wire for the others, so the slots do not add
//! throughput; queueing a transfer's commands across slots lets the driver
//! plan and issue the next one while the disk works on the current one.
//!
//! Waiting is the caller's (`wait`): the kernel parks or spins there. A
//! transfer returns only once the port is idle or stopped, so the HBA never
//! touches the caller's buffers after it returns.

use crate::cmd::{self, Cursor, Header, Plan, PlanError, Prd, MAX_COMMAND_BYTES, MAX_SLOTS};
use crate::cmd::{HEADER_BYTES, PRDBC_OFFSET, PRDT_OFFSET, PRD_BYTES};
use crate::fis::{ata, H2d};
use crate::hba::{Hba, Skip};
use crate::identify::{Disk, IDENTIFY_BYTES};
use crate::regs::{self, is, px};
use crate::reset::PortRegs;
use crate::{poll, Error, Platform, MS};

/// How long IDENTIFY may take.
const IDENTIFY_NS: u64 = 5000 * MS;
/// How long the link may take to come up on a port whose `DET` says a
/// device is there but the PHY is not yet talking.
const SETTLE_NS: u64 = 100 * MS;
/// Bytes per sector.
pub const SECTOR: usize = 512;
/// Consecutive failed recoveries before the port is detached.
const RECOVERY_LIMIT: u8 = 2;

/// Physical memory a port owns for as long as it is attached.
#[derive(Clone, Copy, Debug)]
pub struct PortPages {
    /// Command list: [`cmd::COMMAND_LIST_BYTES`] bytes, 1 KiB aligned.
    pub list: u64,
    /// Received-FIS area: 256 bytes, 256-byte aligned.
    pub fis: u64,
    /// One command table per slot: [`cmd::TABLE_BYTES`] bytes, 128-byte
    /// aligned.
    pub tables: [u64; MAX_SLOTS],
    /// IDENTIFY data lands here (512 bytes).
    pub identify: u64,
}

impl PortPages {
    fn all(&self) -> impl Iterator<Item = (u64, u64, u64)> + '_ {
        let tables = self
            .tables
            .iter()
            .map(|&t| (t, cmd::TABLE_BYTES as u64, 128));
        [
            (self.list, cmd::COMMAND_LIST_BYTES as u64, 1024),
            (self.fis, 256, 256),
            (self.identify, IDENTIFY_BYTES as u64, 2),
        ]
        .into_iter()
        .chain(tables)
    }
}

/// Read or write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read,
    Write,
}

/// A brought-up port and its disk.
pub struct Port {
    pub index: usize,
    pub disk: Disk,
    pages: PortPages,
    slots: usize,
    s64a: bool,
    /// The port accepts commands (`ST` set, no recovery pending).
    running: bool,
    failures: u8,
    detached: bool,
}

impl Port {
    /// Bring port `index` up: stop it, point it at `pages`, check for a
    /// device, start it and identify the disk.
    pub(crate) fn open(
        platform: &dyn Platform,
        hba: &Hba,
        index: usize,
        pages: PortPages,
    ) -> Result<Port, Skip> {
        let regs = PortRegs::new(platform, index);
        for (address, length, align) in pages.all() {
            if address % align != 0 {
                return Err(Skip::Failed(Error::Unsupported("misaligned DMA memory")));
            }
            if !hba.cap.s64a && address.saturating_add(length) > 1 << 32 {
                return Err(Skip::Failed(Error::Unsupported("DMA memory above 4 GiB")));
            }
        }
        if !regs.stop() {
            regs.comreset().map_err(Skip::Failed)?;
            if !regs.stop() {
                return Err(Skip::Failed(Error::Fatal));
            }
        }
        regs.set_address(px::CLB, px::CLBU, pages.list);
        regs.set_address(px::FB, px::FBU, pages.fis);
        regs.write(px::IE, 0);
        regs.clear_status();
        regs.write(px::CMD, regs.read(px::CMD) | regs::cmd::FRE);
        // Presence: DET 0 is an empty port; DET 1 is a device whose PHY is
        // still negotiating.
        if regs.read(px::SSTS) & regs::ssts::DET_MASK != 0 {
            poll(platform, SETTLE_NS, || regs.link_up());
        }
        if !regs.link_up() {
            regs.stop();
            return Err(Skip::Empty);
        }
        if !regs.wait_idle() {
            regs.stop();
            return Err(Skip::Failed(Error::Timeout));
        }
        match regs.read(px::SIG) {
            regs::SIG_ATA => {}
            regs::SIG_ATAPI => {
                regs.stop();
                return Err(Skip::Atapi);
            }
            other => {
                regs.stop();
                return Err(Skip::Unknown(other));
            }
        }
        let mut port = Port {
            index,
            // Replaced once IDENTIFY parses.
            disk: Disk::unidentified(),
            pages,
            slots: usize::from(hba.cap.slots).min(MAX_SLOTS),
            s64a: hba.cap.s64a,
            running: false,
            failures: 0,
            detached: false,
        };
        regs.clear_status();
        regs.start();
        port.running = true;
        match port.identify(platform) {
            Ok(disk) => {
                port.disk = disk;
                Ok(port)
            }
            Err(skip) => {
                port.detach(platform);
                Err(skip)
            }
        }
    }

    fn identify(&mut self, platform: &dyn Platform) -> Result<Disk, Skip> {
        let entry = Prd {
            addr: self.pages.identify,
            bytes: IDENTIFY_BYTES as u32,
        };
        platform.write_mem(self.pages.identify, &[0; IDENTIFY_BYTES]);
        let mut wait = |ready: &dyn Fn() -> bool| poll(platform, IDENTIFY_NS, ready);
        self.run(
            platform,
            H2d::plain(ata::IDENTIFY_DEVICE),
            &[entry],
            false,
            &mut wait,
        )
        .map_err(Skip::Failed)?;
        let mut raw = [0u8; IDENTIFY_BYTES];
        platform.read_mem(self.pages.identify, &mut raw);
        Disk::parse(&raw).map_err(Skip::Refused)
    }

    pub fn is_detached(&self) -> bool {
        self.detached
    }

    /// Stop the port for good: it touches no memory afterwards and every
    /// later request fails.
    pub fn detach(&mut self, platform: &dyn Platform) {
        self.detached = true;
        self.running = false;
        PortRegs::new(platform, self.index).stop();
    }

    /// A command is bad if the disk or the bus said so. Only `PxIS` counts:
    /// it is cleared by the driver before each transfer, while `PxTFD.ERR`
    /// keeps the last error until a later command overwrites it, so it would
    /// fail every command after one bad sector. `PxTFD` only supplies the
    /// detail.
    fn fault(regs: &PortRegs) -> Option<Error> {
        let status = regs.read(px::IS);
        if status & is::TFES != 0 {
            let task = regs.read(px::TFD);
            return Some(Error::TaskFile {
                status: task as u8,
                error: (task >> 8) as u8,
            });
        }
        (status & is::FATAL != 0).then_some(Error::Bus(status))
    }

    /// Stop the port and bring it back after a failure. Two failed recoveries
    /// in a row detach it.
    fn recover(&mut self, platform: &dyn Platform) {
        self.running = false;
        match PortRegs::new(platform, self.index).recover() {
            Ok(()) => {
                self.running = true;
                self.failures = 0;
            }
            Err(_) => {
                self.failures += 1;
                if self.failures >= RECOVERY_LIMIT {
                    self.detach(platform);
                }
            }
        }
    }

    /// A port that is not running (an earlier recovery failed) gets another
    /// try before the next command.
    fn ready(&mut self, platform: &dyn Platform) -> Result<(), Error> {
        if self.detached {
            return Err(Error::Detached);
        }
        if !self.running {
            self.recover(platform);
            if !self.running {
                return Err(if self.detached {
                    Error::Detached
                } else {
                    Error::Fatal
                });
            }
        }
        Ok(())
    }

    /// Write slot `slot`'s table and header and set its `PxCI` bit.
    fn issue(&self, platform: &dyn Platform, slot: usize, fis: &H2d, prds: &[Prd], write: bool) {
        let table = self.pages.tables[slot];
        platform.write_mem(table, &fis.encode());
        for (index, prd) in prds.iter().enumerate() {
            let at = table + (PRDT_OFFSET + index * PRD_BYTES) as u64;
            platform.write_mem(at, &prd.encode());
        }
        let header = Header {
            write,
            prdtl: prds.len() as u16,
            ctba: table,
        };
        platform.write_mem(
            self.pages.list + (slot * HEADER_BYTES) as u64,
            &header.encode(),
        );
        PortRegs::new(platform, self.index).write(px::CI, 1 << slot);
    }

    /// Bytes the HBA reports having moved for `slot`.
    fn moved(&self, platform: &dyn Platform, slot: usize) -> u32 {
        let mut raw = [0u8; 4];
        let at = self.pages.list + (slot * HEADER_BYTES + PRDBC_OFFSET) as u64;
        platform.read_mem(at, &mut raw);
        u32::from_le_bytes(raw)
    }

    /// Run one command in slot 0 and wait for it.
    fn run(
        &mut self,
        platform: &dyn Platform,
        fis: H2d,
        prds: &[Prd],
        write: bool,
        wait: &mut dyn FnMut(&dyn Fn() -> bool) -> bool,
    ) -> Result<(), Error> {
        self.ready(platform)?;
        let regs = PortRegs::new(platform, self.index);
        regs.clear_status();
        self.issue(platform, 0, &fis, prds, write);
        let ready = || regs.read(px::CI) & 1 == 0 || Self::fault(&regs).is_some();
        if !wait(&ready) {
            self.recover(platform);
            return Err(Error::Timeout);
        }
        if let Some(error) = Self::fault(&regs) {
            self.recover(platform);
            return Err(error);
        }
        if regs.read(px::CI) & 1 != 0 {
            self.recover(platform);
            return Err(Error::Timeout);
        }
        let expected: u32 = prds.iter().map(|prd| prd.bytes).sum();
        if !prds.is_empty() && self.moved(platform, 0) != expected {
            self.recover(platform);
            return Err(Error::ShortTransfer);
        }
        Ok(())
    }

    /// Move the bytes of `segments` (`(virtual address, length)`, back to
    /// back) to (`Op::Write`) or from the disk at sector `lba`. `translate`
    /// maps a virtual address to its physical one; `wait` is called with a
    /// readiness test while commands are outstanding and returns whether it
    /// held before the caller's deadline.
    pub fn transfer(
        &mut self,
        platform: &dyn Platform,
        op: Op,
        lba: u64,
        segments: &[(u64, usize)],
        translate: &dyn Fn(u64) -> Option<u64>,
        wait: &mut dyn FnMut(&dyn Fn() -> bool) -> bool,
    ) -> Result<(), Error> {
        if self.detached {
            return Err(Error::Detached);
        }
        let total: usize = segments.iter().map(|&(_, len)| len).sum();
        if !total.is_multiple_of(SECTOR) {
            return Err(Error::Bounds);
        }
        let sectors = (total / SECTOR) as u64;
        if lba
            .checked_add(sectors)
            .is_none_or(|end| end > self.disk.sectors)
        {
            return Err(Error::Bounds);
        }
        if total == 0 {
            return Ok(());
        }
        self.ready(platform)?;
        let regs = PortRegs::new(platform, self.index);
        regs.clear_status();
        let (opcode, write) = match op {
            Op::Read => (ata::READ_DMA_EXT, false),
            Op::Write => (ata::WRITE_DMA_EXT, true),
        };
        let mut cursor = Cursor::default();
        cursor.advance(segments, 0);
        let mut submitted = 0usize;
        // Bytes each slot's command moves, while it is in flight.
        let mut inflight: [Option<usize>; MAX_SLOTS] = [None; MAX_SLOTS];
        let mut result = Ok(());
        loop {
            while result.is_ok() && submitted < total {
                let Some(slot) = inflight[..self.slots].iter().position(Option::is_none) else {
                    break;
                };
                let plan: Plan = match cmd::plan(
                    segments,
                    cursor,
                    MAX_COMMAND_BYTES,
                    SECTOR,
                    self.s64a,
                    translate,
                ) {
                    Ok(plan) => plan,
                    Err(PlanError::Unmapped) => {
                        result = Err(Error::Unmapped);
                        break;
                    }
                    Err(PlanError::Misaligned) => {
                        result = Err(Error::Misaligned);
                        break;
                    }
                };
                let at = lba + (submitted / SECTOR) as u64;
                let fis = H2d::lba48(opcode, at, (plan.bytes / SECTOR) as u16);
                self.issue(platform, slot, &fis, plan.entries(), write);
                inflight[slot] = Some(plan.bytes);
                cursor.advance(segments, plan.bytes);
                submitted += plan.bytes;
            }
            // An error stops the port; whatever is in flight is lost.
            if let Some(error) = Self::fault(&regs) {
                self.recover(platform);
                return Err(error);
            }
            let ci = regs.read(px::CI);
            for (slot, entry) in inflight.iter_mut().enumerate() {
                let Some(expected) = *entry else {
                    continue;
                };
                if ci & (1 << slot) != 0 {
                    continue;
                }
                if self.moved(platform, slot) as usize != expected && result.is_ok() {
                    result = Err(Error::ShortTransfer);
                }
                *entry = None;
            }
            let issued = inflight
                .iter()
                .enumerate()
                .filter(|(_, bytes)| bytes.is_some())
                .fold(0u32, |mask, (slot, _)| mask | 1 << slot);
            if issued == 0 {
                if submitted == total || result.is_err() {
                    if result.is_err() {
                        // A short transfer leaves the port in an unknown
                        // state; the plan errors left it clean.
                        if matches!(result, Err(Error::ShortTransfer)) {
                            self.recover(platform);
                        }
                    }
                    return result;
                }
                continue;
            }
            let ready = || regs.read(px::CI) & issued != issued || Self::fault(&regs).is_some();
            if !wait(&ready) {
                self.recover(platform);
                return Err(Error::Timeout);
            }
        }
    }

    /// Flush the disk's volatile write cache, if it has one on.
    pub fn flush(
        &mut self,
        platform: &dyn Platform,
        wait: &mut dyn FnMut(&dyn Fn() -> bool) -> bool,
    ) -> Result<(), Error> {
        if !self.disk.write_cache {
            return if self.detached {
                Err(Error::Detached)
            } else {
                Ok(())
            };
        }
        self.run(platform, H2d::plain(ata::FLUSH_CACHE_EXT), &[], false, wait)
    }

    /// Power-off: flush, then park the drive with `STANDBY IMMEDIATE`, then
    /// detach. The flush is attempted even if the disk says its cache is
    /// off, since the standby needs the port quiet anyway.
    pub fn shutdown(
        &mut self,
        platform: &dyn Platform,
        wait: &mut dyn FnMut(&dyn Fn() -> bool) -> bool,
    ) -> Result<(), Error> {
        let flushed = self.flush(platform, wait);
        let parked = self.run(
            platform,
            H2d::plain(ata::STANDBY_IMMEDIATE),
            &[],
            false,
            wait,
        );
        self.detach(platform);
        flushed.and(parked)
    }
}
