//! Audit ring with a hash chain (issue #68).
//!
//! `docs/security-model.md` section 9 requires an always-on record of denials,
//! with optional traces of tagged transactions. Two properties make the ring
//! useful for forensics:
//!
//! * `Fixed size`: the kernel can never be made to allocate without bound by a
//!   caller's denial rate; the oldest event is overwritten and the newest is
//!   always retained.
//! * `Hash chain`: every record extends a running hash, so `auditd` can detect
//!   an event that was removed, reordered, or edited after the fact. (FNV-1a is
//!   enough for accidental-tamper evidence; `auditd` re-verifies with the same
//!   [`chain`] function.)
//!
//! The ring is kernel-side: no syscall writes it, and it is never cleared on a
//! running system (the `reset` hook below is test-harness only).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

/// How many events the ring retains.
pub const AUDIT_CAPACITY: usize = 128;

/// The hash the chain starts from (the FNV-1a 64 offset basis).
pub const GENESIS_HASH: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64 multiplier.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// One audit record: when, who (slot/uid/label), what (interface/method),
/// the decision, and the correlation id.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AuditEvent {
    /// PIT ticks at record time (see `task::ticks`).
    pub ticks: u64,
    /// Task slot of the actor.
    pub actor_slot: usize,
    /// Actor uid (kernel-stamped).
    pub uid: u32,
    /// Actor profile label (kernel-stamped).
    pub label_id: u32,
    /// Interface the call targeted.
    pub interface_id: u64,
    /// Method the call targeted.
    pub method: u32,
    /// Whether the call was allowed.
    pub allow: bool,
    /// Machine-readable reason (`acl::reason::*`).
    pub reason_code: u32,
    /// Transaction id for correlation; `0` when not a transaction.
    pub txn_id: u64,
}

/// The ring plus its chain head.
struct Ring {
    events: [Option<AuditEvent>; AUDIT_CAPACITY],
    /// Next write index (oldest once wrapped).
    next: usize,
    /// Events currently retained (grows to [`AUDIT_CAPACITY`], then stays).
    stored: usize,
    /// Total events recorded since boot (monotonic).
    total: u64,
    /// Total denials recorded since boot (monotonic; independent of the ring).
    denials: u64,
    /// Total allows recorded since boot (monotonic; present while tracing).
    allows: u64,
    /// Hash chain head over every event recorded since boot.
    hash: u64,
}

impl Ring {
    const fn new() -> Self {
        Ring {
            events: [None; AUDIT_CAPACITY],
            next: 0,
            stored: 0,
            total: 0,
            denials: 0,
            allows: 0,
            hash: GENESIS_HASH,
        }
    }

    fn push(&mut self, event: AuditEvent) {
        self.hash = chain(self.hash, &event);
        self.events[self.next] = Some(event);
        self.next = (self.next + 1) % AUDIT_CAPACITY;
        if self.stored < AUDIT_CAPACITY {
            self.stored += 1;
        }
        self.total = self.total.wrapping_add(1);
        if event.allow {
            self.allows = self.allows.wrapping_add(1);
        } else {
            self.denials = self.denials.wrapping_add(1);
        }
    }
}

static RING: Mutex<Ring> = Mutex::new(Ring::new());
/// Whether allowed calls are also recorded. Denials are always recorded.
static TRACE: AtomicBool = AtomicBool::new(false);

/// Extend the chain from `previous` with `event`. Public so `auditd` (and the
/// suite) can verify a record exactly as the kernel sealed it.
pub fn chain(previous: u64, event: &AuditEvent) -> u64 {
    let mut hash = previous;
    let fields: [[u8; 8]; 9] = [
        event.ticks.to_le_bytes(),
        (event.actor_slot as u64).to_le_bytes(),
        (event.uid as u64).to_le_bytes(),
        (event.label_id as u64).to_le_bytes(),
        event.interface_id.to_le_bytes(),
        (event.method as u64).to_le_bytes(),
        [event.allow as u8, 0, 0, 0, 0, 0, 0, 0],
        (event.reason_code as u64).to_le_bytes(),
        event.txn_id.to_le_bytes(),
    ];
    for field in fields {
        for byte in field {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

/// Append an event and extend the hash chain. `Kernel-only`: denials from
/// [`super::authorize`] and explicit trace records go here; userspace never
/// writes the ring directly.
pub fn record(event: AuditEvent) {
    RING.lock().push(event);
}

/// The most recent `n` events, newest first.
pub fn recent(n: usize) -> Vec<AuditEvent> {
    let ring = RING.lock();
    let take = n.min(ring.stored);
    let mut out = Vec::with_capacity(take);
    for i in 0..take {
        let index = (ring.next + AUDIT_CAPACITY - 1 - i) % AUDIT_CAPACITY;
        if let Some(event) = ring.events[index] {
            out.push(event);
        }
    }
    out
}

/// Number of events currently retained (bounded by [`AUDIT_CAPACITY`]).
pub fn count() -> usize {
    RING.lock().stored
}

/// Total events recorded since boot (monotonic, unbounded).
pub fn total() -> u64 {
    RING.lock().total
}

/// Total denials recorded since boot (monotonic, unbounded; unlike
/// [`count`], this survives ring wraparound).
pub fn denials() -> u64 {
    RING.lock().denials
}

/// Total allows recorded since boot (monotonic, unbounded). Only advances
/// while tracing is on ([`set_trace`]).
pub fn allows() -> u64 {
    RING.lock().allows
}

/// The hash chain head. The next record chains from here.
pub fn last_hash() -> u64 {
    RING.lock().hash
}

/// Record allowed calls as well as denials (`docs/security-model.md` section 9
/// lists traced transactions as optional). Off by default to keep the ring
/// cheap; `messengerd` turns it on for tagged transactions.
pub fn set_trace(enabled: bool) {
    TRACE.store(enabled, Ordering::Relaxed);
}

/// Whether allowed calls are being recorded.
pub fn trace() -> bool {
    TRACE.load(Ordering::Relaxed)
}

/// Clear the ring and restart the chain. `Test-harness only`: a running system
/// must never be able to erase the audit trail (`auditd` relies on it).
#[cfg(lazyos_tests)]
pub fn reset() {
    *RING.lock() = Ring::new();
}
