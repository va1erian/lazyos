//! The task-context half of interrupt delivery, and the shared-INTx contract
//! (issue #240, driver-plan section 3.3).
//!
//! `dev::irq::dispatch` only masks the line and records that it fired.
//! [`service`] runs from task context (every syscall entry and the mux loop),
//! where it may lock and allocate, and does the rest:
//!
//! * A raised line is delivered to **every armed claimant** as one one-way
//!   message from the kernel identity. The line stays masked until every
//!   claimant that was sent a message has called `irq_ack`.
//! * Each claim keeps its own `pending` bit, so the one-outstanding limit is per
//!   (claim, line): a claim that has not acked is never sent a second message,
//!   only marked `missed`. Its queue depth therefore cannot exceed one no matter
//!   how many interrupts fire.
//! * A claimant that does not ack within [`ACK_DEADLINE_TICKS`] is dropped from
//!   that round: the line is unmasked for the others, the laggard is audited and
//!   stays "owed" (still `pending`). A late ack makes it eligible again and, if
//!   it `missed` an interrupt meanwhile, immediately posts one fresh message. A
//!   late ack never unmasks the line for anyone else: that already happened.
//!
//! A hung driver on a level-triggered line still re-asserts: the line is
//! unmasked when a round ends and the next interrupt masks it again, so the cost
//! is bounded by how often [`service`] runs, not by the interrupt rate.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};

use libmessenger::{flags, Encoder, Header, Parcel, VERSION};

use crate::arch::pic;
use crate::ipc::channels::{self, Error as ChannelError};

use super::claims::{Claim, Claims, CLAIMS, LINES};
use super::class::{method, DEV_INTERFACE};
use super::errno::{Errno, EBUSY, EINVAL, ENOSYS};
use super::irq;
use super::table::MAX_DEVICES;
use super::{report, DeviceId};

/// Ticks (100 Hz) a claimant has to ack before it is dropped from a round.
pub const ACK_DEADLINE_TICKS: u64 = 100;

/// Bit per line that has a delivery round in flight.
static ACTIVE_ROUNDS: AtomicU16 = AtomicU16::new(0);
/// A claim has an interrupt to (re)post without a new raise.
static RETRY: AtomicBool = AtomicBool::new(false);

static DELIVERED: AtomicU64 = AtomicU64::new(0);
static TIMEOUTS: AtomicU64 = AtomicU64::new(0);

/// Messages posted and ack deadlines missed since boot.
pub fn counters() -> (u64, u64) {
    (
        DELIVERED.load(Ordering::Relaxed),
        TIMEOUTS.load(Ordering::Relaxed),
    )
}

/// One notification to send once the claim lock is released.
#[derive(Clone, Copy)]
struct Post {
    dev: u16,
    generation: u32,
    channel: u64,
    side: usize,
}

/// Work collected under the lock and performed after it is dropped.
struct Batch {
    posts: [Option<Post>; MAX_DEVICES],
    post_count: usize,
    expired: [Option<(DeviceId, usize)>; MAX_DEVICES],
    expired_count: usize,
}

impl Batch {
    const fn new() -> Batch {
        Batch {
            posts: [None; MAX_DEVICES],
            post_count: 0,
            expired: [None; MAX_DEVICES],
            expired_count: 0,
        }
    }

    fn post(&mut self, dev: usize, claim: &Claim) {
        let Some(binding) = claim.irq else { return };
        if self.post_count < self.posts.len() {
            self.posts[self.post_count] = Some(Post {
                dev: dev as u16,
                generation: claim.generation,
                channel: binding.channel,
                side: binding.side,
            });
            self.post_count += 1;
        }
    }

    fn expire(&mut self, dev: usize, owner: usize) {
        if self.expired_count < self.expired.len() {
            self.expired[self.expired_count] = Some((DeviceId(dev as u16), owner));
            self.expired_count += 1;
        }
    }
}

impl Claims {
    /// Bitmask of claims armed on `line`.
    fn armed_mask(&self, line: u8) -> u32 {
        let mut mask = 0;
        for (id, claim) in self.slots.iter().enumerate() {
            if claim.is_some_and(|claim| claim.armed && claim.line == Some(line)) {
                mask |= 1 << id;
            }
        }
        mask
    }

    fn set_round(&mut self, line: u8, waiting: u32, deadline: u64) {
        self.rounds[usize::from(line)] = super::claims::Round { waiting, deadline };
        if waiting != 0 {
            ACTIVE_ROUNDS.fetch_or(1 << line, Ordering::AcqRel);
        } else {
            ACTIVE_ROUNDS.fetch_and(!(1 << line), Ordering::AcqRel);
        }
    }

    /// Re-establish the line's PIC state after a change: with nobody armed it
    /// is masked and its round dropped; with a listener and nobody left to
    /// wait for it is unmasked (the round is over). A round still waiting on a
    /// claimant leaves the line masked.
    fn settle(&mut self, line: u8) {
        if self.armed_mask(line) == 0 {
            self.set_round(line, 0, 0);
            pic::set_masked(line, true);
        } else if self.rounds[usize::from(line)].waiting == 0 {
            self.set_round(line, 0, 0);
            pic::set_masked(line, false);
        }
    }

    /// Drop every claimant whose round deadline has passed, unmask its line.
    fn expire(&mut self, now: u64, batch: &mut Batch) {
        for line in 0..LINES as u8 {
            let round = self.rounds[usize::from(line)];
            if round.waiting == 0 || now < round.deadline {
                continue;
            }
            for (id, claim) in self.slots.iter().enumerate() {
                if let (true, Some(claim)) = (round.waiting & (1 << id) != 0, claim) {
                    batch.expire(id, claim.owner);
                }
            }
            self.set_round(line, 0, 0);
            self.settle(line);
        }
    }

    /// Handle a raised `line`: notify every armed claimant that is not owed an
    /// ack, mark the owed ones `missed`, and start a round.
    fn raise(&mut self, line: u8, now: u64, batch: &mut Batch) {
        let armed = self.armed_mask(line);
        if armed == 0 {
            irq::note_stray();
            pic::set_masked(line, true);
            return;
        }
        let mut notified = 0u32;
        for (id, claim) in self.slots.iter_mut().enumerate() {
            if armed & (1 << id) == 0 {
                continue;
            }
            let Some(claim) = claim.as_mut() else {
                continue;
            };
            if claim.pending {
                claim.missed = true;
            } else {
                claim.pending = true;
                claim.missed = false;
                notified |= 1 << id;
                let snapshot = *claim;
                batch.post(id, &snapshot);
            }
        }
        // The line must be masked while anyone owes us an ack.
        pic::set_masked(line, true);
        let in_flight = self.rounds[usize::from(line)].waiting;
        if notified != 0 {
            self.set_round(
                line,
                in_flight | notified,
                now.saturating_add(ACK_DEADLINE_TICKS),
            );
        } else if in_flight == 0 {
            // Everyone is a laggard: nobody can be waited for, so let the line
            // go; `missed` remembers the interrupt for their late acks.
            pic::set_masked(line, false);
        }
    }

    /// Queue the fresh message a `missed` claim is owed once it has acked.
    fn collect_retries(&mut self, batch: &mut Batch) {
        for (id, claim) in self.slots.iter_mut().enumerate() {
            let Some(claim) = claim.as_mut() else {
                continue;
            };
            if claim.armed && claim.missed && !claim.pending && claim.irq.is_some() {
                claim.pending = true;
                claim.missed = false;
                let snapshot = *claim;
                batch.post(id, &snapshot);
            }
        }
    }

    /// The posting of `post` failed: the claimant was not notified.
    fn post_failed(&mut self, post: &Post, error: ChannelError) {
        let id = DeviceId(post.dev);
        let Some(claim) = self.get_mut(id) else {
            return;
        };
        if claim.generation != post.generation {
            return;
        }
        claim.pending = false;
        let line = claim.line;
        if error == ChannelError::QueueFull {
            // The driver's inbox is full of client traffic: try again on the
            // next pass instead of losing the interrupt.
            claim.missed = true;
            RETRY.store(true, Ordering::Release);
        } else {
            // The endpoint is gone; there is nobody to notify.
            claim.armed = false;
            claim.missed = false;
        }
        if let Some(line) = line {
            let mut round = self.rounds[usize::from(line)];
            round.waiting &= !(1 << post.dev);
            self.set_round(line, round.waiting, round.deadline);
            self.settle(line);
        }
    }

    /// Take `id`'s claim out of interrupt delivery without removing it: it no
    /// longer listens, owes an ack, or holds a round open, and its line masks
    /// if nobody else is armed on it.
    pub(super) fn silence(&mut self, id: DeviceId) {
        let Some(claim) = self.get_mut(id) else {
            return;
        };
        claim.armed = false;
        claim.pending = false;
        claim.missed = false;
        let line = claim.line;
        if let Some(line) = line {
            let mut round = self.rounds[usize::from(line)];
            round.waiting &= !(1 << id.0);
            self.set_round(line, round.waiting, round.deadline);
            self.settle(line);
        }
    }

    /// Remove `id`'s claim, taking it out of interrupt delivery first.
    pub(super) fn detach(&mut self, id: DeviceId) -> Option<Claim> {
        let claim = self.slots.get_mut(usize::from(id.0))?.take()?;
        if let Some(line) = claim.line {
            let mut round = self.rounds[usize::from(line)];
            round.waiting &= !(1 << id.0);
            self.set_round(line, round.waiting, round.deadline);
            self.settle(line);
        }
        Some(claim)
    }
}

/// Whether a userspace claimant is armed on `line` (kernel handlers may not
/// take such a line).
pub fn line_in_use(line: u8) -> bool {
    CLAIMS.lock().armed_mask(line) != 0
}

/// Arm the claim for `id` (generation already checked by the caller): it joins
/// delivery rounds, and the line is unmasked unless a round is in flight.
pub fn arm(id: DeviceId) -> Result<(), Errno> {
    let mut claims = CLAIMS.lock();
    let claim = claims.get(id).copied().ok_or(EINVAL)?;
    if claim.irq.is_none() {
        return Err(EINVAL);
    }
    let Some(line) = claim.line else {
        // Not PIC-routable: the driver polls.
        return Err(ENOSYS);
    };
    if irq::has_kernel_handler(line) {
        return Err(EBUSY);
    }
    if claim.armed {
        return Ok(());
    }
    if let Some(entry) = claims.get_mut(id) {
        entry.armed = true;
    }
    if claims.rounds[usize::from(line)].waiting == 0 {
        pic::set_masked(line, false);
    }
    Ok(())
}

/// Acknowledge the outstanding interrupt of `id`'s claim.
///
/// Returns `EINVAL` if nothing is owed. The line unmasks when this was the last
/// claimant a round waited for; a late ack from a claimant that was already
/// dropped from its round never unmasks it, and a claim that `missed` an
/// interrupt in the meantime is posted a fresh message at once.
pub fn ack(id: DeviceId) -> Result<(), Errno> {
    let retry = {
        let mut claims = CLAIMS.lock();
        let claim = claims.get_mut(id).ok_or(EINVAL)?;
        if !claim.pending {
            return Err(EINVAL);
        }
        claim.pending = false;
        let (line, missed) = (claim.line, claim.missed);
        if let Some(line) = line {
            let mut round = claims.rounds[usize::from(line)];
            if round.waiting & (1 << id.0) != 0 {
                round.waiting &= !(1 << id.0);
                claims.set_round(line, round.waiting, round.deadline);
                claims.settle(line);
            }
        }
        missed
    };
    if retry {
        RETRY.store(true, Ordering::Release);
        service();
    }
    Ok(())
}

/// Deliver pending interrupts and expire ack deadlines, using the tick clock.
pub fn service() {
    service_at(crate::task::ticks());
}

/// [`service`] with an explicit clock, so tests can step time.
pub fn service_at(now: u64) {
    if super::teardown::exits_pending() {
        super::teardown::silence_exited();
    }
    let raised = irq::take_raised();
    if raised == 0 && ACTIVE_ROUNDS.load(Ordering::Acquire) == 0 && !RETRY.load(Ordering::Acquire) {
        return;
    }
    x86_64::instructions::interrupts::without_interrupts(|| run(now, raised));
}

fn run(now: u64, raised: u16) {
    let mut batch = Batch::new();
    RETRY.store(false, Ordering::Release);
    {
        let mut claims = CLAIMS.lock();
        claims.expire(now, &mut batch);
        for line in 0..LINES as u8 {
            if raised & (1 << line) != 0 {
                claims.raise(line, now, &mut batch);
            }
        }
        claims.collect_retries(&mut batch);
    }
    for entry in batch.expired.iter().take(batch.expired_count).flatten() {
        record_timeout(entry.0, entry.1);
    }
    for post in batch.posts.iter().take(batch.post_count).flatten() {
        match post_irq(post) {
            Ok(()) => {
                DELIVERED.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => CLAIMS.lock().post_failed(post, error),
        }
    }
}

fn record_timeout(id: DeviceId, owner: usize) {
    TIMEOUTS.fetch_add(1, Ordering::Relaxed);
    let info = super::table().lock().get(id);
    if let Some(info) = info {
        report::record(
            owner,
            &info,
            method::IRQ_TIMEOUT,
            false,
            report::reason::IRQ_TIMEOUT,
        );
    }
}

/// The wire form of an interrupt notification.
fn encode_irq(dev: u16, generation: u32) -> Option<Vec<u8>> {
    let mut body = Encoder::new();
    body.u32(1, u32::from(dev)).ok()?;
    body.u32(2, 0).ok()?;
    body.u32(3, generation).ok()?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: DEV_INTERFACE,
            method: method::IRQ,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).ok()?;
    Some(bytes)
}

fn post_irq(post: &Post) -> Result<(), ChannelError> {
    let bytes = encode_irq(post.dev, post.generation).ok_or(ChannelError::BadParcel)?;
    channels::post_from_kernel(post.channel, post.side, &bytes)
}

/// Test-only: forget every claim's delivery state and all rounds.
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    ACTIVE_ROUNDS.store(0, Ordering::Release);
    RETRY.store(false, Ordering::Release);
    let _ = irq::take_raised();
}
