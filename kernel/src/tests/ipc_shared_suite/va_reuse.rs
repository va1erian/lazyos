//! Virtual-range recycling and page-table reclamation for buffer mappings
//! (issue #237).

use super::*;

fn make(size: u64) -> Result<(u64, u64), String> {
    let handle = shared::create(size).map_err(buffer_reason)?;
    let va = shared::map(handle).map_err(buffer_reason)?;
    Ok((handle, va))
}

fn close(handle: u64) -> Result<(), String> {
    shared::close(handle).map_err(buffer_reason)
}

/// A closed buffer's virtual range is handed out again, and the frames
/// (including page tables) return to the allocator.
pub fn buffer_va_reused_after_close() -> Result<(), String> {
    fresh()?;
    // Warm up so the never-freed PDPT of the buffer region exists.
    let (warm, _) = make(4096)?;
    close(warm)?;
    let live = mem::frame_stats().live();
    let (first, va) = make(3 * 4096)?;
    close(first)?;
    let (second, again) = make(3 * 4096)?;
    check!(again == va, "range not reused: {va:#x} then {again:#x}");
    close(second)?;
    check!(
        mem::frame_stats().live() == live,
        "frames leaked: {} -> {}",
        live,
        mem::frame_stats().live()
    );
    Ok(())
}

/// Live buffers never overlap, and freeing them in any order coalesces the
/// free list back into the cursor.
pub fn buffer_va_no_overlap_and_coalesce() -> Result<(), String> {
    fresh()?;
    let (cursor, free) = crate::ipc::shared_va::snapshot();
    let sizes = [4096u64, 3 * 4096, 2 * 4096, 5 * 4096];
    let mut live: Vec<(u64, u64, u64)> = Vec::new();
    for size in sizes {
        let (handle, va) = make(size)?;
        for (_, other, other_size) in &live {
            check!(
                va + size <= *other || *other + *other_size <= va,
                "overlap {va:#x}+{size:#x} vs {other:#x}+{other_size:#x}"
            );
        }
        live.push((handle, va, size));
    }
    // Free out of order: 1, 3, 0, 2.
    for index in [1usize, 3, 0, 2] {
        close(live[index].0)?;
    }
    check!(
        crate::ipc::shared_va::snapshot() == (cursor, free),
        "free list did not coalesce: {:?} vs {:?}",
        crate::ipc::shared_va::snapshot(),
        (cursor, free)
    );
    Ok(())
}

/// Stress: thousands of create/map/close rounds with a rotating live set of
/// mixed sizes never grow the cursor or the frame count.
pub fn buffer_va_soak_bounded() -> Result<(), String> {
    fresh()?;
    let (warm, _) = make(4096)?;
    close(warm)?;
    let live_frames = mem::frame_stats().live();
    let (cursor, _) = crate::ipc::shared_va::snapshot();
    let mut ring = [None::<u64>; 8];
    let mut high = cursor;
    for round in 0..4000usize {
        let slot = round % ring.len();
        if let Some(old) = ring[slot] {
            close(old)?;
        }
        let (handle, _) = make(4096 * (1 + (round % 5) as u64))?;
        ring[slot] = Some(handle);
        high = high.max(crate::ipc::shared_va::snapshot().0);
    }
    for handle in ring.into_iter().flatten() {
        close(handle)?;
    }
    check!(
        high - cursor <= 8 * 5 * 4096 * 2,
        "cursor grew {:#x} bytes over the soak",
        high - cursor
    );
    check!(
        mem::frame_stats().live() == live_frames,
        "frame leak: {} -> {}",
        live_frames,
        mem::frame_stats().live()
    );
    Ok(())
}
