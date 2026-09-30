//! DMA stress/soak tests (issue #241): allocator churn under fragmentation and
//! driver spawn/kill churn with live DMA buffers. Each ends by proving the
//! pool, quota, handles, frames and claims are back to baseline.

use super::fixture::*;
use super::*;
use crate::dev::claims::CLAIMS;
use crate::quota::Resource;

/// Alloc/free generations with a deterministic xorshift RNG and random sizes,
/// churning the pool into fragments and back. At the end the pool is one free
/// run again and the frame allocator saw no double free.
pub fn dma_pool_alloc_free_soak() -> Result<(), String> {
    const OPS: u32 = 60_000;
    const MAX_LIVE: usize = 16;
    /// Roughly a minute of wall clock; the loop is expected to fit in tens of
    /// seconds even under TCG, and the runner's timeout is the second bound.
    const MAX_CYCLES: u64 = 120_000_000_000;

    let base = pool();
    // SAFETY: `rdtsc` only reads the time-stamp counter.
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut live: Vec<(u64, u64)> = Vec::new();
    for op in 0..OPS {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let pages = 1 + rng % 64;
        let free_first = live.len() >= MAX_LIVE || (rng & 3 == 0 && !live.is_empty());
        if free_first {
            let index = (rng as usize) % live.len();
            let (phys, pages) = live.swap_remove(index);
            for page in 0..pages {
                mem::free_frame(PhysAddr::new(phys + page * 4096));
            }
        } else if let Some(phys) = mem::dma_alloc(pages, 1) {
            live.push((phys.as_u64(), pages));
        } else if !live.is_empty() {
            // Fragmented: release one run and carry on.
            let index = (rng as usize) % live.len();
            let (phys, pages) = live.swap_remove(index);
            for page in 0..pages {
                mem::free_frame(PhysAddr::new(phys + page * 4096));
            }
        }
        if op % 20_000 == 0 {
            serial_println!("TEST:dev_stress_dma_pool_alloc_free_soak:PROGRESS:{op}/{OPS}");
        }
    }
    for (phys, pages) in live.drain(..) {
        for page in 0..pages {
            mem::free_frame(PhysAddr::new(phys + page * 4096));
        }
    }
    // SAFETY: `rdtsc` only reads the time-stamp counter.
    let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
    serial_println!(
        "TEST:dev_stress_dma_pool_alloc_free_soak:INFO:ops={OPS} max_live={MAX_LIVE} cycles={cycles}"
    );
    check!(
        pool() == base,
        "the pool did not coalesce: {:?} != {:?}",
        pool(),
        base
    );
    check!(
        mem::frame_stats().double_frees == 0 && mem::frame_stats().invalid_frees == 0,
        "the soak caused a double free"
    );
    check!(cycles < MAX_CYCLES, "the soak used {cycles} cycles");
    Ok(())
}

/// Spawn a driver, claim the device, allocate several DMA buffers (one
/// transferred to a long-lived client), kill and reap it: many times. The pool,
/// quota, handles, frames and claims must all return to baseline.
pub fn dma_spawn_kill_soak() -> Result<(), String> {
    const ROUNDS: u32 = 200;
    let fx = Fixture::new()?;
    let base = pool();
    let dev = add_device(Spec::nic(None))?;
    // The long-lived client allocates its address-space frame, so take the
    // frame baseline after it exists.
    let client = spawn_driver(driver_cred())?;
    let frames_before = mem::frame_stats();
    let audit_before = audit::total();
    let mut bus = 0u64;
    for round in 0..ROUNDS {
        let slot = spawn_driver(driver_cred())?;
        enter(slot)?;
        let handle = expect_ok(claim_plain(dev), "claim")?;
        let buffer = expect_ok(dma_alloc(handle, 2 * 4096, 0, &mut bus), "alloc")?;
        let _ = expect_ok(dma_alloc(handle, 4096, 0, &mut bus), "alloc two")?;
        if round % 4 == 0 {
            transfer_and_consume(slot, client, buffer)?;
        }
        leave(&fx);
        task::harness::finish(slot, 0);
        check!(
            task::reap_child().map(|reaped| reaped.0) == Some(slot),
            "round {round}: the driver was not reaped"
        );
        check!(CLAIMS.lock().len() == 0, "round {round}: a claim survived");
        check!(
            pool().free_pages == base.free_pages,
            "round {round}: pool leaked: {:?}",
            pool()
        );
    }
    check!(
        pool() == base,
        "the pool leaked: {:?} != {:?}",
        pool(),
        base
    );
    check!(usage(Resource::DmaMemory) == 0, "DMA quota leaked");
    check!(usage(Resource::Handles) == 0, "handles leaked");
    let frames_after = mem::frame_stats();
    check!(
        frames_after.live() == frames_before.live()
            && frames_after.double_frees == frames_before.double_frees
            && frames_after.invalid_frees == frames_before.invalid_frees,
        "frames {} -> {}",
        frames_before.live(),
        frames_after.live()
    );
    check!(
        audit::total() >= audit_before + 2 * u64::from(ROUNDS),
        "the soak did not audit every claim/release"
    );
    check!(
        audit::last_hash() != 0,
        "the audit chain head is empty after the soak"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_stress_dma_pool_alloc_free_soak",
        dma_pool_alloc_free_soak,
    ),
    ("dev_stress_dma_spawn_kill_soak", dma_spawn_kill_soak),
];
