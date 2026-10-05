//! A submission queue and its completion queue, polled (NVMe 1.4 section
//! 4.1).
//!
//! The host owns the submission tail and the completion head; the
//! controller owns the rest. A completion entry is new when its phase bit
//! matches the phase the host expects, which flips each time the head wraps.

use crate::cmd::{Command, Completion, CQE_BYTES, SQE_BYTES};
use crate::{regs, Platform};

/// One queue pair's host-side state.
#[derive(Clone, Copy, Debug)]
pub struct QueuePair {
    pub qid: u16,
    /// Entries in each queue (the same for both).
    pub depth: u16,
    sq: u64,
    cq: u64,
    tail: u16,
    head: u16,
    phase: bool,
    sq_doorbell: usize,
    cq_doorbell: usize,
}

impl QueuePair {
    /// A fresh pair at physical `sq` and `cq` (both zeroed by the caller).
    pub fn new(qid: u16, depth: u16, sq: u64, cq: u64, stride: usize) -> QueuePair {
        QueuePair {
            qid,
            depth,
            sq,
            cq,
            tail: 0,
            head: 0,
            phase: true,
            sq_doorbell: regs::sq_doorbell(qid, stride),
            cq_doorbell: regs::cq_doorbell(qid, stride),
        }
    }

    pub fn sq_base(&self) -> u64 {
        self.sq
    }

    pub fn cq_base(&self) -> u64 {
        self.cq
    }

    /// Write `command` at the tail and ring the doorbell. The caller keeps
    /// fewer than `depth` commands outstanding, so the queue is never full.
    pub fn submit(&mut self, platform: &dyn Platform, command: &Command) {
        let at = self.sq + u64::from(self.tail) * SQE_BYTES as u64;
        platform.write_mem(at, &command.encode());
        self.tail = (self.tail + 1) % self.depth;
        // The entry must be visible before the doorbell names it.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        platform.write32(self.sq_doorbell, u32::from(self.tail));
    }

    /// The entry at the head, if the controller has written it.
    pub fn peek(&self, platform: &dyn Platform) -> Option<Completion> {
        let at = self.cq + u64::from(self.head) * CQE_BYTES as u64;
        // The phase bit first: the rest of the entry is only meaningful once
        // it says the controller finished writing.
        let mut status = [0u8; 1];
        platform.read_mem(at + 14, &mut status);
        if (status[0] & 1 != 0) != self.phase {
            return None;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
        let mut raw = [0u8; CQE_BYTES];
        platform.read_mem(at, &mut raw);
        let entry = Completion::decode(&raw);
        (entry.phase == self.phase).then_some(entry)
    }

    /// Whether a new completion is waiting (for a waiter's poll).
    pub fn ready(&self, platform: &dyn Platform) -> bool {
        self.peek(platform).is_some()
    }

    /// Take the entry at the head and tell the controller.
    pub fn pop(&mut self, platform: &dyn Platform) -> Option<Completion> {
        let entry = self.peek(platform)?;
        self.head += 1;
        if self.head == self.depth {
            self.head = 0;
            self.phase = !self.phase;
        }
        platform.write32(self.cq_doorbell, u32::from(self.head));
        Some(entry)
    }
}
