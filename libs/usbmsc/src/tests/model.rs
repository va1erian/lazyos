//! A model Bulk-Only SCSI disk behind the [`Pipe`] trait, with fault
//! injection, so the transport and the disk layer run against something that
//! behaves (and misbehaves) like a stick.
//!
//! The model is strict about the host: a transfer on a halted endpoint
//! stalls, a CBW sent while it expects data or status is refused, and it only
//! forgets a bad state on a Bulk-Only Mass Storage Reset. That is what makes
//! the recovery tests mean something.

use std::collections::BTreeMap;
use std::vec::Vec;

use crate::bot::{self, Pipe, Setup, XferError, CBW_LEN, CBW_SIGNATURE, CSW_SIGNATURE};
use crate::scsi::op;

pub const BULK_IN: u8 = 0x81;
pub const BULK_OUT: u8 = 0x02;
pub const BLOCK: usize = 512;

/// What the model expects next.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
    Cbw,
    DataIn(Vec<u8>),
    DataOut {
        lba: u64,
        len: usize,
    },
    Csw,
    /// An invalid CBW: everything stalls until a reset.
    NeedReset,
}

/// Faults, each consumed when it fires.
#[derive(Clone, Debug, Default)]
pub struct Faults {
    pub stall_data_in: u32,
    pub stall_data_out: u32,
    pub stall_csw: u32,
    pub bad_signature: u32,
    pub wrong_tag: u32,
    pub phase_error: u32,
    pub big_residue: u32,
    pub unit_attention: u32,
    pub not_ready: u32,
    pub fail_transfer: u32,
    pub short_read: u32,
    pub no_medium: bool,
    pub write_protect: bool,
    pub unsupported_sync: bool,
    pub refuse_mode_sense: bool,
    pub gone: bool,
}

pub struct Model {
    pub blocks: u64,
    pub data: BTreeMap<u64, Vec<u8>>,
    pub faults: Faults,
    phase: Phase,
    tag: u32,
    residue: u32,
    status: u8,
    sense: (u8, u8, u8),
    halted_in: bool,
    halted_out: bool,
    pub resets: u32,
    pub clears: u32,
    pub host_resets: u32,
    pub delays: u32,
    pub syncs: u32,
    pub opcodes: Vec<u8>,
}

impl Model {
    pub fn new(blocks: u64) -> Model {
        Model {
            blocks,
            data: BTreeMap::new(),
            faults: Faults::default(),
            phase: Phase::Cbw,
            tag: 0,
            residue: 0,
            status: 0,
            sense: (0, 0, 0),
            halted_in: false,
            halted_out: false,
            resets: 0,
            clears: 0,
            host_resets: 0,
            delays: 0,
            syncs: 0,
            opcodes: Vec::new(),
        }
    }

    pub fn block(&self, lba: u64) -> Vec<u8> {
        self.data
            .get(&lba)
            .cloned()
            .unwrap_or_else(|| std::vec![0; BLOCK])
    }

    fn take(counter: &mut u32) -> bool {
        if *counter > 0 {
            *counter -= 1;
            true
        } else {
            false
        }
    }

    fn fail(&mut self, sense: (u8, u8, u8)) {
        self.sense = sense;
        self.status = bot::status::FAILED;
        self.phase = Phase::Csw;
    }

    fn pass_in(&mut self, data: Vec<u8>, host_len: usize) {
        let mut data = data;
        data.truncate(host_len);
        self.residue = (host_len - data.len()) as u32;
        self.status = bot::status::PASSED;
        self.phase = if host_len == 0 {
            Phase::Csw
        } else {
            Phase::DataIn(data)
        };
    }

    /// A CBW arrived: decide the response.
    fn command(&mut self, cbw: &[u8]) {
        let tag = u32::from_le_bytes(cbw[4..8].try_into().unwrap());
        let host_len = u32::from_le_bytes(cbw[8..12].try_into().unwrap()) as usize;
        let cdb = &cbw[15..15 + usize::from(cbw[14]).min(16)];
        self.tag = tag;
        self.residue = 0;
        self.opcodes.push(cdb[0]);
        match cdb[0] {
            op::TEST_UNIT_READY => {
                self.status = bot::status::PASSED;
                self.phase = Phase::Csw;
                if self.faults.no_medium {
                    self.fail((2, 0x3A, 0));
                } else if Self::take(&mut self.faults.unit_attention) {
                    self.fail((6, 0x28, 0));
                } else if Self::take(&mut self.faults.not_ready) {
                    self.fail((2, 0x04, 0x01));
                }
            }
            op::REQUEST_SENSE => {
                let mut sense = std::vec![0u8; 18];
                sense[0] = 0x70;
                sense[2] = self.sense.0;
                sense[7] = 10;
                sense[12] = self.sense.1;
                sense[13] = self.sense.2;
                self.sense = (0, 0, 0);
                self.pass_in(sense, host_len);
            }
            op::INQUIRY => {
                let mut data = std::vec![0u8; 36];
                data[1] = 0x80;
                data[4] = 31;
                data[8..16].copy_from_slice(b"LAZYOS  ");
                data[16..32].copy_from_slice(b"MODEL STICK     ");
                self.pass_in(data, host_len);
            }
            op::READ_CAPACITY_10 => {
                let last = u32::try_from(self.blocks - 1).unwrap_or(u32::MAX);
                let mut data = last.to_be_bytes().to_vec();
                data.extend_from_slice(&(BLOCK as u32).to_be_bytes());
                self.pass_in(data, host_len);
            }
            op::SERVICE_ACTION_IN_16 => {
                let mut data = std::vec![0u8; 32];
                data[0..8].copy_from_slice(&(self.blocks - 1).to_be_bytes());
                data[8..12].copy_from_slice(&(BLOCK as u32).to_be_bytes());
                self.pass_in(data, host_len);
            }
            op::MODE_SENSE_6 if self.faults.refuse_mode_sense => self.fail((5, 0x24, 0)),
            op::MODE_SENSE_6 => {
                let wp = if self.faults.write_protect { 0x80 } else { 0 };
                self.pass_in(std::vec![3, 0, wp, 0], host_len);
            }
            op::READ_10 | op::READ_16 | op::WRITE_10 | op::WRITE_16 => self.rw(cdb, host_len),
            op::SYNCHRONIZE_CACHE_10 if self.faults.unsupported_sync => self.fail((5, 0x20, 0)),
            op::SYNCHRONIZE_CACHE_10 => {
                self.syncs += 1;
                self.status = bot::status::PASSED;
                self.phase = Phase::Csw;
            }
            _ => self.fail((5, 0x20, 0)),
        }
    }

    fn rw(&mut self, cdb: &[u8], host_len: usize) {
        let (lba, count) = if matches!(cdb[0], op::READ_10 | op::WRITE_10) {
            (
                u64::from(u32::from_be_bytes(cdb[2..6].try_into().unwrap())),
                u64::from(u16::from_be_bytes(cdb[7..9].try_into().unwrap())),
            )
        } else {
            (
                u64::from_be_bytes(cdb[2..10].try_into().unwrap()),
                u64::from(u32::from_be_bytes(cdb[10..14].try_into().unwrap())),
            )
        };
        let write = matches!(cdb[0], op::WRITE_10 | op::WRITE_16);
        if lba + count > self.blocks || count as usize * BLOCK != host_len {
            self.fail((5, 0x21, 0));
            return;
        }
        if Self::take(&mut self.faults.fail_transfer) {
            self.fail((3, 0x11, 0));
            return;
        }
        if write {
            self.status = bot::status::PASSED;
            self.phase = Phase::DataOut { lba, len: host_len };
            return;
        }
        let mut data = Vec::new();
        for block in lba..lba + count {
            data.extend_from_slice(&self.block(block));
        }
        if Self::take(&mut self.faults.short_read) {
            data.truncate(data.len() / 2);
        }
        self.pass_in(data, host_len);
    }

    fn csw(&mut self) -> [u8; 13] {
        let mut csw = [0u8; 13];
        let mut signature = CSW_SIGNATURE;
        if Self::take(&mut self.faults.bad_signature) {
            signature ^= 1;
        }
        let mut tag = self.tag;
        if Self::take(&mut self.faults.wrong_tag) {
            tag = tag.wrapping_add(7);
        }
        let mut status = self.status;
        if Self::take(&mut self.faults.phase_error) {
            status = bot::status::PHASE_ERROR;
        }
        let mut residue = self.residue;
        if Self::take(&mut self.faults.big_residue) {
            residue = u32::MAX;
        }
        csw[0..4].copy_from_slice(&signature.to_le_bytes());
        csw[4..8].copy_from_slice(&tag.to_le_bytes());
        csw[8..12].copy_from_slice(&residue.to_le_bytes());
        csw[12] = status;
        csw
    }
}

impl Pipe for Model {
    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, XferError> {
        if self.faults.gone {
            return Err(XferError::Gone);
        }
        if self.halted_out {
            return Err(XferError::Stall);
        }
        match self.phase.clone() {
            Phase::Cbw => {
                let valid = data.len() == CBW_LEN
                    && u32::from_le_bytes(data[0..4].try_into().unwrap()) == CBW_SIGNATURE
                    && (1..=16).contains(&data[14]);
                if !valid {
                    self.phase = Phase::NeedReset;
                    self.halted_in = true;
                    self.halted_out = true;
                    return Err(XferError::Stall);
                }
                self.command(data);
                Ok(CBW_LEN)
            }
            Phase::DataOut { lba, len } => {
                if Self::take(&mut self.faults.stall_data_out) {
                    self.halted_out = true;
                    self.residue = len as u32;
                    self.phase = Phase::Csw;
                    return Err(XferError::Stall);
                }
                let got = data.len().min(len);
                self.residue = (len - got) as u32;
                if self.faults.write_protect {
                    self.fail((7, 0x27, 0));
                    return Ok(got);
                }
                for (index, block) in data[..got].chunks(BLOCK).enumerate() {
                    self.data.insert(lba + index as u64, block.to_vec());
                }
                self.phase = Phase::Csw;
                Ok(got)
            }
            // Out of step: refuse until the host resets.
            _ => {
                self.phase = Phase::NeedReset;
                self.halted_in = true;
                self.halted_out = true;
                Err(XferError::Stall)
            }
        }
    }

    fn bulk_in(&mut self, buf: &mut [u8]) -> Result<usize, XferError> {
        if self.faults.gone {
            return Err(XferError::Gone);
        }
        if self.halted_in {
            return Err(XferError::Stall);
        }
        match self.phase.clone() {
            Phase::DataIn(data) => {
                if Self::take(&mut self.faults.stall_data_in) {
                    self.halted_in = true;
                    self.residue = buf.len() as u32;
                    self.phase = Phase::Csw;
                    return Err(XferError::Stall);
                }
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                self.phase = Phase::Csw;
                Ok(n)
            }
            Phase::Csw => {
                if Self::take(&mut self.faults.stall_csw) {
                    self.halted_in = true;
                    return Err(XferError::Stall);
                }
                let csw = self.csw();
                let n = buf.len().min(csw.len());
                buf[..n].copy_from_slice(&csw[..n]);
                self.phase = Phase::Cbw;
                Ok(n)
            }
            _ => Err(XferError::Failed),
        }
    }

    fn control(&mut self, setup: Setup) -> Result<(), XferError> {
        if self.faults.gone {
            return Err(XferError::Gone);
        }
        if setup == bot::mass_storage_reset(0) {
            self.resets += 1;
            self.phase = Phase::Cbw;
            return Ok(());
        }
        if setup == bot::clear_halt(BULK_IN) {
            self.clears += 1;
            self.halted_in = false;
            return Ok(());
        }
        if setup == bot::clear_halt(BULK_OUT) {
            self.clears += 1;
            self.halted_out = false;
            return Ok(());
        }
        Err(XferError::Stall)
    }

    fn reset_host_endpoint(&mut self, _inbound: bool) -> Result<(), XferError> {
        self.host_resets += 1;
        Ok(())
    }

    fn endpoints(&self) -> (u8, u8, u8) {
        (BULK_IN, BULK_OUT, 0)
    }

    fn delay_ms(&mut self, _ms: u32) {
        self.delays += 1;
    }
}
