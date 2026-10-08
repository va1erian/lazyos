//! The bottom half's entry points (`dev::intx`): when it runs, and one pass
//! over the claim table (expired rounds, held lines, raises, retries), with
//! the posts made once the claim lock is dropped.

use core::sync::atomic::Ordering;

use super::super::claims::{CLAIMS, SOURCES};
use super::super::{irq, msi, notify, DeviceId};
use super::{Batch, ACTIVE_ROUNDS, DELIVERED, RETRY};

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
    if super::super::teardown::exits_pending() {
        super::super::teardown::silence_exited();
    }
    // The release closes handles and walks page tables: task context only.
    if !in_interrupt && super::super::teardown::releases_pending() {
        super::super::teardown::release_exited();
    }
    let raised = irq::take_raised();
    if raised == 0
        && ACTIVE_ROUNDS.load(Ordering::Acquire) == 0
        && !RETRY.load(Ordering::Acquire)
        && !super::super::throttle::any_held()
    {
        return;
    }
    x86_64::instructions::interrupts::without_interrupts(|| run(now, raised, in_interrupt));
}

fn run(now: u64, raised: u64, in_interrupt: bool) {
    let mut batch = Batch::new(in_interrupt);
    RETRY.store(false, Ordering::Release);
    {
        let mut claims = CLAIMS.lock();
        claims.release_held(now);
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
    super::super::throttle::reset_for_test();
    CLAIMS.lock().throttle = [super::super::throttle::Throttle::NEW; irq::LINES as usize];
    let _ = irq::take_raised();
}
