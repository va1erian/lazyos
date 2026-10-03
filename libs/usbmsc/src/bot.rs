//! Bulk-Only Transport (USB MSC BOT 1.0): the 31-byte Command Block Wrapper,
//! the 13-byte Command Status Wrapper, and one command's three stages with
//! the specification's error recovery (BOT 5.3, 6.6, 6.7).
//!
//! The host side of the rules this module applies:
//!
//! * A stalled **data** stage: Clear Feature HALT on that endpoint, then read
//!   the CSW as usual (BOT 6.7.2, 6.7.3).
//! * A stalled **status** stage: Clear Feature HALT on bulk-IN and read the
//!   CSW once more; a second failure is a Reset Recovery (BOT 5.3.3, Figure 2).
//! * A CSW that is not **valid** (13 bytes, `USBS`, the tag just sent) or not
//!   **meaningful** (status 0..=2, residue no larger than the transfer), a
//!   **phase error** (status 2) or a failed CBW: Reset Recovery, that is the
//!   Bulk-Only Mass Storage Reset class request followed by Clear Feature
//!   HALT on bulk-IN and then bulk-OUT (BOT 5.3.4).
//! * A device that went away ([`XferError::Gone`]) ends the command at once:
//!   there is nothing left to recover.

use crate::Error;

/// `dCBWSignature`, "USBC" little-endian.
pub const CBW_SIGNATURE: u32 = 0x4342_5355;
/// `dCSWSignature`, "USBS" little-endian.
pub const CSW_SIGNATURE: u32 = 0x5342_5355;
pub const CBW_LEN: usize = 31;
pub const CSW_LEN: usize = 13;
/// `bmCBWFlags` bit 7: data-in (device to host).
const FLAG_IN: u8 = 0x80;
/// The longest command block a CBW carries.
pub const MAX_CDB: usize = 16;

/// `bCSWStatus` values.
pub mod status {
    pub const PASSED: u8 = 0;
    pub const FAILED: u8 = 1;
    pub const PHASE_ERROR: u8 = 2;
}

/// A control request: the eight bytes of a setup packet, field by field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

/// Bulk-Only Mass Storage Reset (BOT 3.1): class, interface recipient.
pub fn mass_storage_reset(interface: u8) -> Setup {
    Setup {
        request_type: 0x21,
        request: 0xFF,
        value: 0,
        index: u16::from(interface),
        length: 0,
    }
}

/// Get Max LUN (BOT 3.2): one byte, the highest LUN. Devices with one LUN
/// may stall it, which means LUN 0.
pub fn get_max_lun(interface: u8) -> Setup {
    Setup {
        request_type: 0xA1,
        request: 0xFE,
        value: 0,
        index: u16::from(interface),
        length: 1,
    }
}

/// CLEAR_FEATURE(ENDPOINT_HALT) on `endpoint` (USB 2.0 9.4.1).
pub fn clear_halt(endpoint: u8) -> Setup {
    Setup {
        request_type: 0x02,
        request: 0x01,
        value: 0,
        index: u16::from(endpoint),
        length: 0,
    }
}

/// Encode a CBW. `cdb` is 1..=16 bytes (checked by [`Bot::command`]).
pub fn encode_cbw(tag: u32, length: u32, data_in: bool, lun: u8, cdb: &[u8]) -> [u8; CBW_LEN] {
    let mut cbw = [0u8; CBW_LEN];
    cbw[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
    cbw[4..8].copy_from_slice(&tag.to_le_bytes());
    cbw[8..12].copy_from_slice(&length.to_le_bytes());
    cbw[12] = if data_in { FLAG_IN } else { 0 };
    cbw[13] = lun & 0x0F;
    let len = cdb.len().min(MAX_CDB);
    cbw[14] = len as u8;
    cbw[15..15 + len].copy_from_slice(&cdb[..len]);
    cbw
}

/// A decoded CSW. Only [`Csw::check`] says whether it may be believed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Csw {
    pub signature: u32,
    pub tag: u32,
    pub residue: u32,
    pub status: u8,
}

/// Decode the 13 bytes of a CSW (anything else is [`Error::BadLength`]).
pub fn decode_csw(bytes: &[u8]) -> Result<Csw, Error> {
    if bytes.len() != CSW_LEN {
        return Err(if bytes.len() < CSW_LEN {
            Error::Short
        } else {
            Error::BadLength
        });
    }
    let word =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Ok(Csw {
        signature: word(0),
        tag: word(4),
        residue: word(8),
        status: bytes[12],
    })
}

impl Csw {
    /// BOT 6.3: valid (signature and tag) and meaningful (a known status and
    /// a residue no larger than what was asked for).
    pub fn check(&self, tag: u32, length: u32) -> Result<(), Error> {
        if self.signature != CSW_SIGNATURE || self.tag != tag {
            return Err(Error::Malformed);
        }
        if self.status > status::PHASE_ERROR || self.residue > length {
            return Err(Error::BadLength);
        }
        Ok(())
    }
}

/// A transfer failure as the controller reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XferError {
    /// The endpoint answered STALL (it is halted until cleared).
    Stall,
    /// The device is no longer there (detached, port disabled).
    Gone,
    /// Anything else: babble, transaction error, timeout.
    Failed,
}

/// The transport `usbd` gives the library: the two bulk pipes and the
/// control requests recovery needs.
pub trait Pipe {
    /// Send all of `data` on bulk-OUT; returns the bytes the device took.
    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, XferError>;
    /// Receive up to `buf.len()` bytes on bulk-IN; a short packet ends the
    /// transfer early. Returns the bytes received.
    fn bulk_in(&mut self, buf: &mut [u8]) -> Result<usize, XferError>;
    /// Issue a control request with no data stage ([`mass_storage_reset`],
    /// [`clear_halt`]).
    fn control(&mut self, setup: Setup) -> Result<(), XferError>;
    /// After CLEAR_FEATURE(ENDPOINT_HALT) on the device: reset the host's
    /// side of the bulk endpoint (`inbound` selects which): the controller's
    /// halted state, its ring position and the data toggle / sequence number.
    fn reset_host_endpoint(&mut self, inbound: bool) -> Result<(), XferError>;
    /// The bulk endpoint addresses and the interface number.
    fn endpoints(&self) -> (u8, u8, u8);
    /// Sleep about `ms` milliseconds (retry back-off).
    fn delay_ms(&mut self, ms: u32);
}

/// A command's data stage.
pub enum Data<'a> {
    None,
    In(&'a mut [u8]),
    Out(&'a [u8]),
}

impl Data<'_> {
    fn len(&self) -> usize {
        match self {
            Data::None => 0,
            Data::In(buf) => buf.len(),
            Data::Out(buf) => buf.len(),
        }
    }
}

/// How a command ended, when the device's CSW was believed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// `bCSWStatus` 0. `transferred` is what actually moved in the data
    /// stage; `residue` is the device's own account of the shortfall.
    Passed { transferred: usize, residue: u32 },
    /// `bCSWStatus` 1: the caller should REQUEST SENSE.
    Failed { residue: u32 },
}

/// Why a command did not complete. Recovery has already been attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotError {
    /// The command block is empty, longer than 16 bytes, or the data stage
    /// is larger than a CBW can describe.
    BadCommand,
    /// The device is gone.
    Gone,
    /// Phase error, invalid or meaningless CSW, or a failed transfer: the
    /// device was reset (Reset Recovery) and the command did not complete.
    Reset,
    /// Reset Recovery itself failed: the device is unusable.
    RecoveryFailed,
}

/// Counters of what recovery had to do (the driver logs them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub commands: u64,
    pub stalls: u64,
    pub resets: u64,
}

/// The transport state of one logical unit: the next tag and counters.
#[derive(Clone, Debug, Default)]
pub struct Bot {
    tag: u32,
    pub lun: u8,
    pub stats: Stats,
}

impl Bot {
    pub fn new(lun: u8) -> Bot {
        Bot {
            tag: 0,
            lun,
            stats: Stats::default(),
        }
    }

    /// Run one command: CBW, data stage, CSW, with BOT error recovery.
    pub fn command<P: Pipe>(
        &mut self,
        pipe: &mut P,
        cdb: &[u8],
        mut data: Data<'_>,
    ) -> Result<Status, BotError> {
        if cdb.is_empty() || cdb.len() > MAX_CDB {
            return Err(BotError::BadCommand);
        }
        let length = u32::try_from(data.len()).map_err(|_| BotError::BadCommand)?;
        self.tag = self.tag.wrapping_add(1);
        let tag = self.tag;
        self.stats.commands += 1;
        let data_in = matches!(data, Data::In(_));
        let cbw = encode_cbw(tag, length, data_in, self.lun, cdb);
        match pipe.bulk_out(&cbw) {
            Ok(CBW_LEN) => {}
            Err(XferError::Gone) => return Err(BotError::Gone),
            // A refused or short CBW: the device is out of step.
            _ => return self.fail_with_reset(pipe),
        }
        let transferred = match &mut data {
            Data::None => 0,
            Data::In(buf) => match pipe.bulk_in(buf) {
                Ok(n) => n.min(buf.len()),
                Err(error) => {
                    self.data_stage_failed(pipe, error, true)?;
                    0
                }
            },
            Data::Out(buf) => match pipe.bulk_out(buf) {
                Ok(n) => n.min(buf.len()),
                Err(error) => {
                    self.data_stage_failed(pipe, error, false)?;
                    0
                }
            },
        };
        let csw = self.read_csw(pipe)?;
        if csw.check(tag, length).is_err() {
            return self.fail_with_reset(pipe);
        }
        match csw.status {
            status::PASSED => Ok(Status::Passed {
                transferred,
                residue: csw.residue,
            }),
            status::FAILED => Ok(Status::Failed {
                residue: csw.residue,
            }),
            _ => self.fail_with_reset(pipe),
        }
    }

    /// A data stage that did not complete: a stall is cleared and the CSW
    /// still read; anything else resets the device.
    fn data_stage_failed<P: Pipe>(
        &mut self,
        pipe: &mut P,
        error: XferError,
        inbound: bool,
    ) -> Result<(), BotError> {
        match error {
            XferError::Gone => Err(BotError::Gone),
            XferError::Stall => {
                self.stats.stalls += 1;
                self.clear(pipe, inbound)
            }
            XferError::Failed => self.fail_with_reset(pipe),
        }
    }

    /// The status stage, retried once after a stall (BOT Figure 2).
    fn read_csw<P: Pipe>(&mut self, pipe: &mut P) -> Result<Csw, BotError> {
        let mut bytes = [0u8; CSW_LEN];
        for attempt in 0..2 {
            match pipe.bulk_in(&mut bytes) {
                Ok(n) => match decode_csw(&bytes[..n.min(CSW_LEN)]) {
                    Ok(csw) => return Ok(csw),
                    Err(_) => break,
                },
                Err(XferError::Gone) => return Err(BotError::Gone),
                Err(XferError::Stall) if attempt == 0 => {
                    self.stats.stalls += 1;
                    self.clear(pipe, true)?;
                }
                Err(_) => break,
            }
        }
        self.fail_with_reset(pipe)
    }

    /// Clear a halted bulk endpoint, device side first.
    fn clear<P: Pipe>(&mut self, pipe: &mut P, inbound: bool) -> Result<(), BotError> {
        let (bulk_in, bulk_out, _) = pipe.endpoints();
        let endpoint = if inbound { bulk_in } else { bulk_out };
        let result = pipe
            .control(clear_halt(endpoint))
            .and_then(|()| pipe.reset_host_endpoint(inbound));
        match result {
            Ok(()) => Ok(()),
            Err(XferError::Gone) => Err(BotError::Gone),
            Err(_) => self.fail_with_reset(pipe),
        }
    }

    /// Reset Recovery, then report the command as not completed.
    fn fail_with_reset<P: Pipe, T>(&mut self, pipe: &mut P) -> Result<T, BotError> {
        match reset_recovery(pipe) {
            Ok(()) => {
                self.stats.resets += 1;
                Err(BotError::Reset)
            }
            Err(XferError::Gone) => Err(BotError::Gone),
            Err(_) => Err(BotError::RecoveryFailed),
        }
    }
}

/// BOT 5.3.4 Reset Recovery: the class reset, then Clear Feature HALT on
/// bulk-IN and bulk-OUT (device and host side each).
pub fn reset_recovery<P: Pipe>(pipe: &mut P) -> Result<(), XferError> {
    let (bulk_in, bulk_out, interface) = pipe.endpoints();
    pipe.control(mass_storage_reset(interface))?;
    pipe.control(clear_halt(bulk_in))?;
    pipe.reset_host_endpoint(true)?;
    pipe.control(clear_halt(bulk_out))?;
    pipe.reset_host_endpoint(false)
}
