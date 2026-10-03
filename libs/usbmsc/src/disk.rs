//! A SCSI direct-access logical unit over Bulk-Only Transport: bring-up, block
//! reads and writes split to the transfer limit, and the cache flush.
//!
//! Bring-up is the sequence every host uses on a stick: INQUIRY (a direct
//! access device must answer), TEST UNIT READY until the unit is ready (a
//! UNIT ATTENTION after power-on or reset is retried at once, NOT READY
//! "becoming ready" after a back-off, MEDIUM NOT PRESENT is final), READ
//! CAPACITY(10) and READ CAPACITY(16) when the medium is over 2 TiB, then
//! MODE SENSE(6) for the write-protect bit (a device that refuses it is taken
//! as writable). Every loop has a fixed bound.

use crate::bot::{Bot, BotError, Data, Pipe, Status};
use crate::scsi::{self, Action, Capacity, Inquiry};

/// The most bytes one READ or WRITE moves (the driver's DMA buffer).
pub const MAX_TRANSFER: usize = 64 * 1024;
/// TEST UNIT READY attempts during bring-up, [`READY_WAIT_MS`] apart when
/// the unit is becoming ready: 10 s in all, enough for a slow stick.
pub const READY_ATTEMPTS: u32 = 100;
pub const READY_WAIT_MS: u32 = 100;
/// Attempts per command when the device asks for a retry or was reset.
pub const ATTEMPTS: u32 = 4;

/// Why the logical unit cannot be used, or a transfer failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskError {
    /// The device went away.
    Gone,
    /// Not a direct-access block device (or no device at this LUN).
    NotDisk,
    /// No medium, or it never became ready.
    NoMedium,
    /// The capacity data was malformed.
    BadCapacity,
    /// A write to a write-protected medium.
    WriteProtected,
    /// The range is outside the medium or not whole blocks.
    Range,
    /// The device does not support the command (ILLEGAL REQUEST).
    Unsupported,
    /// The transport could not be recovered.
    Dead,
    /// The command failed after its retries.
    Io,
}

impl From<BotError> for DiskError {
    fn from(error: BotError) -> DiskError {
        match error {
            BotError::Gone => DiskError::Gone,
            BotError::RecoveryFailed => DiskError::Dead,
            BotError::BadCommand | BotError::Reset => DiskError::Io,
        }
    }
}

/// A data stage borrowed for one attempt.
enum Buf<'a> {
    None,
    In(&'a mut [u8]),
    Out(&'a [u8]),
}

impl Buf<'_> {
    fn len(&self) -> usize {
        match self {
            Buf::None => 0,
            Buf::In(buf) => buf.len(),
            Buf::Out(buf) => buf.len(),
        }
    }
}

/// The outcome of one attempt.
enum Attempt {
    Done,
    Again(Action),
}

/// A ready logical unit.
#[derive(Clone, Debug)]
pub struct Disk {
    pub bot: Bot,
    pub inquiry: Inquiry,
    pub capacity: Capacity,
    pub write_protected: bool,
}

impl Disk {
    /// Bring up LUN `lun`.
    pub fn bring_up<P: Pipe>(pipe: &mut P, lun: u8) -> Result<Disk, DiskError> {
        let mut bot = Bot::new(lun);
        let mut data = [0u8; scsi::INQUIRY_LEN as usize];
        let got = command(
            &mut bot,
            pipe,
            scsi::inquiry().as_bytes(),
            Buf::In(&mut data),
        )?;
        let inquiry = scsi::parse_inquiry(&data[..got]).map_err(|_| DiskError::NotDisk)?;
        // 0x00 is SBC; 0x0E (simplified direct access) speaks the same set.
        if inquiry.qualifier != 0 || !matches!(inquiry.device_type, 0x00 | 0x0E) {
            return Err(DiskError::NotDisk);
        }
        wait_ready(&mut bot, pipe)?;
        let capacity = read_capacity(&mut bot, pipe)?;
        let mut mode = [0u8; scsi::MODE_SENSE_LEN as usize];
        let write_protected = match command(
            &mut bot,
            pipe,
            scsi::mode_sense_6().as_bytes(),
            Buf::In(&mut mode),
        ) {
            Ok(got) => scsi::parse_write_protect(&mode[..got]).unwrap_or(false),
            Err(DiskError::Gone) => return Err(DiskError::Gone),
            Err(DiskError::Dead) => return Err(DiskError::Dead),
            Err(_) => false,
        };
        Ok(Disk {
            bot,
            inquiry,
            capacity,
            write_protected,
        })
    }

    /// Bytes per block.
    pub fn block_len(&self) -> usize {
        self.capacity.block_len as usize
    }

    /// Read whole blocks at `lba` into `buf`.
    pub fn read<P: Pipe>(
        &mut self,
        pipe: &mut P,
        lba: u64,
        buf: &mut [u8],
    ) -> Result<(), DiskError> {
        let blocks = self.check(lba, buf.len())?;
        let mut done = 0u64;
        for chunk in buf.chunks_mut(MAX_TRANSFER) {
            let count = (chunk.len() / self.block_len()) as u32;
            let cdb = scsi::rw(false, lba + done, count).ok_or(DiskError::Range)?;
            let got = command(&mut self.bot, pipe, cdb.as_bytes(), Buf::In(chunk))?;
            if got != chunk.len() {
                return Err(DiskError::Io);
            }
            done += u64::from(count);
        }
        debug_assert_eq!(done, blocks);
        Ok(())
    }

    /// Write whole blocks at `lba` from `buf`.
    pub fn write<P: Pipe>(&mut self, pipe: &mut P, lba: u64, buf: &[u8]) -> Result<(), DiskError> {
        self.check(lba, buf.len())?;
        if self.write_protected {
            return Err(DiskError::WriteProtected);
        }
        let mut done = 0u64;
        for chunk in buf.chunks(MAX_TRANSFER) {
            let count = (chunk.len() / self.block_len()) as u32;
            let cdb = scsi::rw(true, lba + done, count).ok_or(DiskError::Range)?;
            let got = command(&mut self.bot, pipe, cdb.as_bytes(), Buf::Out(chunk));
            match got {
                Ok(got) if got == chunk.len() => {}
                Ok(_) => return Err(DiskError::Io),
                Err(error) => return Err(error),
            }
            done += u64::from(count);
        }
        Ok(())
    }

    /// SYNCHRONIZE CACHE: everything written so far is on the medium. A
    /// device without a cache command (ILLEGAL REQUEST) has nothing to flush.
    pub fn flush<P: Pipe>(&mut self, pipe: &mut P) -> Result<(), DiskError> {
        match command(
            &mut self.bot,
            pipe,
            scsi::synchronize_cache().as_bytes(),
            Buf::None,
        ) {
            Ok(_) | Err(DiskError::Unsupported) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Whole blocks inside the medium; returns the block count.
    fn check(&self, lba: u64, bytes: usize) -> Result<u64, DiskError> {
        let block = self.block_len();
        if bytes == 0 || !bytes.is_multiple_of(block) {
            return Err(DiskError::Range);
        }
        let blocks = (bytes / block) as u64;
        match lba.checked_add(blocks) {
            Some(end) if end <= self.capacity.blocks => Ok(blocks),
            _ => Err(DiskError::Range),
        }
    }
}

/// TEST UNIT READY until the unit is ready (bounded).
fn wait_ready<P: Pipe>(bot: &mut Bot, pipe: &mut P) -> Result<(), DiskError> {
    for _ in 0..READY_ATTEMPTS {
        match attempt(
            bot,
            pipe,
            scsi::test_unit_ready().as_bytes(),
            &mut Buf::None,
        )? {
            (Attempt::Done, _) => return Ok(()),
            (Attempt::Again(Action::Retry), _) => {}
            (Attempt::Again(Action::Wait), _) => pipe.delay_ms(READY_WAIT_MS),
            (Attempt::Again(Action::NoMedium), _) => return Err(DiskError::NoMedium),
            (Attempt::Again(_), _) => return Err(DiskError::Io),
        }
    }
    Err(DiskError::NoMedium)
}

/// READ CAPACITY(10), then (16) when the medium is larger than 2 TiB.
fn read_capacity<P: Pipe>(bot: &mut Bot, pipe: &mut P) -> Result<Capacity, DiskError> {
    let mut data = [0u8; scsi::CAPACITY_10_LEN];
    let got = command(
        bot,
        pipe,
        scsi::read_capacity_10().as_bytes(),
        Buf::In(&mut data),
    )?;
    match scsi::parse_capacity_10(&data[..got]) {
        Ok(Some(capacity)) => return Ok(capacity),
        Ok(None) => {}
        Err(_) => return Err(DiskError::BadCapacity),
    }
    let mut data = [0u8; scsi::CAPACITY_16_LEN as usize];
    let got = command(
        bot,
        pipe,
        scsi::read_capacity_16().as_bytes(),
        Buf::In(&mut data),
    )?;
    scsi::parse_capacity_16(&data[..got]).map_err(|_| DiskError::BadCapacity)
}

/// Run a command until it passes, retrying what the sense data says may be
/// retried, at most [`ATTEMPTS`] times. Returns the bytes moved. A command
/// the device does not support ends as [`DiskError::Unsupported`] (the caller
/// decides whether that matters), a write-protected medium as
/// [`DiskError::WriteProtected`].
fn command<P: Pipe>(
    bot: &mut Bot,
    pipe: &mut P,
    cdb: &[u8],
    mut buf: Buf<'_>,
) -> Result<usize, DiskError> {
    for _ in 0..ATTEMPTS {
        match attempt(bot, pipe, cdb, &mut buf)? {
            (Attempt::Done, moved) => return Ok(moved),
            (Attempt::Again(Action::Retry), _) => {}
            (Attempt::Again(Action::Wait), _) => pipe.delay_ms(READY_WAIT_MS),
            (Attempt::Again(Action::NoMedium), _) => return Err(DiskError::NoMedium),
            (Attempt::Again(Action::WriteProtected), _) => return Err(DiskError::WriteProtected),
            (Attempt::Again(Action::Unsupported), _) => return Err(DiskError::Unsupported),
            (Attempt::Again(Action::Fail), _) => return Err(DiskError::Io),
        }
    }
    Err(DiskError::Io)
}

/// One attempt: the command, and REQUEST SENSE when it failed. A transport
/// reset (the command did not complete) asks for a retry.
fn attempt<P: Pipe>(
    bot: &mut Bot,
    pipe: &mut P,
    cdb: &[u8],
    buf: &mut Buf<'_>,
) -> Result<(Attempt, usize), DiskError> {
    let len = buf.len();
    let data = match buf {
        Buf::None => Data::None,
        Buf::In(b) => Data::In(b),
        Buf::Out(b) => Data::Out(b),
    };
    match bot.command(pipe, cdb, data) {
        Ok(Status::Passed {
            transferred,
            residue,
        }) => {
            // The device's residue and what actually moved must agree on
            // a short transfer; trust the smaller of the two accounts.
            let claimed = len.saturating_sub(residue as usize);
            Ok((Attempt::Done, transferred.min(claimed)))
        }
        Ok(Status::Failed { .. }) => Ok((Attempt::Again(sense(bot, pipe)?), 0)),
        Err(BotError::Reset) => Ok((Attempt::Again(Action::Retry), 0)),
        Err(error) => Err(error.into()),
    }
}

/// REQUEST SENSE and what it means; unreadable sense is a plain failure.
fn sense<P: Pipe>(bot: &mut Bot, pipe: &mut P) -> Result<Action, DiskError> {
    let mut data = [0u8; scsi::SENSE_LEN as usize];
    match bot.command(pipe, scsi::request_sense().as_bytes(), Data::In(&mut data)) {
        Ok(Status::Passed {
            transferred,
            residue,
        }) => {
            let got = transferred.min(data.len().saturating_sub(residue as usize));
            Ok(scsi::parse_sense(&data[..got]).map_or(Action::Fail, |sense| sense.action()))
        }
        Ok(Status::Failed { .. }) | Err(BotError::Reset) => Ok(Action::Fail),
        Err(error) => Err(error.into()),
    }
}
