//! ATA PIO driver for the primary IDE channel (issue #100: behind the block
//! layer).
//!
//! The primary master is where QEMU puts a plain `-drive` image, and it stays
//! the fallback boot device. The driver speaks 28-bit LBA programmed I/O, so
//! it needs no DMA and no interrupt: every transfer polls status. `IDENTIFY
//! DEVICE` both proves the drive exists and supplies the sector count the
//! registry advertises.
//!
//! [`probe`] returns the driver singleton for registration; filesystems read
//! through the [`BlockDevice`] trait, so the FAT volume reaches this driver
//! through whichever handle it mounted.

use super::{BlockDevice, BlockError, SECTOR_SIZE};
use crate::arch::io::{inb, insw_bytes, inw, outb};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

const DATA: u16 = 0x1F0;
const SECTORS: u16 = 0x1F2;
const LBA_LO: u16 = 0x1F3;
const LBA_MID: u16 = 0x1F4;
const LBA_HI: u16 = 0x1F5;
const DRIVE: u16 = 0x1F6;
const STATUS: u16 = 0x1F7;
/// Alternate status when read, device control when written.
const ALT_STATUS: u16 = 0x3F6;
const DEVICE_CONTROL: u16 = ALT_STATUS;
/// Device control: software reset of both drives on the channel.
const CONTROL_SRST: u8 = 0x04;
const COMMAND_IDENTIFY: u8 = 0xEC;
const COMMAND_READ: u8 = 0x20;

/// Serialises PIO sequences: the scheduler can have several tasks reading the
/// filesystem, and interleaving command bytes would corrupt a transfer.
static IO: Mutex<()> = Mutex::new(());

/// The primary master's size, discovered by `probe` with `IDENTIFY DEVICE`.
static SECTORS_ON_DISK: AtomicU64 = AtomicU64::new(0);

/// The driver singleton handed to the registry. Stateless: port numbers are
/// constants and the discovered size lives in [`SECTORS_ON_DISK`].
pub struct AtaPio;

static ATA: AtaPio = AtaPio;

/// Delay by reading the alternate status port `reads` times. Each read takes
/// at least ~100ns on real hardware (more under a hypervisor, where it is a VM
/// exit), so `reads` is a lower bound on the wait, never an upper one.
fn delay_alt_status_reads(reads: u32) {
    for _ in 0..reads {
        // Safety: reading the alternate status register has no side effect
        // the driver needs to guard against; it exists to be polled.
        let _: u8 = unsafe { inb(ALT_STATUS) };
    }
}

/// 400ns delay: four alternate-status reads.
fn delay_400ns() {
    delay_alt_status_reads(4);
}

/// Reads holding SRST asserted: at least 64 x ~100ns, past the 5us minimum.
const SRST_HOLD_READS: u32 = 64;
/// Reads after clearing SRST: at least 20,000 x ~100ns, past the 2ms the
/// spec gives the device before BSY may be trusted.
const SRST_SETTLE_READS: u32 = 20_000;

fn status() -> u8 {
    // Safety: reading the status register has no side effect; it exists to
    // be polled and this driver never treats it as read-to-clear.
    unsafe { inb(STATUS) }
}

fn wait_not_busy() -> bool {
    for _ in 0..1_000_000 {
        if status() & 0x80 == 0 {
            return true;
        }
    }
    false
}

fn wait_for_data() -> bool {
    // Once per sector: PIO runs with interrupts off (`input::ps2`).
    crate::input::ps2::service();
    for _ in 0..1_000_000 {
        let status = status();
        if status & 0x08 != 0 {
            return true;
        }
        if status & 0x01 != 0 {
            return false; // error
        }
    }
    false
}

/// Most sectors one `READ SECTORS` command carries. The count register is 8
/// bits (0 means 256); 128 keeps a run to 64 KiB and the maths obvious.
pub const MAX_RUN: usize = 128;

/// Times one run is issued before its read fails with [`BlockError::Io`]
/// when the transfer keeps being aborted (see [`RunError::Aborted`]).
pub const RUN_ATTEMPTS: usize = 4;

/// Runs re-issued after an aborted transfer, since boot.
static RETRIED_RUNS: AtomicU64 = AtomicU64::new(0);

/// Why a run failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunError {
    /// The device reported an error or never became ready.
    Device,
    /// The hypervisor aborted a `rep insw` mid-transfer (a spurious fault the
    /// kernel recovered, see `arch::string_io`). Port data may have been
    /// consumed, so the device is out of step: reset it and re-issue.
    Aborted,
}

/// Runs re-issued after an aborted transfer, since boot.
#[cfg(lazyos_tests)]
pub fn retried_runs() -> u64 {
    RETRIED_RUNS.load(Ordering::Relaxed)
}

/// Read one run, re-issuing it after an aborted transfer (up to
/// [`RUN_ATTEMPTS`] issues). Callers hold [`IO`].
fn read_run(lba: u32, buf: &mut [u8]) -> Result<(), BlockError> {
    for _ in 0..RUN_ATTEMPTS {
        match pio_read_run(lba, buf) {
            Ok(()) => return Ok(()),
            Err(RunError::Device) => return Err(BlockError::Io),
            Err(RunError::Aborted) => {
                RETRIED_RUNS.fetch_add(1, Ordering::Relaxed);
                if !reset_channel() {
                    return Err(BlockError::Io);
                }
            }
        }
    }
    Err(BlockError::Io)
}

/// Software-reset the channel, abandoning the command in flight and any
/// sector data still in the device's buffer. Returns false if the drive does
/// not come back ready.
fn reset_channel() -> bool {
    // Safety: SRST is the documented way to abort a command; the driver
    // re-selects the drive and reprograms every register for the next one.
    unsafe { outb(DEVICE_CONTROL, CONTROL_SRST) };
    delay_alt_status_reads(SRST_HOLD_READS);
    // Safety: as above; clearing SRST ends the reset (interrupts stay as the
    // driver found them: nIEN clear, and the driver polls regardless).
    unsafe { outb(DEVICE_CONTROL, 0) };
    delay_alt_status_reads(SRST_SETTLE_READS);
    wait_not_busy()
}

/// Read `buf.len() / 512` consecutive sectors (1..=[`MAX_RUN`]) with a single
/// `READ SECTORS` command. One command per run instead of per sector saves
/// the register setup and the 400ns delay, and the data goes straight into
/// `buf` with `rep insw` (one VM exit per string instruction under a
/// hypervisor rather than one per word). Callers hold [`IO`].
fn pio_read_run(lba: u32, buf: &mut [u8]) -> Result<(), RunError> {
    let count = buf.len() / SECTOR_SIZE;
    if count == 0 || count > MAX_RUN || !buf.len().is_multiple_of(SECTOR_SIZE) {
        return Err(RunError::Device);
    }
    // Safety: this is the documented ATA PIO read protocol, in order:
    // select the drive/LBA-high nibble, load the sector count and LBA, then
    // issue the read command. No register here is read-to-clear.
    unsafe {
        outb(DRIVE, 0xE0 | ((lba >> 24) & 0x0F) as u8);
    }
    delay_400ns();
    // Safety: same protocol contract as above.
    unsafe {
        outb(SECTORS, count as u8);
        outb(LBA_LO, lba as u8);
        outb(LBA_MID, (lba >> 8) as u8);
        outb(LBA_HI, (lba >> 16) as u8);
        outb(STATUS, COMMAND_READ);
    }

    for sector in buf.as_chunks_mut::<SECTOR_SIZE>().0 {
        // The device raises DRQ once per sector of the run.
        if !wait_not_busy() || !wait_for_data() {
            return Err(RunError::Device);
        }
        // Safety: the data port is read-many within one sector transfer;
        // `wait_for_data` above confirmed the device has a sector ready, and
        // `sector` is exactly the 512 bytes it will supply.
        unsafe { insw_bytes(DATA, &mut sector[..]) }.map_err(|_| RunError::Aborted)?;
    }
    Ok(())
}

/// Ask the primary master for its identity. Returns the sector count, or
/// `None` when no drive answers (QEMU's floating bus reads as status 0).
fn identify() -> Option<u64> {
    let _guard = IO.lock();
    // Select the master; a missing drive leaves the bus floating, which QEMU
    // reports as status 0, so the probe can bail out before the full timeout.
    // Safety: same ATA protocol contract as `pio_read_run`.
    unsafe {
        outb(DRIVE, 0xA0);
    }
    delay_400ns();
    if status() == 0 {
        return None;
    }
    // IDENTIFY takes no address and expects the count/LBA registers cleared.
    // Safety: same ATA protocol contract as `pio_read_run`.
    unsafe {
        outb(SECTORS, 0);
        outb(LBA_LO, 0);
        outb(LBA_MID, 0);
        outb(LBA_HI, 0);
        outb(STATUS, COMMAND_IDENTIFY);
    }
    if !wait_not_busy() || !wait_for_data() {
        return None;
    }

    let mut words = [0u16; 256];
    for word in words.iter_mut() {
        // Safety: the data port is read-many within one IDENTIFY transfer;
        // `wait_for_data` above confirmed the device has a word ready.
        *word = unsafe { inw(DATA) };
    }
    // Words 60/61: total addressable sectors in 28-bit LBA mode.
    let lba28 = (u64::from(words[61]) << 16) | u64::from(words[60]);
    if lba28 == 0 {
        None
    } else {
        Some(lba28)
    }
}

/// Probe the primary master and return the driver singleton for registration.
pub fn probe() -> Option<&'static dyn BlockDevice> {
    let sectors = identify()?;
    SECTORS_ON_DISK.store(sectors, Ordering::Relaxed);
    Some(&ATA)
}

impl BlockDevice for AtaPio {
    fn name(&self) -> &'static str {
        "ata0"
    }

    fn sector_count(&self) -> u64 {
        SECTORS_ON_DISK.load(Ordering::Relaxed)
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        self.check_range(lba, buf.len())?;
        // 28-bit LBA limit: refuse rather than truncate.
        let end = lba + (buf.len() / SECTOR_SIZE) as u64;
        if end > 1 << 28 {
            return Err(BlockError::Unsupported);
        }
        let _guard = IO.lock();
        for (index, run) in buf.chunks_mut(MAX_RUN * SECTOR_SIZE).enumerate() {
            let start = lba as u32 + (index * MAX_RUN) as u32;
            read_run(start, run)?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), BlockError> {
        // PIO writes are posted synchronously; there is no cache to flush.
        Ok(())
    }
}
