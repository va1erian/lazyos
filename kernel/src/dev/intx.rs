//! The task-context half of interrupt delivery, and the shared-INTx contract
//! (issue #240, driver-plan section 3.3).
//!
//! `dev::irq::dispatch` only masks the line and records that it fired.
//! [`service`] does the rest, where it may lock and allocate: on the way out
//! of the line's own interrupt when that interrupt stopped user code or a
//! halted task (`task::interrupted_quiet_context`), on every syscall (native
//! entry, Linux return), on a tick that lands in such code, and in the mux
//! loop:
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
//!
//! Rounds run per delivery *source*: the sixteen legacy lines, then one per
//! MSI vector (`dev::msi`, issue #616). A vector belongs to one claim, so its
//! rounds never wait on anyone else, and it is masked per vector; everything
//! else (the one-outstanding limit, deadlines, late acks) is the same code.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::irqchip;
use crate::ipc::channels::Error as ChannelError;

use super::claims::{Claim, ClaimMask, Claims, CLAIMS, SOURCES};
use super::errno::{Errno, EBUSY, EINVAL, ENOSYS};
use super::irq;
use super::msi::{self, Mode};
use super::table::MAX_DEVICES;
use super::{notify, DeviceId, DeviceInfo};

/// Ticks (100 Hz) a claimant has to ack before it is dropped from a round.
pub const ACK_DEADLINE_TICKS: u64 = 100;

/// Bit per source that has a delivery round in flight.
static ACTIVE_ROUNDS: AtomicU64 = AtomicU64::new(0);
/// A claim has an interrupt to (re)post without a new raise.
static RETRY: AtomicBool = AtomicBool::new(false);

static DELIVERED: AtomicU64 = AtomicU64::new(0);
pub(super) static TIMEOUTS: AtomicU64 = AtomicU64::new(0);

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
    /// The claiming task (latency accounting, `perf::irq_posted`).
    owner: usize,
    /// The claim's MSI vector, if it is on one (`DEV:MSI:PASS`).
    msi: Option<u8>,
}

/// Work collected under the lock and performed after it is dropped.
struct Batch {
    posts: [Option<Post>; MAX_DEVICES],
    post_count: usize,
    expired: [Option<(DeviceId, usize)>; MAX_DEVICES],
    expired_count: usize,
    /// Running inside an interrupt handler ([`service_in_interrupt`]).
    in_interrupt: bool,
}

impl Batch {
    const fn new(in_interrupt: bool) -> Batch {
        Batch {
            posts: [None; MAX_DEVICES],
            post_count: 0,
            expired: [None; MAX_DEVICES],
            expired_count: 0,
            in_interrupt,
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
                owner: claim.owner,
                msi: claim.msi,
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
    /// Bitmask of claims armed on `line` (a delivery source).
    fn armed_mask(&self, line: u8) -> ClaimMask {
        let mut mask = 0;
        for (id, claim) in self.slots.iter().enumerate() {
            if claim.is_some_and(|claim| claim.armed && claim.source() == Some(line)) {
                mask |= 1 << id;
            }
        }
        mask
    }

    fn set_round(&mut self, line: u8, waiting: ClaimMask, deadline: u64) {
        self.rounds[usize::from(line)] = super::claims::Round { waiting, deadline };
        if waiting != 0 {
            ACTIVE_ROUNDS.fetch_or(1 << line, Ordering::AcqRel);
        } else {
            ACTIVE_ROUNDS.fetch_and(!(1 << line), Ordering::AcqRel);
        }
    }

    /// Re-establish the source's mask after a change: with nobody armed it
    /// is masked and its round dropped; with a listener and nobody left to
    /// wait for it is unmasked (the round is over). A round still waiting on a
    /// claimant leaves the line masked.
    fn settle(&mut self, line: u8) {
        if self.armed_mask(line) == 0 {
            self.set_round(line, 0, 0);
            set_masked(line, true);
        } else if self.rounds[usize::from(line)].waiting == 0 {
            self.set_round(line, 0, 0);
            set_masked(line, false);
        }
    }

    /// Drop every claimant whose round deadline has passed, unmask its line.
    fn expire(&mut self, now: u64, batch: &mut Batch) {
        for line in 0..SOURCES as u8 {
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
            set_masked(line, true);
            return;
        }
        let mut notified: ClaimMask = 0;
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
        set_masked(line, true);
        let in_flight = self.rounds[usize::from(line)].waiting;
        if notified != 0 {
            self.set_round(
                line,
                in_flight | notified,
                now.saturating_add(ACK_DEADLINE_TICKS),
            );
        } else if in_flight == 0 {
            if batch.in_interrupt {
                // Unmasking here, inside the interrupt, would let a
                // level-triggered device that nobody quiets re-enter at
                // once, forever. Hand the raise to the next task-context
                // pass, which lets the line go as below.
                irq::requeue(line);
                return;
            }
            // Everyone is a laggard: nobody can be waited for, so let the line
            // go; `missed` remembers the interrupt for their late acks.
            set_masked(line, false);
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
        let line = claim.source();
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
    /// if nobody else is armed on it. Returns the MSI vector the claim gives
    /// up, for the caller to free once the lock is dropped.
    pub(super) fn silence(&mut self, id: DeviceId) -> Option<u8> {
        let claim = self.get_mut(id)?;
        claim.armed = false;
        claim.pending = false;
        claim.missed = false;
        let line = claim.source();
        if let Some(line) = line {
            let mut round = self.rounds[usize::from(line)];
            round.waiting &= !(1 << id.0);
            self.set_round(line, round.waiting, round.deadline);
            self.settle(line);
        }
        self.get_mut(id)?.msi.take()
    }

    /// Remove `id`'s claim, taking it out of interrupt delivery first. The
    /// caller frees its MSI vector, if it had one, once the lock is dropped.
    pub(super) fn detach(&mut self, id: DeviceId) -> Option<Claim> {
        let claim = self.slots.get_mut(usize::from(id.0))?.take()?;
        if let Some(line) = claim.source() {
            let mut round = self.rounds[usize::from(line)];
            round.waiting &= !(1 << id.0);
            self.set_round(line, round.waiting, round.deadline);
            self.settle(line);
        }
        Some(claim)
    }
}

/// Mask or unmask a delivery source at its controller: a legacy line, or an
/// MSI vector.
fn set_masked(source: u8, masked: bool) {
    if source < msi::SOURCE_BASE {
        irqchip::set_masked(source, masked);
    } else {
        msi::set_masked(source - msi::SOURCE_BASE, masked);
    }
}

/// Whether a userspace claimant is armed on `line` (kernel handlers may not
/// take such a line).
pub fn line_in_use(line: u8) -> bool {
    CLAIMS.lock().armed_mask(line) != 0
}

/// Arm the claim for `id` (generation already checked by the caller): it joins
/// delivery rounds, and its source is unmasked unless a round is in flight.
///
/// A function with an MSI or MSI-X capability is given a vector of its own
/// (`dev::msi`); otherwise, or when no vector is free, the claim takes its
/// INTx line, and a function with neither answers `ENOSYS` (the driver
/// polls). Returns how the interrupts will arrive.
pub fn arm(id: DeviceId, info: &DeviceInfo) -> Result<Mode, Errno> {
    let claim = CLAIMS.lock().get(id).copied().ok_or(EINVAL)?;
    if claim.irq.is_none() {
        return Err(EINVAL);
    }
    if claim.armed {
        return Ok(claim.msi.map_or(Mode::Intx, msi::mode));
    }
    // Routing programs config space and may map the MSI-X table: not under
    // the claim lock.
    let routed = msi::route(id, info).ok();
    let armed = arm_locked(id, claim.generation, routed.map(|(index, _)| index));
    if armed.is_err() {
        if let Some((index, _)) = routed {
            msi::unroute(index, id);
        }
    }
    armed.map(|()| routed.map_or(Mode::Intx, |(_, mode)| mode))
}

/// The locked half of [`arm`]: join delivery on vector `msi`, or on the INTx
/// line without one.
fn arm_locked(id: DeviceId, generation: u32, msi: Option<u8>) -> Result<(), Errno> {
    let mut claims = CLAIMS.lock();
    let entry = claims
        .get_mut(id)
        .filter(|claim| claim.generation == generation && !claim.armed)
        .ok_or(EINVAL)?;
    if msi.is_none() {
        // Not routable: the driver polls.
        let line = entry.line.ok_or(ENOSYS)?;
        if irq::has_kernel_handler(line) {
            return Err(EBUSY);
        }
    }
    entry.armed = true;
    entry.msi = msi;
    let source = entry.source().ok_or(EINVAL)?;
    if claims.rounds[usize::from(source)].waiting == 0 {
        set_masked(source, false);
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
        let (line, missed) = (claim.source(), claim.missed);
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
    }
    // A missed interrupt, or a message an MSI vector latched while masked
    // (unmasking raised it again), is delivered now, not at the next pass.
    if retry || irq::raised_pending() {
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
    service_with(now, false);
}

/// [`service`] from an interrupt handler that stopped code holding no lock
/// (`task::interrupted_quiet_context`, P1.2). The same work, except that a
/// raise nobody can be notified of keeps its line masked until a
/// task-context pass: see `Claims::raise`.
pub fn service_in_interrupt() {
    service_with(crate::task::ticks(), true);
}

/// [`service_in_interrupt`] with an explicit clock (tests).
#[cfg(lazyos_tests)]
pub fn service_in_interrupt_at(now: u64) {
    service_with(now, true);
}

fn service_with(now: u64, in_interrupt: bool) {
    if super::teardown::exits_pending() {
        super::teardown::silence_exited();
    }
    let raised = irq::take_raised();
    if raised == 0 && ACTIVE_ROUNDS.load(Ordering::Acquire) == 0 && !RETRY.load(Ordering::Acquire) {
        return;
    }
    x86_64::instructions::interrupts::without_interrupts(|| run(now, raised, in_interrupt));
}

fn run(now: u64, raised: u64, in_interrupt: bool) {
    let mut batch = Batch::new(in_interrupt);
    RETRY.store(false, Ordering::Release);
    {
        let mut claims = CLAIMS.lock();
        claims.expire(now, &mut batch);
        for source in 0..SOURCES as u8 {
            if raised & (1 << source) != 0 {
                claims.raise(source, now, &mut batch);
            }
        }
        claims.collect_retries(&mut batch);
    }
    for entry in batch.expired.iter().take(batch.expired_count).flatten() {
        notify::record_timeout(entry.0, entry.1);
    }
    crate::perf::lines_posting(raised);
    for post in batch.posts.iter().take(batch.post_count).flatten() {
        crate::perf::irq_posted(post.owner);
        match notify::post_irq(post.dev, post.generation, post.channel, post.side) {
            Ok(()) => {
                DELIVERED.fetch_add(1, Ordering::Relaxed);
                if let Some(index) = post.msi {
                    msi::note_delivered(index, DeviceId(post.dev));
                }
            }
            Err(error) => CLAIMS.lock().post_failed(post, error),
        }
    }
    crate::perf::lines_posted();
}

/// Test-only: forget every claim's delivery state and all rounds.
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    ACTIVE_ROUNDS.store(0, Ordering::Release);
    RETRY.store(false, Ordering::Release);
    msi::reset_for_test();
    let _ = irq::take_raised();
}
