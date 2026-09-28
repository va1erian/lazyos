//! Slab allocator (issue #61).

use super::*;
use crate::mem::slab;
use core::ptr::NonNull;

/// Every class hands out distinct, class-aligned, zeroed slots whose full
/// payload round-trips, and the counters return to baseline after freeing.
pub fn alloc_distinct_aligned() -> Result<(), String> {
    let baseline = slab::stats();
    let mut held: Vec<(usize, NonNull<u8>)> = Vec::new();
    for class in 0..slab::CLASS_COUNT {
        let size = slab::class_size(class);
        for index in 0..3usize {
            let ptr = slab::alloc(class)
                .ok_or_else(|| format!("class {class} slot {index}: alloc returned None"))?;
            let address = ptr.as_ptr() as usize;
            check!(
                address % size == 0,
                "class {class} slot {index} at {address:#x} is not {size}-aligned"
            );
            check!(
                !held
                    .iter()
                    .any(|(_, other)| other.as_ptr() as usize == address),
                "class {class} slot {index} at {address:#x} was handed out twice"
            );
            // Slots are zeroed on allocation.
            for offset in [0, size / 2, size - 1] {
                // Safety: the slot is ours and `offset` is inside it.
                let byte = unsafe { ptr.as_ptr().add(offset).read_volatile() };
                check!(
                    byte == 0,
                    "class {class} slot {index} byte {offset} is {byte:#x}, not zeroed"
                );
            }
            // Fill and verify the whole payload (exactly the class size).
            let seed = (class as u8) ^ (index as u8).wrapping_mul(31);
            for offset in 0..size {
                // Safety: as above.
                unsafe { ptr.as_ptr().add(offset).write_volatile(seed ^ offset as u8) };
            }
            for offset in 0..size {
                // Safety: as above.
                let got = unsafe { ptr.as_ptr().add(offset).read_volatile() };
                check!(
                    got == seed ^ offset as u8,
                    "class {class} slot {index} corrupted at {offset}: {got:#x}"
                );
            }
            held.push((class, ptr));
        }
    }

    let during = slab::stats();
    let expected: usize = (0..slab::CLASS_COUNT)
        .map(|class| 3 * slab::class_size(class))
        .sum();
    check!(
        during.live_bytes == baseline.live_bytes + expected,
        "live bytes while holding all slots: {} (expected {})",
        during.live_bytes,
        baseline.live_bytes + expected
    );
    check!(
        during.peak_bytes >= during.live_bytes,
        "peak {} is below live {}",
        during.peak_bytes,
        during.live_bytes
    );
    for class in 0..slab::CLASS_COUNT {
        check!(
            during.classes[class].live == baseline.classes[class].live + 3,
            "class {class} live is {}, expected {}",
            during.classes[class].live,
            baseline.classes[class].live + 3
        );
    }

    for (class, ptr) in held.into_iter().rev() {
        // Safety: each slot is live and freed exactly once, with its class.
        unsafe { slab::dealloc(class, ptr) };
    }
    let after = slab::stats();
    check!(
        after.live_bytes == baseline.live_bytes,
        "live bytes after freeing all slots: {}, baseline {}",
        after.live_bytes,
        baseline.live_bytes
    );
    for class in 0..slab::CLASS_COUNT {
        check!(
            after.classes[class].live == baseline.classes[class].live,
            "class {class} live after freeing: {}, baseline {}",
            after.classes[class].live,
            baseline.classes[class].live
        );
        check!(
            after.classes[class].frees == baseline.classes[class].frees + 3,
            "class {class} did not record three frees"
        );
    }
    Ok(())
}

/// The free list is LIFO: freeing slots and allocating again returns the
/// most recently freed slot first, for every class.
pub fn reuse_after_free() -> Result<(), String> {
    let baseline = slab::stats();
    for class in 0..slab::CLASS_COUNT {
        let first = slab::alloc(class).ok_or_else(|| format!("class {class}: alloc failed"))?;
        let second = slab::alloc(class).ok_or_else(|| format!("class {class}: alloc failed"))?;
        check!(
            first != second,
            "class {class}: two live slots share an address"
        );

        // Free second then first: first is on top and must come back.
        // Safety: both slots are live and freed exactly once.
        unsafe {
            slab::dealloc(class, second);
            slab::dealloc(class, first);
        }
        let reused = slab::alloc(class).ok_or_else(|| format!("class {class}: realloc failed"))?;
        check!(
            reused == first,
            "class {class}: free list handed back {:#x}, expected {:#x}",
            reused.as_ptr() as usize,
            first.as_ptr() as usize
        );
        // Safety: as above.
        unsafe { slab::dealloc(class, reused) };
    }
    let after = slab::stats();
    check!(
        after.live_bytes == baseline.live_bytes,
        "reuse test left {} live bytes over baseline",
        after.live_bytes.saturating_sub(baseline.live_bytes)
    );
    for class in 0..slab::CLASS_COUNT {
        check!(
            after.classes[class].live == baseline.classes[class].live,
            "class {class} live is {}, baseline {}",
            after.classes[class].live,
            baseline.classes[class].live
        );
    }
    Ok(())
}

/// `class_for_size` rounds up at the class boundaries, `stats` tracks
/// live/peak per class, and peak survives the frees.
pub fn stats_live_peak() -> Result<(), String> {
    check!(
        slab::CLASSES == [32, 64, 128, 256, 512, 1024, 2048, 4096],
        "size classes changed: {:?}",
        slab::CLASSES
    );
    check!(
        slab::class_for_size(0) == Some(0),
        "size 0 did not pick a class"
    );
    check!(
        slab::class_for_size(1) == Some(0),
        "size 1 did not pick class 0"
    );
    check!(
        slab::class_for_size(32) == Some(0),
        "size 32 did not pick class 0"
    );
    check!(
        slab::class_for_size(33) == Some(1),
        "size 33 did not round up to class 1"
    );
    check!(
        slab::class_for_size(4096) == Some(7),
        "the largest size did not pick the last class"
    );
    check!(
        slab::class_for_size(4097).is_none(),
        "a size above the classes picked a slab class"
    );
    check!(
        slab::alloc(slab::CLASS_COUNT).is_none(),
        "alloc accepted an out-of-range class"
    );

    let baseline = slab::stats();
    let small = 2; // 128-byte slots
    let large = 4; // 512-byte slots
    let mut held: Vec<(usize, NonNull<u8>)> = Vec::new();
    for _ in 0..4 {
        held.push((
            small,
            slab::alloc(small).ok_or("128-byte class alloc failed")?,
        ));
    }
    for _ in 0..2 {
        held.push((
            large,
            slab::alloc(large).ok_or("512-byte class alloc failed")?,
        ));
    }

    let during = slab::stats();
    let bytes = 4 * slab::class_size(small) + 2 * slab::class_size(large);
    check!(
        during.live_bytes == baseline.live_bytes + bytes,
        "live bytes are {}, expected {}",
        during.live_bytes,
        baseline.live_bytes + bytes
    );
    check!(
        during.peak_bytes >= during.live_bytes,
        "peak {} is below live {}",
        during.peak_bytes,
        during.live_bytes
    );
    check!(
        during.classes[small].live == baseline.classes[small].live + 4,
        "small class live is {}, expected {}",
        during.classes[small].live,
        baseline.classes[small].live + 4
    );
    check!(
        during.classes[large].live == baseline.classes[large].live + 2,
        "large class live is {}, expected {}",
        during.classes[large].live,
        baseline.classes[large].live + 2
    );
    check!(
        during.classes[small].peak >= during.classes[small].live,
        "class peak below live"
    );

    for (class, ptr) in held {
        // Safety: each slot is live and freed exactly once.
        unsafe { slab::dealloc(class, ptr) };
    }
    let after = slab::stats();
    check!(
        after.live_bytes == baseline.live_bytes,
        "live bytes after frees: {}, baseline {}",
        after.live_bytes,
        baseline.live_bytes
    );
    check!(
        after.classes[small].live == baseline.classes[small].live
            && after.classes[large].live == baseline.classes[large].live,
        "class live did not return to baseline"
    );
    check!(
        after.peak_bytes >= during.live_bytes,
        "peak forgot the high-water mark"
    );
    Ok(())
}

/// Requests above the largest class use the heap fallback, are zeroed and
/// writable end to end, and are reported by the oversized counters.
pub fn oversized_fallback() -> Result<(), String> {
    let baseline = slab::stats();
    let size = slab::MAX_SLAB_SIZE + 123;
    check!(
        slab::class_for_size(size).is_none(),
        "an oversized request picked a slab class"
    );
    let ptr = slab::alloc_bytes(size).ok_or("oversized alloc_bytes returned None")?;
    check!(
        ptr.as_ptr() as usize % 16 == 0,
        "oversized allocation {:#x} is not 16-aligned",
        ptr.as_ptr() as usize
    );
    for offset in (0..size).step_by(64) {
        // Safety: the whole allocation is ours.
        let byte = unsafe { ptr.as_ptr().add(offset).read_volatile() };
        check!(
            byte == 0,
            "oversized byte {offset} is {byte:#x}, not zeroed"
        );
    }
    for offset in [0, size / 2, size - 1] {
        // Safety: as above.
        unsafe { ptr.as_ptr().add(offset).write_volatile(0xa5) };
    }
    for offset in [0, size / 2, size - 1] {
        // Safety: as above.
        let got = unsafe { ptr.as_ptr().add(offset).read_volatile() };
        check!(got == 0xa5, "oversized byte {offset} is {got:#x}");
    }

    let during = slab::stats();
    check!(
        during.oversized_allocations == baseline.oversized_allocations + 1,
        "oversized allocation was not counted"
    );
    check!(
        during.oversized_bytes == baseline.oversized_bytes + size,
        "oversized live bytes are {}, expected {}",
        during.oversized_bytes,
        baseline.oversized_bytes + size
    );
    check!(
        during.oversized_peak_bytes >= during.oversized_bytes,
        "oversized peak below live"
    );

    // Safety: the allocation is live and freed once with the same size.
    unsafe { slab::dealloc_bytes(size, ptr) };
    let after = slab::stats();
    check!(
        after.oversized_bytes == baseline.oversized_bytes,
        "oversized live bytes after free: {}, baseline {}",
        after.oversized_bytes,
        baseline.oversized_bytes
    );
    check!(
        after.oversized_frees == baseline.oversized_frees + 1,
        "oversized free was not counted"
    );

    // An impossible layout is refused, not a panic.
    check!(
        slab::alloc_bytes(usize::MAX / 2).is_none(),
        "an impossible allocation size was accepted"
    );
    Ok(())
}

/// `charge`/`uncharge` are owner-scoped, record peaks, count errors, and
/// saturate instead of underflowing or panicking.
pub fn owner_accounting() -> Result<(), String> {
    let owner = slab::MAX_OWNERS - 1;
    let other = slab::MAX_OWNERS - 2;
    let baseline = slab::owner_stats(owner).ok_or("owner slot is invalid")?;
    let other_baseline = slab::owner_stats(other).ok_or("second owner slot is invalid")?;

    check!(slab::charge(owner, 100), "charge(100) failed");
    check!(slab::charge(owner, 200), "charge(200) failed");
    let during = slab::owner_stats(owner).ok_or("owner vanished")?;
    check!(
        during.live_bytes == baseline.live_bytes + 300,
        "owner live is {}, expected {}",
        during.live_bytes,
        baseline.live_bytes + 300
    );
    check!(
        during.peak_bytes >= during.live_bytes,
        "owner peak below live"
    );
    check!(
        during.charges == baseline.charges + 2,
        "owner charges are {}, expected {}",
        during.charges,
        baseline.charges + 2
    );
    check!(
        slab::owner_stats(other).map(|state| state.live_bytes) == Some(other_baseline.live_bytes),
        "charging one owner moved another"
    );

    check!(slab::uncharge(owner, 100), "uncharge(100) failed");
    let during = slab::owner_stats(owner).ok_or("owner vanished")?;
    check!(
        during.live_bytes == baseline.live_bytes + 200,
        "owner live is {}, expected {}",
        during.live_bytes,
        baseline.live_bytes + 200
    );

    // Over-uncharge is an error, saturates at zero, never wraps.
    let errors = slab::stats().accounting_errors;
    check!(!slab::uncharge(owner, 10_000), "an over-uncharge succeeded");
    let during = slab::owner_stats(owner).ok_or("owner vanished")?;
    check!(
        during.live_bytes == 0,
        "over-uncharge did not saturate at zero"
    );
    check!(
        slab::stats().accounting_errors == errors + 1,
        "over-uncharge was not counted"
    );

    // Out-of-range owners are refused and counted.
    let errors = slab::stats().accounting_errors;
    check!(
        !slab::charge(slab::MAX_OWNERS, 1),
        "charge accepted an out-of-range owner"
    );
    check!(
        !slab::uncharge(slab::MAX_OWNERS, 1),
        "uncharge accepted an out-of-range owner"
    );
    check!(
        slab::owner_stats(slab::MAX_OWNERS).is_none(),
        "bad owner has stats"
    );
    check!(
        slab::stats().accounting_errors == errors + 2,
        "bad-owner calls were not counted"
    );
    Ok(())
}

/// Soak: a million alloc/free rounds across all classes with a moving
/// window of live slots. Live bytes must stay bounded by the window (no
/// leak, no fragmentation blow-up) and drain back to baseline.
pub fn soak_bounded_live() -> Result<(), String> {
    const ROUNDS: usize = 1_000_000;
    const WINDOW: usize = 8;

    let baseline = slab::stats();
    let ceiling = baseline.live_bytes + WINDOW * slab::MAX_SLAB_SIZE;
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    let mut window: [Option<(usize, NonNull<u8>)>; WINDOW] = [None; WINDOW];

    for round in 0..ROUNDS {
        let class = (round / 3) % slab::CLASS_COUNT;
        let size = slab::class_size(class);
        let ptr = slab::alloc(class)
            .ok_or_else(|| format!("round {round}: class {class} alloc failed"))?;
        let seed = (round as u8).wrapping_mul(31);
        // Safety: the slot is ours and at least `size` bytes long.
        unsafe {
            ptr.as_ptr().write_volatile(seed);
            ptr.as_ptr().add(size - 1).write_volatile(seed ^ 0xff);
        }

        let slot = round % WINDOW;
        if let Some((old_class, old)) = window[slot].take() {
            // The slot allocated `WINDOW` rounds ago still holds its own
            // pattern: recycled slots do not bleed into each other.
            let old_seed = ((round - WINDOW) as u8).wrapping_mul(31);
            // Safety: `old` is still live and owned by this task.
            let first = unsafe { old.as_ptr().read_volatile() };
            let last = unsafe {
                old.as_ptr()
                    .add(slab::class_size(old_class) - 1)
                    .read_volatile()
            };
            check!(
                first == old_seed && last == old_seed ^ 0xff,
                "round {round}: slot recycling corrupted data"
            );
            // Safety: `old` is freed exactly once.
            unsafe { slab::dealloc(old_class, old) };
        }
        window[slot] = Some((class, ptr));

        if round % (ROUNDS / 16) == 0 {
            let live = slab::stats().live_bytes;
            check!(
                live <= ceiling,
                "round {round}: live {live} bytes over the ceiling {ceiling}"
            );
        }
    }

    for (class, ptr) in window.into_iter().flatten() {
        // Safety: each remaining window slot is live and freed once.
        unsafe { slab::dealloc(class, ptr) };
    }
    let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
    let after = slab::stats();
    check!(
        after.live_bytes == baseline.live_bytes,
        "soak leaked {} live bytes and {} slots",
        after.live_bytes.saturating_sub(baseline.live_bytes),
        (0..slab::CLASS_COUNT)
            .map(|class| after.classes[class].live)
            .sum::<usize>()
    );
    for class in 0..slab::CLASS_COUNT {
        check!(
            after.classes[class].live == baseline.classes[class].live,
            "class {class} live is {}, baseline {}",
            after.classes[class].live,
            baseline.classes[class].live
        );
    }
    serial_println!(
        "TEST:slab_soak_bounded_live:INFO:rounds={ROUNDS} ops={} cycles={cycles} peak_bytes={}",
        ROUNDS * 2,
        after.peak_bytes.saturating_sub(baseline.live_bytes)
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("slab_alloc_distinct_aligned", alloc_distinct_aligned),
    ("slab_reuse_after_free", reuse_after_free),
    ("slab_stats_live_peak", stats_live_peak),
    ("slab_oversized_fallback", oversized_fallback),
    ("slab_owner_accounting", owner_accounting),
    ("slab_soak_bounded_live", soak_bounded_live),
];
