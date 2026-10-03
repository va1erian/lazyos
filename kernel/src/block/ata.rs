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

/// Status reads before a wait gives up. Each read is at least ~100ns (about
/// 1us on a real chipset), so a wait is bounded near a second.
pub const POLL_LIMIT: u32 = 1_000_000;

/// Status bits.
const STATUS_BSY: u8 = 0x80;
const STATUS_DRQ: u8 = 0x08;
const STATUS_ERR: u8 = 0x01;
/// What an undecoded port reads: no IDE controller answers at 0x1F0 (every
/// PC whose SATA runs in AHCI mode, and chipsets with no legacy IDE at all),
/// so the pulled-up bus floats high. BSY is set in it, so a naive "wait until
/// not busy" would spin out its whole bound on every probe (issue #449).
pub const FLOATING_BUS: u8 = 0xFF;

/// Whether a status read right after selecting the master means nobody is
/// there: a floating bus (`0xFF`, real hardware) or a channel with no drive
/// (`0`, QEMU).
pub fn status_means_absent(status: u8) -> bool {
    status == FLOATING_BUS || status == 0
}

/// The primary channel's registers, as the probe and the waits use them. The
/// real implementation is [`Ports`]; the kernel suite drives the same code
/// with fakes (a floating bus, a drive stuck busy) to prove every path ends.
pub trait Channel {
    /// Read the status register.
    fn status(&mut self) -> u8;
    /// Wait the 400ns a drive needs after a select or command.
    fn delay_400ns(&mut self);
    /// Select the master drive.
    fn select_master(&mut self);
    /// Clear the address registers and issue `IDENTIFY DEVICE`.
    fn issue_identify(&mut self);
    /// Read one data word.
    fn read_data(&mut self) -> u16;
}

/// The real primary channel at 0x1F0/0x3F6.
pub struct Ports;

impl Channel for Ports {
    fn status(&mut self) -> u8 {
        // Safety: reading the status register has no side effect; it exists to
        // be polled and this driver never treats it as read-to-clear.
        unsafe { inb(STATUS) }
    }

    fn delay_400ns(&mut self) {
        delay_400ns();
    }

    fn select_master(&mut self) {
        // Safety: same ATA protocol contract as `pio_read_run`.
        unsafe { outb(DRIVE, 0xA0) };
    }

    fn issue_identify(&mut self) {
        // IDENTIFY takes no address and expects the count/LBA registers cleared.
        // Safety: same ATA protocol contract as `pio_read_run`.
        unsafe {
            outb(SECTORS, 0);
            outb(LBA_LO, 0);
            outb(LBA_MID, 0);
            outb(LBA_HI, 0);
            outb(STATUS, COMMAND_IDENTIFY);
        }
    }

    fn read_data(&mut self) -> u16 {
        // Safety: the data port is read-many within one transfer; callers only
        // read after `wait_for_data_on` confirmed a word is ready.
        unsafe { inw(DATA) }
    }
}

/// Poll until BSY clears. False on timeout, and at once on a floating bus.
pub fn wait_not_busy_on(channel: &mut impl Channel) -> bool {
    for _ in 0..POLL_LIMIT {
        let status = channel.status();
        if status == FLOATING_BUS {
            return false;
        }
        if status & STATUS_BSY == 0 {
            return true;
        }
    }
    false
}

/// Poll until DRQ is set. False on timeout, on an error, or on a floating
/// bus. ERR and DRQ are undefined while BSY is set, so they are read only
/// once it clears.
pub fn wait_for_data_on(channel: &mut impl Channel) -> bool {
    for _ in 0..POLL_LIMIT {
        let status = channel.status();
        if status == FLOATING_BUS {
            return false;
        }
        if status & STATUS_BSY != 0 {
            continue;
        }
        if status & STATUS_ERR != 0 {
            return false;
        }
        if status & STATUS_DRQ != 0 {
            return true;
        }
    }
    false
}

fn wait_not_busy() -> bool {
    wait_not_busy_on(&mut Ports)
}

fn wait_for_data() -> bool {
    // Once per sector: PIO runs with interrupts off (`input::ps2`).
    crate::input::ps2::service();
    wait_for_data_on(&mut Ports)
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
/// `None` when no drive answers.
fn identify() -> Option<u64> {
    let _guard = IO.lock();
    identify_on(&mut Ports)
}

/// [`identify`] over any [`Channel`]. Absence is decided from the first
/// status read after the select, before any wait: a floating bus (`0xFF`, no
/// IDE controller) or an empty channel (`0`, QEMU) returns at once. Every
/// later wait is bounded by [`POLL_LIMIT`] reads.
pub fn identify_on(channel: &mut impl Channel) -> Option<u64> {
    channel.select_master();
    channel.delay_400ns();
    if status_means_absent(channel.status()) {
        return None;
    }
    channel.issue_identify();
    channel.delay_400ns();
    // A device that vanished between the two reads (or a bus that only
    // floats once driven) reads absent here too.
    if status_means_absent(channel.status()) {
        return None;
    }
    if !wait_not_busy_on(channel) || !wait_for_data_on(channel) {
        return None;
    }
    let mut words = [0u16; 256];
    for word in words.iter_mut() {
        *word = channel.read_data();
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
