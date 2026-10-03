//! The physical memory map and the virtual layout: usable regions gathered
//! from hostile firmware maps, a private user window that spans many PML4
//! entries (fork and teardown walk all of them), RAM above the 4 GiB PCI hole,
//! and a Linux image's lazily populated stack.

use super::*;
use crate::mem::{Regions, MAX_REGIONS, USER_TOP};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// Sorted, disjoint, non-touching, frame-aligned and above the low megabyte.
fn well_formed(regions: &Regions) -> Result<(), String> {
    for index in 0..regions.count {
        let (start, end) = (regions.starts[index], regions.ends[index]);
        check!(start < end, "region {index} is empty: {start:#x}..{end:#x}");
        check!(
            start % 4096 == 0 && end % 4096 == 0,
            "region {index} unaligned"
        );
        check!(start >= 0x10_0000, "region {index} below 1 MiB");
        if index > 0 {
            check!(
                regions.ends[index - 1] < start,
                "regions {} and {index} overlap or touch",
                index - 1
            );
        }
    }
    Ok(())
}

/// A UEFI-shaped map: unsorted, overlapping, touching, unaligned, below 1 MiB
/// and above the 4 GiB hole. Touching and overlapping entries merge.
pub fn regions_merge_hostile_maps() -> Result<(), String> {
    let map = [
        (5 * GIB, 6 * GIB),           // above the hole
        (0x10_0000 + 100, 0x20_0000), // unaligned start
        (0x20_0000, 0x30_0000 + 100), // touches the previous, unaligned end
        (0, 0x9_f000),                // below 1 MiB: dropped
        (0x40_0000, 0x50_0000),
        (0x48_0000, 0x60_0000),        // overlaps the previous
        (4 * GIB, 5 * GIB),            // touches the first
        (0x7000_0000, 0x7000_0800),    // less than a frame
        (u64::MAX - 0x1000, u64::MAX), // absurd but must not overflow
    ];
    let regions = Regions::gather(map);
    well_formed(&regions)?;
    let got: Vec<(u64, u64)> = (0..regions.count)
        .map(|i| (regions.starts[i], regions.ends[i]))
        .collect();
    // The sub-frame and absurd entries round to nothing.
    let want = [
        (0x10_1000, 0x30_0000),
        (0x40_0000, 0x60_0000),
        (4 * GIB, 6 * GIB),
    ];
    check!(got == want, "regions {got:x?}");
    check!(
        regions.highest() == 6 * GIB,
        "highest {:#x}",
        regions.highest()
    );
    check!(regions.dropped == 0, "dropped {}", regions.dropped);
    Ok(())
}

/// More disjoint ranges than slots: the smallest are dropped and counted,
/// never a large one, whatever order they arrive in.
pub fn regions_keep_the_largest_when_full() -> Result<(), String> {
    let total = MAX_REGIONS as u64 + 40;
    // Small 1-frame islands first, then two big ranges at the end.
    let mut map: Vec<(u64, u64)> = (0..total)
        .map(|i| (0x100_0000 + i * 0x2000, 0x100_0000 + i * 0x2000 + 0x1000))
        .collect();
    map.push((GIB, 2 * GIB));
    map.push((4 * GIB, 8 * GIB));
    let regions = Regions::gather(map.iter().copied());
    well_formed(&regions)?;
    check!(regions.count == MAX_REGIONS, "count {}", regions.count);
    check!(regions.highest() == 8 * GIB, "the top range was dropped");
    check!(
        (0..regions.count).any(|i| regions.starts[i] == GIB),
        "the 1 GiB range was dropped"
    );
    check!(
        regions.dropped == 42 * 0x1000,
        "dropped {:#x}",
        regions.dropped
    );
    Ok(())
}

/// Soak: thousands of seeded random maps; the result is always well formed
/// and never claims more bytes than the input offered.
pub fn regions_soak_random_maps() -> Result<(), String> {
    let mut seed = 0x51_7cc1_b727_220au64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..3000 {
        let entries = (next() % 200) as usize;
        let mut map = Vec::new();
        let mut offered = 0u64;
        for _ in 0..entries {
            let start = next() % (16 * GIB);
            let len = next() % (if next() % 4 == 0 { GIB } else { 64 * MIB });
            map.push((start, start + len));
            offered += len;
        }
        let regions = Regions::gather(map.iter().copied());
        well_formed(&regions).map_err(|e| format!("round {round}: {e}"))?;
        check!(
            regions.bytes() <= offered,
            "round {round}: {} bytes from {offered} offered",
            regions.bytes()
        );
    }
    Ok(())
}

/// The private window spans PML4 entries 0..255: pages near the bottom, in
/// the middle and at the very top of it are mapped, counted, shared
/// copy-on-write by fork and freed by teardown, frame for frame.
pub fn user_window_spans_many_entries() -> Result<(), String> {
    let vas = [
        0x40_0000u64,
        1 << 39,
        process::layout::MMAP_BASE + 0x1000,
        0x6400_0000_0000,
        process::layout::STACK_TOP - 0x1000,
        USER_TOP - 0x1000,
    ];
    let before = mem::frame_stats().free;
    let parent = mem::new_user_table().ok_or("new_user_table failed")?;
    for (seed, &va) in vas.iter().enumerate() {
        let pages = process::map_range(parent, va, va + 4096).map_err(to_string)?;
        fill_frame(pages[0].1, seed as u8);
    }
    check!(
        mem::user_table_frame_count(parent) == vas.len(),
        "counted {} pages",
        mem::user_table_frame_count(parent)
    );
    let child = mem::clone_user_table(parent).ok_or("fork failed")?;
    for (seed, &va) in vas.iter().enumerate() {
        let shared = frame_of(child, va)?;
        check!(
            shared == frame_of(parent, va)?,
            "{va:#x} is not shared after fork"
        );
        check!(
            frame_matches(shared, seed as u8),
            "{va:#x} lost its contents"
        );
        check!(
            mem::cow_fault(child, va),
            "write to {va:#x} in the child did not copy"
        );
        let private = frame_of(child, va)?;
        check!(
            private != shared && frame_matches(private, seed as u8),
            "copy of {va:#x}"
        );
    }
    mem::free_user_table(child);
    mem::free_user_table(parent);
    let after = mem::frame_stats().free;
    check!(
        after == before,
        "frames leaked: {before} free before, {after} after"
    );
    Ok(())
}

/// With RAM above 4 GiB (QEMU `-m 6G` puts it above the PCI hole), frames
/// there are handed out, reachable through the physical map and returned.
/// Passes trivially (with a note) on a smaller machine.
pub fn high_frames_reachable() -> Result<(), String> {
    const FOUR_GIB: u64 = 1 << 32;
    if mem::usable_ram() <= 4 * GIB {
        serial_println!("TEST:mem_high_frames_reachable:INFO:no RAM above 4 GiB to test");
        return Ok(());
    }
    // Frames come out in address order, so drain the low ones until a high
    // frame appears (bounded by what is below 4 GiB).
    let mut held = Vec::new();
    let mut high = None;
    while held.len() < (FOUR_GIB / 4096) as usize {
        let Some(frame) = mem::alloc_frame() else {
            break;
        };
        if frame.as_u64() >= FOUR_GIB {
            high = Some(frame);
            break;
        }
        held.push(frame);
    }
    let outcome = match high {
        Some(frame) => {
            fill_frame(frame.as_u64(), 0x6b);
            let ok = frame_matches(frame.as_u64(), 0x6b);
            mem::free_frame(frame);
            if ok {
                Ok(())
            } else {
                Err(format!("frame {:#x} did not hold its data", frame.as_u64()))
            }
        }
        None => Err(String::from("no frame above 4 GiB was handed out")),
    };
    for frame in held {
        mem::free_frame(frame);
    }
    outcome
}

/// A Linux image's stack: `limit.stack_size` of `Stack` VMA under the stack
/// top, only the start frame present, the rest faulting in zeroed; the break
/// starts on the page after the image.
pub fn linux_stack_is_lazy() -> Result<(), String> {
    let elf = super::super::service_suite::minimal_elf();
    let argv = process::linux::nul_terminated(&[b"prog", b"arg"]);
    let envp = process::linux::nul_terminated(&[b"A=b"]);
    let before = mem::frame_stats().free;
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    let outcome = (|| {
        let started = process::linux::load_image(table, &elf, &argv, &envp, (0, 0))
            .map_err(|error| String::from(error.message()))?;
        let top = process::layout::STACK_TOP;
        let size = process::linux::stack_size();
        let stack = mem::vma::find_range(table, top - size, top);
        check!(
            stack.len() == 1 && stack[0].kind == Kind::Stack && stack[0].start == top - size,
            "stack VMA {stack:?}"
        );
        check!(
            started.rsp % 16 == 0 && started.rsp < top,
            "rsp {:#x}",
            started.rsp
        );
        check!(
            raw_entry(table, started.rsp & !0xfff).is_some(),
            "start frame not mapped"
        );
        let deep = top - size + 0x1000;
        check!(
            raw_entry(table, deep).is_none(),
            "deep stack mapped eagerly"
        );
        check!(
            mem::demand_fault(table, deep, PageFaultErrorCode::CAUSED_BY_WRITE),
            "deep stack page did not fault in"
        );
        check!(
            raw_entry(table, top - size - 0x1000).is_none()
                && mem::vma::find(table, top - size - 0x1000).is_none(),
            "something is mapped below the stack"
        );
        check!(
            started.brk % 4096 == 0 && started.brk < process::layout::MMAP_BASE,
            "brk {:#x}",
            started.brk
        );
        Ok(())
    })();
    mem::free_user_table(table);
    check!(mem::frame_stats().free == before, "the load leaked frames");
    outcome
}
