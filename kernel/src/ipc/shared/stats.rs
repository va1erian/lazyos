//! Stats snapshots and reset for shared buffers.

use super::*;

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
    }
    stats.handoffs = registry.handoffs;
    stats
}

/// Per-process accounting for `slot`.
pub fn process_stats(slot: usize) -> ProcessStats {
    let registry = REGISTRY.lock();
    let used = registry.uses.iter().find(|used| used.slot == slot);
    ProcessStats {
        bytes: used.map(|used| used.bytes).unwrap_or(0),
        buffers: used.map(|used| used.buffers).unwrap_or(0),
    }
}

/// Drop every buffer, unmapping its frames, and reset the counters. Process
/// teardown and test isolation (the caller must run before the affected
/// address spaces are torn down).
pub fn reset() {
    let mut registry = REGISTRY.lock();
    while !registry.buffers.is_empty() {
        let last = registry.buffers.len() - 1;
        // Handles may still be open: release their allocator references too.
        destroy_buffer(&mut registry, last, true, false);
    }
    registry.uses.clear();
    registry.handoffs = 0;
}
