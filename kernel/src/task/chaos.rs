//! Chaos preemption (docs/smp-plan.md section 4, item 5; test kernels only).
//!
//! A test image built with `LAZYOS_CHAOS_SEED=<n>` (`tools/test/run.py
//! --chaos-seed N`) asks for a reschedule at a seeded fraction of its
//! preemption points (every syscall and interrupt return, `task::preempt`)
//! and opens an interrupt window at a seeded fraction of its poll points
//! inside syscalls (`arch::irq_window`), so tasks lose and regain CPUs, and
//! interrupts land inside long kernel work, at moments a quiet run never
//! picks: an ordering assumption the entry lock does not cover shows as a
//! failure. (The plan's third site, every lock acquire, is not hooked: a
//! forced park there would change the locks' own behaviour under test.) The
//! choice comes from a per-CPU xorshift seeded by the seed and the CPU
//! index, so under TCG (`-icount`) one seed replays one interleaving; the
//! suite prints the seed with every result (`tests::run`). Without the
//! switch [`seed`] is `None` and the hook costs a constant test.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::percpu::{self, MAX_CPUS};

/// One preemption point in this many forces a reschedule.
const ONE_IN: u64 = 4;

/// Per CPU: the xorshift state (0: not seeded yet).
static STATE: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// The build's chaos seed (decimal or `0x` hex), parsed once at compile time.
/// Empty means none; anything else that is not a number fails the build, so
/// a mistyped seed never runs as a quiet image that passes.
const SEED: Option<u64> = match option_env!("LAZYOS_CHAOS_SEED") {
    Some(text) if text.is_empty() => None,
    Some(text) => match parse(text.as_bytes()) {
        Some(seed) => Some(seed),
        None => panic!("LAZYOS_CHAOS_SEED is not a decimal or 0x-prefixed hex number"),
    },
    None => None,
};

/// A decimal or `0x`-prefixed hex number; `None` for anything else.
const fn parse(text: &[u8]) -> Option<u64> {
    let (radix, mut at) = match text {
        [b'0', b'x' | b'X', ..] => (16, 2),
        _ => (10, 0),
    };
    if at == text.len() {
        return None;
    }
    let mut value: u64 = 0;
    while at < text.len() {
        let digit = match text[at] {
            byte @ b'0'..=b'9' => (byte - b'0') as u64,
            byte @ b'a'..=b'f' if radix == 16 => (byte - b'a' + 10) as u64,
            byte @ b'A'..=b'F' if radix == 16 => (byte - b'A' + 10) as u64,
            _ => return None,
        };
        value = match value.checked_mul(radix) {
            Some(scaled) => match scaled.checked_add(digit) {
                Some(sum) => sum,
                None => return None,
            },
            None => return None,
        };
        at += 1;
    }
    Some(value)
}

/// The build's chaos seed, if any.
pub fn seed() -> Option<u64> {
    SEED
}

/// Whether this preemption or poll point should act (one in [`ONE_IN`] when
/// a seed is set). Interrupts off: only this CPU touches its state.
pub(crate) fn strike() -> bool {
    let Some(seed) = seed() else {
        return false;
    };
    let state = &STATE[percpu::current().index() as usize];
    let mut x = state.load(Ordering::Relaxed);
    if x == 0 {
        // Never zero (xorshift's fixed point), distinct per CPU.
        x = (seed ^ 0x9E37_79B9_7F4A_7C15).wrapping_add(percpu::current().index() as u64) | 1;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    state.store(x, Ordering::Relaxed);
    x % ONE_IN == 0
}
