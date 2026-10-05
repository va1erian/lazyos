//! The submission queue entry (64 bytes) and completion queue entry
//! (16 bytes), NVMe 1.4 sections 4.2 and 4.6.

/// Admin command opcodes.
pub mod admin {
    pub const DELETE_SQ: u8 = 0x00;
    pub const CREATE_SQ: u8 = 0x01;
    pub const DELETE_CQ: u8 = 0x04;
    pub const CREATE_CQ: u8 = 0x05;
    pub const IDENTIFY: u8 = 0x06;
    pub const SET_FEATURES: u8 = 0x09;
}

/// NVM command set opcodes.
pub mod nvm {
    pub const FLUSH: u8 = 0x00;
    pub const WRITE: u8 = 0x01;
    pub const READ: u8 = 0x02;
}

/// Identify CNS values.
pub mod cns {
    pub const NAMESPACE: u32 = 0x00;
    pub const CONTROLLER: u32 = 0x01;
}

/// Set Features identifiers.
pub mod feature {
    pub const NUMBER_OF_QUEUES: u32 = 0x07;
}

/// Bytes in one submission entry.
pub const SQE_BYTES: usize = 64;
/// Bytes in one completion entry.
pub const CQE_BYTES: usize = 16;

/// One submission queue entry, field by field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Command {
    pub opcode: u8,
    pub cid: u16,
    pub nsid: u32,
    pub prp1: u64,
    pub prp2: u64,
    /// Command dwords 10 to 15.
    pub cdw: [u32; 6],
}

impl Command {
    /// The entry as the controller reads it (little endian; PSDT = 0, PRPs).
    pub fn encode(&self) -> [u8; SQE_BYTES] {
        let mut out = [0u8; SQE_BYTES];
        let dw0 = u32::from(self.opcode) | u32::from(self.cid) << 16;
        out[0..4].copy_from_slice(&dw0.to_le_bytes());
        out[4..8].copy_from_slice(&self.nsid.to_le_bytes());
        out[24..32].copy_from_slice(&self.prp1.to_le_bytes());
        out[32..40].copy_from_slice(&self.prp2.to_le_bytes());
        for (index, dword) in self.cdw.iter().enumerate() {
            let at = 40 + index * 4;
            out[at..at + 4].copy_from_slice(&dword.to_le_bytes());
        }
        out
    }

    /// Parse an entry (the model controller's side).
    pub fn decode(raw: &[u8; SQE_BYTES]) -> Command {
        let dw = |at: usize| u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
        let qw = |at: usize| u64::from(dw(at)) | u64::from(dw(at + 4)) << 32;
        let mut cdw = [0u32; 6];
        for (index, dword) in cdw.iter_mut().enumerate() {
            *dword = dw(40 + index * 4);
        }
        Command {
            opcode: raw[0],
            cid: (dw(0) >> 16) as u16,
            nsid: dw(4),
            prp1: qw(24),
            prp2: qw(32),
            cdw,
        }
    }

    /// Identify `cns` (for `nsid`) into the page at `prp1`.
    pub fn identify(cns: u32, nsid: u32, prp1: u64) -> Command {
        Command {
            opcode: admin::IDENTIFY,
            nsid,
            prp1,
            cdw: [cns, 0, 0, 0, 0, 0],
            ..Command::default()
        }
    }

    /// Create I/O Completion Queue `qid` of `entries` at `base`
    /// (physically contiguous, interrupts off).
    pub fn create_cq(qid: u16, entries: u16, base: u64) -> Command {
        Command {
            opcode: admin::CREATE_CQ,
            prp1: base,
            cdw: [
                (u32::from(entries) - 1) << 16 | u32::from(qid),
                1, // PC
                0,
                0,
                0,
                0,
            ],
            ..Command::default()
        }
    }

    /// Create I/O Submission Queue `qid` of `entries` at `base`, completing
    /// to queue `cqid` (physically contiguous, medium priority).
    pub fn create_sq(qid: u16, entries: u16, base: u64, cqid: u16) -> Command {
        Command {
            opcode: admin::CREATE_SQ,
            prp1: base,
            cdw: [
                (u32::from(entries) - 1) << 16 | u32::from(qid),
                u32::from(cqid) << 16 | 1, // PC
                0,
                0,
                0,
                0,
            ],
            ..Command::default()
        }
    }

    /// Set Features: Number of Queues, asking for `count` of each kind.
    pub fn set_queue_count(count: u16) -> Command {
        let n = u32::from(count) - 1;
        Command {
            opcode: admin::SET_FEATURES,
            cdw: [feature::NUMBER_OF_QUEUES, n << 16 | n, 0, 0, 0, 0],
            ..Command::default()
        }
    }

    /// Read or Write `blocks` blocks at `lba` of namespace `nsid`.
    pub fn io(opcode: u8, nsid: u32, lba: u64, blocks: u32, prp1: u64, prp2: u64) -> Command {
        Command {
            opcode,
            nsid,
            prp1,
            prp2,
            cdw: [lba as u32, (lba >> 32) as u32, blocks - 1, 0, 0, 0],
            ..Command::default()
        }
    }

    /// Flush namespace `nsid`'s volatile write cache.
    pub fn flush(nsid: u32) -> Command {
        Command {
            opcode: nvm::FLUSH,
            nsid,
            ..Command::default()
        }
    }
}

/// One completion queue entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Completion {
    /// Command-specific dword 0.
    pub result: u32,
    /// Where the controller's submission head is now.
    pub sq_head: u16,
    pub sq_id: u16,
    pub cid: u16,
    pub phase: bool,
    /// Status Code Type and Status Code; both zero is success.
    pub sct: u8,
    pub sc: u8,
    /// Do Not Retry.
    pub dnr: bool,
}

impl Completion {
    pub fn decode(raw: &[u8; CQE_BYTES]) -> Completion {
        let dw = |at: usize| u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
        let dw2 = dw(8);
        let dw3 = dw(12);
        let status = dw3 >> 17;
        Completion {
            result: dw(0),
            sq_head: dw2 as u16,
            sq_id: (dw2 >> 16) as u16,
            cid: dw3 as u16,
            phase: dw3 & (1 << 16) != 0,
            sc: status as u8,
            sct: ((status >> 8) & 0x7) as u8,
            dnr: status & (1 << 14) != 0,
        }
    }

    /// The entry as a controller writes it (the model controller's side).
    pub fn encode(&self) -> [u8; CQE_BYTES] {
        let mut out = [0u8; CQE_BYTES];
        out[0..4].copy_from_slice(&self.result.to_le_bytes());
        let dw2 = u32::from(self.sq_head) | u32::from(self.sq_id) << 16;
        out[8..12].copy_from_slice(&dw2.to_le_bytes());
        let status = u32::from(self.sc)
            | u32::from(self.sct & 0x7) << 8
            | if self.dnr { 1 << 14 } else { 0 };
        let dw3 = u32::from(self.cid) | u32::from(self.phase) << 16 | status << 17;
        out[12..16].copy_from_slice(&dw3.to_le_bytes());
        out
    }

    pub fn ok(&self) -> bool {
        self.sct == 0 && self.sc == 0
    }
}
