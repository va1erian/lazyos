//! The model controller's command set: the admin commands (Identify, queue
//! creation), the NVM I/O commands with their PRP walk, and the Identify
//! pages it serves.

use std::vec;
use std::vec::Vec;

use super::{Behavior, Cq, Sq, State};
use crate::cmd::{admin, cns, nvm, Command};
use crate::PAGE;

impl State {
    pub(super) fn execute_admin(&mut self, command: &Command) -> ((u8, u8), u32) {
        match command.opcode {
            admin::IDENTIFY => {
                let page = match command.cdw[0] {
                    cns::CONTROLLER => identify_controller(&self.behavior),
                    cns::NAMESPACE if command.nsid == 1 => {
                        identify_namespace(&self.behavior, self.disk.len())
                    }
                    cns::NAMESPACE => vec![0; 4096],
                    _ => return ((0, 0x02), 0), // invalid field
                };
                assert!(
                    command.prp1.is_multiple_of(PAGE),
                    "identify buffer unaligned"
                );
                self.write(command.prp1, &page);
                ((0, 0), 0)
            }
            admin::SET_FEATURES => ((0, 0), 0), // one queue of each granted
            admin::CREATE_CQ => {
                let qid = command.cdw[0] & 0xFFFF;
                let depth = (command.cdw[0] >> 16) as u16 + 1;
                assert_eq!(qid, 1, "only queue 1 expected");
                assert_eq!(command.cdw[1] & 1, 1, "CQ not physically contiguous");
                assert_eq!(command.cdw[1] & 2, 0, "CQ asked for interrupts");
                self.cqs[1] = Some(Cq {
                    base: command.prp1,
                    depth,
                    tail: 0,
                    head: 0,
                    phase: true,
                });
                ((0, 0), 0)
            }
            admin::CREATE_SQ => {
                let qid = command.cdw[0] & 0xFFFF;
                let depth = (command.cdw[0] >> 16) as u16 + 1;
                let cqid = (command.cdw[1] >> 16) as u16;
                assert_eq!(qid, 1, "only queue 1 expected");
                if self.cqs[usize::from(cqid)].is_none() {
                    return ((1, 0x00), 0); // completion queue invalid
                }
                self.sqs[1] = Some(Sq {
                    base: command.prp1,
                    depth,
                    head: 0,
                    cqid,
                });
                ((0, 0), 0)
            }
            _ => ((0, 0x01), 0), // invalid opcode
        }
    }

    pub(super) fn execute_io(&mut self, command: Command) {
        self.io_commands += 1;
        if self.behavior.stray_completions {
            self.complete(1, command.cid ^ 0x8000, (0, 0x06), 0);
        }
        let status = match command.opcode {
            nvm::FLUSH => {
                self.flushes += 1;
                (0, 0)
            }
            nvm::READ | nvm::WRITE => self.read_write(&command),
            _ => (0, 0x01),
        };
        self.complete(1, command.cid, status, 0);
    }

    fn read_write(&mut self, command: &Command) -> (u8, u8) {
        assert_eq!(command.nsid, 1);
        let block = 1usize << self.behavior.lba_shift.unwrap_or(9);
        let lba = u64::from(command.cdw[0]) | u64::from(command.cdw[1]) << 32;
        let blocks = (command.cdw[2] & 0xFFFF) as usize + 1;
        let bytes = blocks * block;
        let start = lba as usize * block;
        if start + bytes > self.disk.len() {
            return (0, 0x80); // LBA out of range
        }
        if let Some(bad) = self.behavior.fail_lba {
            if (lba..lba + blocks as u64).contains(&bad) {
                return (2, 0x81); // unrecovered read error
            }
        }
        let pieces = self.walk_prps(command.prp1, command.prp2, bytes);
        let mut at = start;
        for (phys, len) in pieces {
            if command.opcode == nvm::WRITE {
                let mut data = vec![0u8; len];
                self.read(phys, &mut data);
                self.disk[at..at + len].copy_from_slice(&data);
            } else {
                let data = self.disk[at..at + len].to_vec();
                self.write(phys, &data);
            }
            at += len;
        }
        (0, 0)
    }

    /// The data pieces of a PRP pair, checked against NVMe 1.4 section 4.3.
    fn walk_prps(&mut self, prp1: u64, prp2: u64, bytes: usize) -> Vec<(u64, usize)> {
        assert_eq!(prp1 % 4, 0, "PRP1 not dword aligned");
        let first = ((PAGE - prp1 % PAGE) as usize).min(bytes);
        let mut pieces = vec![(prp1, first)];
        let mut left = bytes - first;
        if left == 0 {
            return pieces;
        }
        if left <= PAGE as usize {
            assert_eq!(prp2 % PAGE, 0, "PRP2 entry not page aligned");
            pieces.push((prp2, left));
            return pieces;
        }
        assert_eq!(prp2 % 8, 0, "PRP list pointer not qword aligned");
        let mut at = prp2;
        while left > 0 {
            assert!(
                !at.is_multiple_of(PAGE) || at == prp2,
                "PRP list ran into the next page (no chaining)"
            );
            let mut raw = [0u8; 8];
            self.read(at, &mut raw);
            let entry = u64::from_le_bytes(raw);
            assert_eq!(entry % PAGE, 0, "PRP list entry not page aligned");
            let len = left.min(PAGE as usize);
            pieces.push((entry, len));
            left -= len;
            at += 8;
        }
        pieces
    }
}

fn put(page: &mut [u8], at: usize, bytes: &[u8]) {
    page[at..at + bytes.len()].copy_from_slice(bytes);
}

/// An Identify Controller page for `behavior`.
pub fn identify_controller(behavior: &Behavior) -> Vec<u8> {
    let mut page = vec![0u8; 4096];
    put(&mut page, 0, &0x1B36u16.to_le_bytes());
    put(&mut page, 4, b"lazyos-model        ");
    put(&mut page, 24, b"LazyOS model NVMe controller            ");
    put(&mut page, 64, b"1.0     ");
    page[77] = behavior.mdts;
    page[512] = 0x66;
    page[513] = 0x44;
    put(&mut page, 516, &1u32.to_le_bytes());
    page[525] = u8::from(behavior.vwc);
    page
}

/// An Identify Namespace page for a disk of `bytes` bytes.
pub fn identify_namespace(behavior: &Behavior, bytes: usize) -> Vec<u8> {
    let shift = behavior.lba_shift.unwrap_or(9);
    let blocks = (bytes >> shift) as u64;
    let mut page = vec![0u8; 4096];
    put(&mut page, 0, &blocks.to_le_bytes());
    put(&mut page, 8, &blocks.to_le_bytes());
    put(&mut page, 16, &blocks.to_le_bytes());
    page[25] = 0; // one format
    page[26] = 0; // format 0 in use
    put(&mut page, 128, &(u32::from(shift) << 16).to_le_bytes());
    page
}
