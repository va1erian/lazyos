//! The heap's small-object slabs (docs/performance-plan.md P6.4).

use super::*;
use alloc::alloc::{alloc as raw_alloc, dealloc as raw_dealloc};
use alloc::boxed::Box;
use core::alloc::Layout;

/// Whether `ptr` lies in the linked-list heap's span (else a slab slot).
fn in_list(ptr: *const u8) -> bool {
    (mem::HEAP_START..mem::HEAP_START + (1 << 39)).contains(&(ptr as u64))
}

/// Allocations of up to 2 KiB come from the slabs (outside the list's span),
/// larger ones from the list; every one is aligned as asked and reads back
/// intact, and the heap's statistics count both and return to where they
/// were.
pub fn small_objects_use_slabs() -> Result<(), String> {
    let before = mem::heap_stats();
    let mut small: Vec<Box<[u8; 48]>> = Vec::with_capacity(1000);
    for index in 0..1000usize {
        small.push(Box::new([index as u8; 48]));
    }
    for (index, block) in small.iter().enumerate() {
        let ptr = block.as_ptr();
        check!(!in_list(ptr), "a 48-byte box came from the list at {ptr:p}");
        check!(
            ptr as usize % 64 == 0,
            "a 48-byte box at {ptr:p} is not slot-aligned"
        );
        check!(
            block.iter().all(|&byte| byte == index as u8),
            "box {index} corrupted"
        );
    }
    let big: Vec<u8> = vec![0x5a; 8192];
    check!(in_list(big.as_ptr()), "an 8 KiB block left the list");
    let during = mem::heap_stats();
    check!(
        during.used >= before.used + 1000 * 64 + 8192,
        "the heap counts {} bytes in use, {} before",
        during.used,
        before.used
    );
    for (size, align) in [
        (1, 1),
        (24, 8),
        (100, 64),
        (200, 256),
        (1500, 1024),
        (2048, 2048),
    ] {
        let layout = Layout::from_size_align(size, align).map_err(|_| "layout")?;
        // SAFETY: a non-zero size; freed with the same layout below.
        let ptr = unsafe { raw_alloc(layout) };
        check!(!ptr.is_null(), "{size}/{align} refused");
        check!(
            ptr as usize % align == 0 && !in_list(ptr),
            "{size}/{align} at {ptr:p}: misaligned or not a slab slot"
        );
        // SAFETY: `ptr` is a live allocation of `layout`.
        unsafe {
            core::ptr::write_bytes(ptr, 0xa5, size);
            raw_dealloc(ptr, layout);
        }
    }
    drop(small);
    drop(big);
    let after = mem::heap_stats();
    check!(
        after.used == before.used,
        "{} bytes in use after, {} before",
        after.used,
        before.used
    );
    Ok(())
}

/// Soak: two million allocations of random sizes up to 2 KiB and random
/// alignments, up to 512 live at once, each filled with its own pattern and
/// checked when freed: no slot is handed out twice, nothing is corrupted,
/// and the bytes in use come back exactly.
pub fn small_object_soak() -> Result<(), String> {
    let before = mem::heap_stats().used;
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut live: Vec<(*mut u8, Layout, u8)> = Vec::with_capacity(512);
    for round in 0..2_000_000u32 {
        let roll = next();
        if live.len() == 512 || (!live.is_empty() && roll % 3 == 0) {
            let (ptr, layout, tag) = live.swap_remove((roll as usize >> 8) % live.len());
            // SAFETY: `ptr` is a live allocation of `layout`, filled below.
            let intact = unsafe { core::slice::from_raw_parts(ptr, layout.size()) }
                .iter()
                .all(|&byte| byte == tag);
            check!(intact, "round {round}: a {layout:?} block was overwritten");
            // SAFETY: as above; freed once.
            unsafe { raw_dealloc(ptr, layout) };
            continue;
        }
        let size = 1 + (roll as usize >> 16) % 2048;
        let align = 1usize << ((roll >> 40) % 8);
        let layout = Layout::from_size_align(size, align).map_err(|_| "layout")?;
        // SAFETY: a non-zero size; freed with the same layout.
        let ptr = unsafe { raw_alloc(layout) };
        check!(!ptr.is_null(), "round {round}: {layout:?} refused");
        check!(
            ptr as usize % align == 0,
            "round {round}: {ptr:p} misaligned"
        );
        let tag = round as u8;
        // SAFETY: `ptr` is a fresh allocation of `size` bytes.
        unsafe { core::ptr::write_bytes(ptr, tag, size) };
        live.push((ptr, layout, tag));
    }
    for (ptr, layout, _) in live.drain(..) {
        // SAFETY: each is a live allocation of its layout, freed once.
        unsafe { raw_dealloc(ptr, layout) };
    }
    drop(live);
    let after = mem::heap_stats().used;
    check!(
        after == before,
        "{after} bytes in use after, {before} before"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("heap_small_objects_use_slabs", small_objects_use_slabs),
    ("heap_small_object_soak", small_object_soak),
];
