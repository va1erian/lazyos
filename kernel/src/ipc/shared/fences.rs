//! Fences, stats and reset for shared buffers.

use super::*;

/// Record that everything up to `sequence` in the buffer is ready for readers.
///
/// Sequences must not go backwards: a submit older than the current head is
/// [`Error::StaleSequence`], a repeated submit is a no-op. Submitting wakes
/// every fence waiter (advisory; each re-checks its own buffer).
pub fn fence_submit(handle: u64, sequence: u64) -> Result<(), Error> {
    let object_id = object_of(handle, rights::CALL)?;
    {
        let mut registry = REGISTRY.lock();
        let Some(index) = registry
            .buffers
            .iter()
            .position(|buffer| buffer.object_id == object_id)
        else {
            return Err(Error::NotFound);
        };
        let buffer = &mut registry.buffers[index];
        if sequence < buffer.submitted {
            return Err(Error::StaleSequence);
        }
        buffer.submitted = sequence;
        registry.fences_submitted += 1;
    }
    FENCES.notify_all();
    Ok(())
}

/// Wait until the buffer's fence head is at least `sequence`.
///
/// Parks on [`FENCES`] until a producer's [`fence_submit`] passes the sequence
/// or `deadline` (absolute PIT ticks) passes. A submit that lands between the
/// check and the park still resolves the wait: every wakeup re-checks the
/// counter.
pub fn fence_wait(handle: u64, sequence: u64, deadline: Option<u64>) -> Result<(), Error> {
    let object_id = object_of(handle, rights::CALL)?;
    let me = task::current();
    let mut counted = false;
    loop {
        {
            let mut registry = REGISTRY.lock();
            let Some(index) = registry
                .buffers
                .iter()
                .position(|buffer| buffer.object_id == object_id)
            else {
                return Err(Error::NotFound);
            };
            let buffer = &mut registry.buffers[index];
            if buffer.submitted >= sequence {
                buffer.waited = buffer.waited.max(sequence);
                return Ok(());
            }
            if !counted {
                counted = true;
                registry.fence_waits += 1;
                use_of(&mut registry, me).fence_waits += 1;
            }
        }
        let reason = FENCES.wait(me, deadline);
        // A killed waiter must reach its syscall return to die; the error is
        // never seen.
        if reason == WakeReason::Interrupted && task::signal::killed(me) {
            return Err(Error::TimedOut);
        }
        if reason != WakeReason::TimedOut {
            continue;
        }
        // Re-check once more: a submit may have won the race with the
        // deadline sweep.
        let mut registry = REGISTRY.lock();
        let Some(index) = registry
            .buffers
            .iter()
            .position(|buffer| buffer.object_id == object_id)
        else {
            return Err(Error::NotFound);
        };
        let buffer = &mut registry.buffers[index];
        if buffer.submitted >= sequence {
            buffer.waited = buffer.waited.max(sequence);
            return Ok(());
        }
        registry.fence_timeouts += 1;
        use_of(&mut registry, me).fence_timeouts += 1;
        return Err(Error::TimedOut);
    }
}

/// Aggregate counters across every live buffer.
pub fn stats() -> Stats {
    let registry = REGISTRY.lock();
    let mut stats = Stats {
        buffers: registry.buffers.len() as u64,
        ..Default::default()
    };
    for buffer in &registry.buffers {
        stats.bytes += buffer.size;
        stats.mappings += buffer.mappings.len() as u64;
        stats.outstanding_fences += buffer.submitted.saturating_sub(buffer.waited);
    }
    stats.fences_submitted = registry.fences_submitted;
    stats.fence_waits = registry.fence_waits;
    stats.fence_timeouts = registry.fence_timeouts;
    stats.handoffs = registry.handoffs;
    stats
}

/// Per-process accounting for `slot`.
pub fn process_stats(slot: usize) -> ProcessStats {
    let registry = REGISTRY.lock();
    let used = registry.uses.iter().find(|used| used.slot == slot);
    let mut stats = ProcessStats {
        bytes: used.map(|used| used.bytes).unwrap_or(0),
        buffers: used.map(|used| used.buffers).unwrap_or(0),
        fence_waits: used.map(|used| used.fence_waits).unwrap_or(0),
        fence_timeouts: used.map(|used| used.fence_timeouts).unwrap_or(0),
        outstanding_fences: 0,
    };
    for buffer in &registry.buffers {
        if buffer.owner == slot {
            stats.outstanding_fences += buffer.submitted.saturating_sub(buffer.waited);
        }
    }
    stats
}

/// Drop every buffer, unmapping its frames, and reset the counters and fence
/// waiters. Process teardown and test isolation (the caller must run before the
/// affected address spaces are torn down).
pub fn reset() {
    let mut registry = REGISTRY.lock();
    while !registry.buffers.is_empty() {
        let last = registry.buffers.len() - 1;
        // Handles may still be open: release their allocator references too.
        destroy_buffer(&mut registry, last, true, false);
    }
    registry.uses.clear();
    registry.fences_submitted = 0;
    registry.fence_waits = 0;
    registry.fence_timeouts = 0;
    registry.handoffs = 0;
    drop(registry);
    FENCES.notify_all();
}
