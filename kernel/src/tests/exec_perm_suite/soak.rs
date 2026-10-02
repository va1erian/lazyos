//! Soak for the execute-permission gate (see the parent module): refusals and
//! successful spawn/exit cycles, by the ten thousand, leave frames, heap,
//! task slots and the interned task names where they started.

use super::*;

const CYCLES: usize = 10_000;

/// What a soak must give back: live frames, heap bytes (general heap plus
/// slab), free task slots.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
struct Usage {
    frames: usize,
    heap: usize,
    slots: usize,
}

fn usage() -> Usage {
    Usage {
        frames: mem::frame_stats().live() as usize,
        heap: crate::mem::slab::stats().live_bytes as usize
            + crate::mem::heap_stats().used as usize,
        slots: task::free_slots(),
    }
}

/// Run `cycle` through `kinds` rounds of every kind to warm the caches it
/// touches (path caches, interned names, the per-slot argument entry), then
/// `CYCLES` times, and require the usage to be back where the warm-up left it.
fn soak(
    what: &str,
    kinds: usize,
    mut cycle: impl FnMut(usize) -> Result<(), String>,
) -> Result<(), String> {
    for warm in 0..kinds * 2 {
        cycle(warm)?;
    }
    let before = usage();
    for index in 0..CYCLES {
        cycle(index)?;
    }
    let after = usage();
    check!(
        after == before,
        "{what}: {CYCLES} cycles moved usage from {before:?} to {after:?}"
    );
    Ok(())
}

/// Every refusal kind, alternating identities and personalities: none of
/// them reads the image, starts a task or keeps anything.
pub fn denied_spawns_leak_nothing() -> Result<(), String> {
    with_tables(|| {
        let lines = [
            (USER, PLAIN),
            (Cred::ROOT, PLAIN),
            (USER, PRIVATE),
            (USER, DIR),
            (Cred::ROOT, DIR),
            (Cred::ROOT, LOCKED),
            (USER, "linux:/transient/plain"),
            (Cred::ROOT, "linux:/transient/dir"),
            (Cred::ROOT, "linux:/mnt/run"),
        ];
        soak("denied spawns", lines.len(), |index| {
            let (cred, line) = lines[index % lines.len()];
            credentials::set(task::KERNEL_TASK, cred);
            let got = spawn(line);
            check!(got == -EACCES, "cycle {index}: {line} gave {got}");
            Ok(())
        })
    })
}

/// Allowed spawns, each finished and reaped at once, native and `linux:` in
/// turn, as root and as a user.
pub fn allowed_spawns_leak_nothing() -> Result<(), String> {
    with_tables(|| {
        let lines = [
            (Cred::ROOT, RUN),
            (USER, "linux:/transient/run"),
            (USER, RUN),
            (Cred::ROOT, PRIVATE),
        ];
        soak("allowed spawns", lines.len(), |index| {
            let (cred, line) = lines[index % lines.len()];
            credentials::set(task::KERNEL_TASK, cred);
            let got = spawn(line);
            check!(got > 0, "cycle {index}: {line} gave {got}");
            Ok(())
        })
    })
}
