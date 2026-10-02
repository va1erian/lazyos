//! Fuzz entry points, shared by the seeded tests below and the cargo-fuzz
//! targets (`fuzz/fuzz_targets/mscdesc.rs`, `mscreply.rs`, `mscsession.rs`),
//! so a crash found by one replays under the other.
//!
//! * [`run_desc`]: any bytes as a configuration chain. An accepted BOT
//!   interface must have a bulk-IN and a bulk-OUT endpoint within limits.
//! * [`run_reply`]: any bytes as each device reply parser (CSW, INQUIRY,
//!   sense, capacity, mode sense). Accepted values must be self-consistent.
//! * [`run_session`]: a whole hostile device. Every transfer's outcome and
//!   every byte the device returns comes from the script; the disk is brought
//!   up and read and written. Nothing may panic, every loop must end (the
//!   pipe counts its calls), and a read that succeeds filled its buffer.

use std::vec;

use crate::bot::{self, Pipe, Setup, XferError};
use crate::desc::{find_bot, MAX_BULK_PACKET, MAX_BURST};
use crate::disk::{Disk, ATTEMPTS, READY_ATTEMPTS};
use crate::scsi;

/// Parse `data` as a configuration chain; panics on an inconsistent result.
pub fn run_desc(data: &[u8]) {
    let Ok(Some(found)) = find_bot(data) else {
        return;
    };
    for (endpoint, inbound) in [(found.bulk_in, true), (found.bulk_out, false)] {
        assert_eq!(endpoint.is_in(), inbound, "direction");
        assert!((1..=15).contains(&endpoint.number()), "endpoint number");
        assert!((8..=MAX_BULK_PACKET).contains(&endpoint.max_packet));
        assert!(endpoint.max_burst <= MAX_BURST);
    }
}

/// Feed `data` (after a selector byte) to every reply parser.
pub fn run_reply(data: &[u8]) {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    if let Ok(csw) = bot::decode_csw(rest) {
        let tag = u32::from(selector);
        if csw.check(tag, 512).is_ok() {
            assert_eq!(csw.signature, bot::CSW_SIGNATURE);
            assert_eq!(csw.tag, tag);
            assert!(csw.residue <= 512 && csw.status <= 2);
        }
    }
    if let Ok(inquiry) = scsi::parse_inquiry(rest) {
        let printable = |b: &u8| (0x20..0x7F).contains(b);
        assert!(inquiry.vendor.iter().all(printable));
        assert!(inquiry.product.iter().all(printable));
    }
    if let Ok(sense) = scsi::parse_sense(rest) {
        assert!(sense.key <= 0x0F);
        let _ = sense.action();
    }
    for capacity in [
        scsi::parse_capacity_10(rest).ok().flatten(),
        scsi::parse_capacity_16(rest).ok(),
    ]
    .into_iter()
    .flatten()
    {
        assert!(capacity.blocks > 0);
        assert!(matches!(capacity.block_len, 512 | 1024 | 2048 | 4096));
        assert!(capacity
            .blocks
            .checked_mul(u64::from(capacity.block_len))
            .is_some());
    }
    let _ = scsi::parse_write_protect(rest);
}

/// A device that answers from a byte script.
struct Script<'a> {
    data: &'a [u8],
    at: usize,
    calls: u64,
    /// The last CBW's tag and operation code, so a "plausible" answer can
    /// echo the tag and fit the command.
    tag: u32,
    opcode: u8,
}

impl Script<'_> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    /// An outcome: `None` once the script is spent (the device is gone).
    fn outcome(&mut self) -> Result<u8, XferError> {
        self.calls += 1;
        let selector = self.byte().ok_or(XferError::Gone)?;
        match selector % 16 {
            0 => Err(XferError::Stall),
            1 => Err(XferError::Failed),
            2 if selector & 0x80 != 0 => Err(XferError::Gone),
            _ => Ok(selector),
        }
    }
}

impl Pipe for Script<'_> {
    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, XferError> {
        if data.len() == bot::CBW_LEN && data[0..4] == bot::CBW_SIGNATURE.to_le_bytes() {
            self.tag = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
            self.opcode = data[15];
        }
        let selector = self.outcome()?;
        // Mostly the whole buffer, sometimes a short or an absurd count.
        Ok(match selector % 5 {
            0 => data.len() / 2,
            1 => usize::MAX,
            _ => data.len(),
        })
    }

    fn bulk_in(&mut self, buf: &mut [u8]) -> Result<usize, XferError> {
        let selector = self.outcome()?;
        // Half the answers are plausible (a CSW echoing the tag, data that
        // fits the command) so the script gets past the transport into the
        // disk layer; the rest are raw script bytes.
        if selector & 0x40 != 0 {
            return Ok(self.plausible(buf, selector));
        }
        let len = usize::from(self.byte().unwrap_or(0));
        let n = len.min(buf.len());
        for slot in buf[..n].iter_mut() {
            *slot = self.byte().unwrap_or(0);
        }
        Ok(n)
    }

    fn control(&mut self, _: Setup) -> Result<(), XferError> {
        self.outcome().map(|_| ())
    }

    fn reset_host_endpoint(&mut self, _: bool) -> Result<(), XferError> {
        Ok(())
    }

    fn endpoints(&self) -> (u8, u8, u8) {
        (0x81, 0x02, 0)
    }

    fn delay_ms(&mut self, _: u32) {}
}

impl Script<'_> {
    fn plausible(&mut self, buf: &mut [u8], selector: u8) -> usize {
        buf.fill(0);
        if buf.len() == bot::CSW_LEN {
            buf[0..4].copy_from_slice(&bot::CSW_SIGNATURE.to_le_bytes());
            buf[4..8].copy_from_slice(&self.tag.to_le_bytes());
            // Mostly passed, sometimes failed or a phase error.
            buf[12] = [0, 0, 1, 2][usize::from(selector >> 4) & 3];
            return buf.len();
        }
        match self.opcode {
            scsi::op::READ_CAPACITY_10 if buf.len() >= 8 => {
                let last = u32::from(self.byte().unwrap_or(7)) + 1;
                buf[0..4].copy_from_slice(&last.to_be_bytes());
                buf[4..8].copy_from_slice(&512u32.to_be_bytes());
            }
            scsi::op::REQUEST_SENSE if buf.len() >= 14 => {
                buf[0] = 0x70;
                buf[2] = self.byte().unwrap_or(0) & 0x0F;
                buf[12] = self.byte().unwrap_or(0);
            }
            _ => {
                for slot in buf.iter_mut().take(64) {
                    *slot = self.byte().unwrap_or(0);
                }
                if self.opcode == scsi::op::INQUIRY {
                    buf[0] &= 0x0E;
                }
            }
        }
        buf.len()
    }
}

/// Bring a scripted device up and use it; panics on a broken invariant.
pub fn run_session(data: &[u8]) {
    let mut pipe = Script {
        data,
        at: 0,
        calls: 0,
        tag: 0,
        opcode: 0,
    };
    // Bring-up is bounded: every command retries at most `ATTEMPTS` times
    // with a request sense and a recovery each, TEST UNIT READY at most
    // `READY_ATTEMPTS` times.
    let bound = u64::from(READY_ATTEMPTS + 8 * ATTEMPTS) * 16;
    let Ok(mut disk) = Disk::bring_up(&mut pipe, 0) else {
        assert!(pipe.calls <= bound, "bring-up did not end: {}", pipe.calls);
        return;
    };
    assert!(disk.capacity.blocks > 0);
    let block = disk.block_len();
    let mut buf = vec![0xEEu8; 2 * block];
    let lba = disk.capacity.blocks.saturating_sub(2);
    let _ = disk.read(&mut pipe, lba, &mut buf);
    let _ = disk.write(&mut pipe, lba, &buf);
    let _ = disk.flush(&mut pipe);
    assert!(
        disk.read(&mut pipe, disk.capacity.blocks, &mut buf)
            .is_err(),
        "a read past the end succeeded"
    );
    assert!(
        pipe.calls <= 4 * bound,
        "session did not end: {}",
        pipe.calls
    );
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::{for_seeds, Rng};
    use std::vec::Vec;

    fn replay(target: &str, run: fn(&[u8])) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for {target}");
        }
    }

    #[test]
    fn checked_in_seeds_replay() {
        replay("mscdesc", run_desc);
        replay("mscreply", run_reply);
        replay("mscsession", run_session);
    }

    #[test]
    fn mutated_descriptors() {
        let golden: [&[u8]; 3] = [
            &crate::tests::golden::HS_CONFIG,
            &crate::tests::golden::SS_CONFIG,
            &crate::tests::golden::COMPOSITE_CONFIG,
        ];
        for_seeds("usbmsc::fuzz::mutated_descriptors", |_, rng: &mut Rng| {
            let mut data = golden[rng.below(3) as usize].to_vec();
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(data.len() as u64) as usize;
                data[at] = rng.byte();
            }
            let cut = rng.below(data.len() as u64 + 8) as usize;
            data.resize(cut, rng.byte());
            run_desc(&data);
        });
    }

    #[test]
    fn random_replies() {
        for_seeds("usbmsc::fuzz::random_replies", |_, rng: &mut Rng| {
            let len = rng.below(40) as usize;
            let data: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
            run_reply(&data);
        });
    }

    /// How far the random sessions get: some must reach a ready disk, or the
    /// generator is not exercising the disk layer at all.
    #[test]
    fn random_sessions_reach_the_disk_layer() {
        let mut ready = 0;
        for seed in 0..512u64 {
            let mut rng = Rng::new(seed);
            let data: Vec<u8> = (0..2000).map(|_| rng.byte() | 0x40).collect();
            let mut pipe = Script {
                data: &data,
                at: 0,
                calls: 0,
                tag: 0,
                opcode: 0,
            };
            if Disk::bring_up(&mut pipe, 0).is_ok() {
                ready += 1;
            }
        }
        assert!(ready > 0, "no random session brought a disk up");
    }

    #[test]
    fn random_sessions() {
        for_seeds("usbmsc::fuzz::random_sessions", |_, rng: &mut Rng| {
            let len = rng.below(4000) as usize;
            let data: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
            run_session(&data);
        });
    }
}
