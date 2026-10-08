//! `dma_alloc` through the syscall path (issue #241): layout and zeroing,
//! alignment, hostile input, fragmentation refusal and the `DmaMemory` quota.
//! Lifetime, teardown and ordering live in `sys_dma_life.rs`.

use super::fixture::*;
use super::*;
use crate::dev::errno::*;
use crate::dev::syscall::*;
use crate::mem::vma::Prot;
use crate::quota::{self, Resource};

/// Claim `dev` in a fresh driver task (entered) and return `(slot, handle)`.
fn driver_with(dev: DeviceId) -> Result<(usize, u64), String> {
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    Ok((slot, handle))
}

fn close(handle: u64) -> Result<(), String> {
    crate::ipc::shared::close(handle).map_err(|error| error.message().to_string())
}

/// Runs of `pages` frames are contiguous, page aligned, below 4 GiB, zeroed;
/// the bus address is the first frame; two allocations never overlap.
pub fn dma_layout_and_zeroing() -> Result<(), String> {
    let fx = Fixture::new()?;
    let base = idle_pool()?;
    let dev = add_device(Spec::nic(None))?;
    let (slot, handle) = driver_with(dev)?;
    let len = 3 * 4096;
    let mut bus = 0u64;
    let buffer = expect_ok(dma_alloc(handle, len, 0, &mut bus), "dma_alloc")?;
    let info = crate::ipc::shared::info(buffer).map_err(|error| error.message().to_string())?;
    check!(info.dma && info.size == len, "buffer info {info:?}");
    let frames = frames_of(buffer)?;
    check!(frames.len() == 3, "{} frames, expected 3", frames.len());
    check!(
        bus == frames[0],
        "bus {bus:#x} != first frame {:#x}",
        frames[0]
    );
    check!(bus & 0xfff == 0, "bus {bus:#x} is not page aligned");
    check!(bus + len <= (1u64 << 32), "run crosses 4 GiB");
    for (index, frame) in frames.iter().enumerate() {
        check!(frame & 0xfff == 0, "frame {index} misaligned");
        if index > 0 {
            check!(
                *frame == frames[index - 1] + 4096,
                "frames {index} and {} are not contiguous",
                index - 1
            );
        }
        let ptr = mem::phys_to_virt(PhysAddr::new(*frame)).as_ptr::<u8>();
        for offset in (0..4096).step_by(97) {
            // Safety: a freshly allocated pool frame is mapped readable.
            let byte = unsafe { ptr.add(offset).read_volatile() };
            check!(
                byte == 0,
                "frame {index} byte {offset} is {byte:#x}, not zero"
            );
        }
    }
    check!(
        usage(Resource::DmaMemory) == len,
        "quota {} != {len}",
        usage(Resource::DmaMemory)
    );
    check!(
        handles::count_for_task(slot) == 2,
        "handle count {}",
        handles::count_for_task(slot)
    );

    // A second allocation is disjoint from the first.
    let mut bus2 = 0u64;
    let second = expect_ok(dma_alloc(handle, 4096, 0, &mut bus2), "second")?;
    let frames2 = frames_of(second)?;
    check!(
        frames2.iter().all(|frame| !frames.contains(frame)),
        "the two runs overlap"
    );

    close(buffer)?;
    close(second)?;
    check!(pool() == base, "pool leaked: {:?} != {:?}", pool(), base);
    check!(usage(Resource::DmaMemory) == 0, "quota leaked");
    leave(&fx);
    Ok(())
}

/// The pool allocator honours page-aligned starts for larger alignments and
/// refuses a non-power-of-two alignment and a zero length.
pub fn dma_alignment() -> Result<(), String> {
    let base = pool();
    for align in [1u64, 4, 16, 64] {
        let phys = mem::dma_alloc(3, align).ok_or("dma_alloc failed")?;
        check!(
            phys.as_u64() % (align * 4096) == 0,
            "alignment {align}: {:#x}",
            phys.as_u64()
        );
        for page in 0..3u64 {
            mem::free_frame(PhysAddr::new(phys.as_u64() + page * 4096));
        }
    }
    check!(
        mem::dma_alloc(1, 3).is_none(),
        "a non-power-of-two alignment was accepted"
    );
    check!(mem::dma_alloc(0, 1).is_none(), "zero pages was accepted");
    check!(pool() == base, "pool leaked: {:?} != {:?}", pool(), base);
    Ok(())
}

/// Every hostile request fails closed with no pool, quota or handle side
/// effect.
pub fn dma_hostile_input() -> Result<(), String> {
    let fx = Fixture::new()?;
    let base = idle_pool()?;
    let dev = add_device(Spec::nic(None))?;
    let (slot, handle) = driver_with(dev)?;
    let handles_before = handles::count_for_task(slot);
    let mut bus = 0u64;

    for (what, len, flags, errno) in [
        ("zero length", 0u64, 0u64, EINVAL),
        ("u64::MAX length", u64::MAX, 0, EINVAL),
        ("rounding overflow", u64::MAX - 4095, 0, EINVAL),
        (
            "larger than the pool",
            crate::dev::dma::max_dma_bytes() + 1,
            0,
            EINVAL,
        ),
        ("unknown flag bits", 4096, 4, EINVAL),
    ] {
        expect_errno(dma_alloc(handle, len, flags, &mut bus), errno, what)?;
    }
    {
        let _strict = Strict::on();
        expect_errno(
            dma_alloc(handle, 4096, 0, &mut bus),
            EFAULT,
            "kernel out-pointer",
        )?;
        expect_errno(
            sys(OP_DMA_ALLOC, handle, 4096, 0, 0),
            EFAULT,
            "null out-pointer",
        )?;
    }
    check!(
        pool() == base,
        "hostile input touched the pool: {:?}",
        pool()
    );
    check!(
        usage(Resource::DmaMemory) == 0,
        "hostile input charged quota"
    );
    check!(
        handles::count_for_task(slot) == handles_before,
        "hostile input leaked a handle"
    );

    // Bad, foreign and stale handles.
    let bogus = handle + 4096;
    expect_errno(dma_alloc(bogus, 4096, 0, &mut bus), EBADF, "bad handle")?;
    leave(&fx);
    expect_errno(
        sys(OP_DMA_ALLOC, handle, 4096, 0, &mut bus as *mut u64 as u64),
        EBADF,
        "the kernel task cannot use a driver's device handle",
    )?;
    enter(slot)?;
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    expect_errno(
        dma_alloc(handle, 4096, 0, &mut bus),
        EBADF,
        "stale handle after release",
    )?;

    // A device whose resources carry no DMA right (a PCI bridge).
    let bridge = add_device(Spec::nic(None).with_class(0x06, 0))?;
    let bridge_handle = expect_ok(claim_plain(bridge), "claim bridge")?;
    expect_errno(
        dma_alloc(bridge_handle, 4096, 0, &mut bus),
        EPERM,
        "bridge has no DMA right",
    )?;
    check!(pool() == base, "a refused request touched the pool");
    leave(&fx);
    Ok(())
}

/// If the bus address cannot be written back (the pointer is readable but not
/// writable), the whole allocation is undone: pool, quota and handles.
pub fn dma_copy_out_failure_undoes_everything() -> Result<(), String> {
    let fx = Fixture::new()?;
    let base = idle_pool()?;
    let dev = add_device(Spec::nic(None))?;
    let (slot, handle) = driver_with(dev)?;
    let handles_before = handles::count_for_task(slot);
    // A buffer mapping made read-only is a valid readable, unwritable user
    // address (every shared-buffer mapping starts read/write).
    let ro = crate::ipc::shared::create(4096).map_err(|error| error.message().to_string())?;
    let va = crate::ipc::shared::map(ro).map_err(|error| error.message().to_string())?;
    check!(
        crate::mem::protect_range(crate::mem::kernel_table(), va, va + 4096, Prot::READ),
        "the buffer page could not be made read-only"
    );
    let handles_mid = handles::count_for_task(slot);
    let strict = Strict::on();
    crate::mem::dma::order::reset();
    expect_errno(
        sys(OP_DMA_ALLOC, handle, 4096, 0, va),
        EFAULT,
        "read-only bus-address pointer",
    )?;
    drop(strict);
    check!(
        crate::mem::dma::order::events()
            .iter()
            .all(|event| *event != crate::mem::dma::order::QUIESCE),
        "a failed allocation stopped the device"
    );
    check!(pool() == base, "the failed copy-out leaked pool pages");
    check!(
        usage(Resource::DmaMemory) == 0,
        "the failed copy-out kept a charge"
    );
    check!(
        handles::count_for_task(slot) == handles_mid && handles_mid == handles_before + 1,
        "the failed copy-out leaked a handle"
    );
    let _ = close(ro);
    leave(&fx);
    Ok(())
}

/// Fragmentation is refused cleanly: a run larger than the largest hole fails
/// while total free space would suffice, and coalescing makes it succeed.
pub fn dma_fragmentation() -> Result<(), String> {
    let fx = Fixture::new()?;
    let base = idle_pool()?;
    quota::set_limit(DRIVER_UID, Resource::DmaMemory, u64::MAX);
    let dev = add_device(Spec::nic(None))?;
    let (_slot, handle) = driver_with(dev)?;
    let total = base.total_pages;
    if total < 16 {
        serial_println!("TEST:dev_dma_fragmentation:INFO:pool too small ({total} pages)");
        leave(&fx);
        return Ok(());
    }
    let chunk = total / 8;
    let mut bus = 0u64;
    let mut buffers = Vec::new();
    for _ in 0..8 {
        buffers.push(expect_ok(
            dma_alloc(handle, chunk * 4096, 0, &mut bus),
            "fill",
        )?);
    }
    expect_errno(
        dma_alloc(handle, chunk * 4096, 0, &mut bus),
        ENOMEM,
        "pool full",
    )?;
    for index in [0usize, 2, 4, 6] {
        close(buffers[index])?;
    }
    let split = pool();
    check!(
        split.largest_run == chunk,
        "largest run {} != {chunk} after splitting",
        split.largest_run
    );
    check!(
        split.free_pages >= 2 * chunk,
        "free pages {}",
        split.free_pages
    );
    expect_errno(
        dma_alloc(handle, 2 * chunk * 4096, 0, &mut bus),
        ENOMEM,
        "fragmented request",
    )?;
    for index in [1usize, 3, 5, 7] {
        close(buffers[index])?;
    }
    check!(
        pool() == base,
        "pool not coalesced: {:?} != {:?}",
        pool(),
        base
    );
    let big = expect_ok(dma_alloc(handle, 2 * chunk * 4096, 0, &mut bus), "big")?;
    close(big)?;
    check!(pool() == base, "pool leaked after the big run");
    check!(usage(Resource::DmaMemory) == 0, "quota leaked");
    leave(&fx);
    Ok(())
}

/// The `DmaMemory` quota is exact at the boundary, released on close, and
/// isolated per uid.
pub fn dma_quota() -> Result<(), String> {
    let fx = Fixture::new()?;
    let base = idle_pool()?;
    quota::set_limit(DRIVER_UID, Resource::DmaMemory, 3 * 4096);
    let dev = add_device(Spec::nic(None))?;
    let (_slot, handle) = driver_with(dev)?;
    let mut bus = 0u64;
    let first = expect_ok(dma_alloc(handle, 4096, 0, &mut bus), "one page")?;
    check!(usage(Resource::DmaMemory) == 4096, "usage after one page");
    let second = expect_ok(dma_alloc(handle, 2 * 4096, 0, &mut bus), "two pages")?;
    check!(
        usage(Resource::DmaMemory) == 3 * 4096,
        "usage {} != 3 pages",
        usage(Resource::DmaMemory)
    );
    expect_errno(dma_alloc(handle, 4096, 0, &mut bus), EDQUOT, "over quota")?;
    check!(
        usage(Resource::DmaMemory) == 3 * 4096,
        "a refused charge changed usage"
    );
    check!(
        quota::usage(DRIVER_UID + 1, Resource::DmaMemory) == 0,
        "another uid was charged"
    );
    close(first)?;
    check!(usage(Resource::DmaMemory) == 2 * 4096, "usage after close");
    let third = expect_ok(dma_alloc(handle, 4096, 0, &mut bus), "after free")?;
    check!(
        usage(Resource::DmaMemory) == 3 * 4096,
        "usage after realloc"
    );
    close(second)?;
    close(third)?;
    check!(
        usage(Resource::DmaMemory) == 0,
        "quota leaked: {}",
        usage(Resource::DmaMemory)
    );
    check!(pool() == base, "pool leaked: {:?}", pool());
    leave(&fx);
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_dma_copy_out_failure_undoes_everything",
        dma_copy_out_failure_undoes_everything,
    ),
    ("dev_dma_layout_and_zeroing", dma_layout_and_zeroing),
    ("dev_dma_alignment", dma_alignment),
    ("dev_dma_hostile_input", dma_hostile_input),
    ("dev_dma_fragmentation", dma_fragmentation),
    ("dev_dma_quota", dma_quota),
];
