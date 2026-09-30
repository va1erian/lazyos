//! DMA buffer lifetime and teardown ordering (issue #241): a transferred
//! buffer survives the driver releasing the device, a `SHARE_ONLY` buffer is
//! never mapped by a client, death returns everything to baseline, and bus
//! mastering is cleared before any frame is reused.

use super::fixture::*;
use super::*;
use crate::dev::claims::CLAIMS;
use crate::dev::dma::flag as dma_flag;
use crate::dev::syscall::*;
use crate::quota::{self, Resource};

fn close(handle: u64) -> Result<(), String> {
    crate::ipc::shared::close(handle).map_err(|error| error.message().to_string())
}

/// A buffer transferred to a client survives the driver releasing the device
/// (bus mastering is off, frames stay for the reader), and the pool and quota
/// return only when the client closes the last reference.
pub fn dma_lifetime_transfer() -> Result<(), String> {
    let fx = Fixture::new()?;
    let base = idle_pool();
    let dev = add_device(Spec::nic(None))?;
    // Spawn both tasks first: `spawn_driver` leaves the kernel task current, so
    // entering the driver must come after the client exists.
    let driver = spawn_driver(driver_cred())?;
    let client = spawn_driver(driver_cred())?;
    enter(driver)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    let mut bus = 0u64;
    let buffer = expect_ok(dma_alloc(handle, 2 * 4096, 0, &mut bus), "dma_alloc")?;
    check!(
        usage(Resource::DmaMemory) == 2 * 4096,
        "charge before transfer"
    );

    let (endpoint, server) = channel_to(client)?;
    send_buffer(endpoint, buffer)?;
    check!(handles::get(buffer).is_err(), "the sender kept its handle");

    // Driver releases the device: frames must stay for the client.
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    check!(
        pool().free_pages == base.free_pages - 2,
        "frames returned before the last reference: {:?}",
        pool()
    );
    check!(
        usage(Resource::DmaMemory) == 2 * 4096,
        "charge released before the last reference"
    );

    // The client maps and reads the very frames, then closes them.
    task::harness::switch_current(client);
    mem::switch_to(PhysAddr::new(
        task::harness::pml4(client).ok_or("no table")?,
    ));
    let message = channels::try_recv(server)
        .map_err(|error| error.message().to_string())?
        .ok_or("the transferred buffer never arrived")?;
    check!(message.handles.len() == 1, "transferred handles");
    let received = message.handles[0];
    let va = crate::ipc::shared::map(received).map_err(|error| error.message().to_string())?;
    // Safety: the client mapping is readable.
    let byte = unsafe { (va as *const u8).read_volatile() };
    check!(byte == 0, "client read {byte:#x}");
    close(received)?;
    check!(
        usage(Resource::DmaMemory) == 0,
        "quota leaked after client close"
    );
    check!(
        pool() == base,
        "pool leaked after client close: {:?}",
        pool()
    );

    task::harness::switch_current(driver);
    mem::switch_to(PhysAddr::new(
        task::harness::pml4(driver).ok_or("no table")?,
    ));
    leave(&fx);
    Ok(())
}

/// A `SHARE_ONLY` DMA buffer is never mappable by a client that receives it.
pub fn dma_share_only() -> Result<(), String> {
    let fx = Fixture::new()?;
    let base = idle_pool();
    let dev = add_device(Spec::nic(None))?;
    let driver = spawn_driver(driver_cred())?;
    let client = spawn_driver(driver_cred())?;
    enter(driver)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    let mut bus = 0u64;
    let buffer = expect_ok(
        dma_alloc(handle, 4096, dma_flag::SHARE_ONLY, &mut bus),
        "share-only alloc",
    )?;
    let (endpoint, server) = channel_to(client)?;
    send_buffer(endpoint, buffer)?;

    task::harness::switch_current(client);
    mem::switch_to(PhysAddr::new(
        task::harness::pml4(client).ok_or("no table")?,
    ));
    let message = channels::try_recv(server)
        .map_err(|error| error.message().to_string())?
        .ok_or("the transferred buffer never arrived")?;
    let received = message.handles[0];
    check!(
        crate::ipc::shared::map(received) == Err(crate::ipc::shared::Error::ShareOnly),
        "a SHARE_ONLY buffer was mapped into the client"
    );
    close(received)?;
    check!(usage(Resource::DmaMemory) == 0, "quota leaked");
    check!(pool() == base, "pool leaked");
    leave(&fx);
    Ok(())
}

/// Killing and reaping a driver that holds live DMA buffers returns every
/// frame, the quota, the handles and the claims to baseline.
pub fn dma_teardown_releases_all() -> Result<(), String> {
    let fx = Fixture::new()?;
    let base = idle_pool();
    let frames_before = mem::frame_stats();
    let handles_before = quota::usage(DRIVER_UID, Resource::Handles);
    let dev = add_device(Spec::nic(None))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    let mut bus = 0u64;
    let _ = expect_ok(dma_alloc(handle, 3 * 4096, 0, &mut bus), "alloc one")?;
    let _ = expect_ok(dma_alloc(handle, 4096, 0, &mut bus), "alloc two")?;
    check!(
        usage(Resource::DmaMemory) == 4 * 4096,
        "charge before teardown"
    );
    leave(&fx);

    task::harness::finish(slot, 0);
    check!(
        task::reap_child().map(|reaped| reaped.0) == Some(slot),
        "the driver was not reaped"
    );
    check!(pool() == base, "pool leaked: {:?} != {:?}", pool(), base);
    check!(usage(Resource::DmaMemory) == 0, "quota leaked");
    check!(
        quota::usage(DRIVER_UID, Resource::Handles) == handles_before,
        "handles leaked"
    );
    check!(CLAIMS.lock().len() == 0, "claims leaked");
    let frames_after = mem::frame_stats();
    check!(
        frames_after.live() == frames_before.live()
            && frames_after.double_frees == frames_before.double_frees
            && frames_after.invalid_frees == frames_before.invalid_frees,
        "frames {} -> {}",
        frames_before.live(),
        frames_after.live()
    );
    Ok(())
}

/// Bus mastering is cleared before any DMA frame returns to the pool, on the
/// release path, the task-death path and the zombie-exit path.
pub fn dma_busmaster_ordering() -> Result<(), String> {
    use crate::mem::dma::order;
    let fx = Fixture::new()?;
    let dev = add_device(Spec::nic(None))?;
    let mut bus = 0u64;

    // (a) `release`.
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    let _ = expect_ok(dma_alloc(handle, 2 * 4096, 0, &mut bus), "alloc")?;
    order::reset();
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    let events = order::events();
    check!(quiesce_before_free(&events), "release order {events:?}");
    leave(&fx);

    // (b) task death and reap.
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    let _ = expect_ok(dma_alloc(handle, 4096, 0, &mut bus), "alloc")?;
    leave(&fx);
    order::reset();
    task::harness::finish(slot, 0);
    check!(
        task::reap_child().map(|reaped| reaped.0) == Some(slot),
        "the driver was not reaped"
    );
    let events = order::events();
    check!(quiesce_before_free(&events), "death order {events:?}");

    // (c) zombie exit: quiesce now, frames only at reap.
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    let _ = expect_ok(dma_alloc(handle, 4096, 0, &mut bus), "alloc")?;
    leave(&fx);
    order::reset();
    task::harness::finish(slot, 0);
    let zombie = order::events();
    check!(
        zombie.contains(&order::QUIESCE),
        "the zombie did not quiesce: {zombie:?}"
    );
    check!(
        !zombie.contains(&order::DMA_FREE),
        "frames returned before the reap: {zombie:?}"
    );
    check!(
        task::reap_child().map(|reaped| reaped.0) == Some(slot),
        "the zombie was not reaped"
    );
    let events = order::events();
    check!(quiesce_before_free(&events), "zombie order {events:?}");
    Ok(())
}

/// Whether every pool free in `events` follows a quiesce.
fn quiesce_before_free(events: &[u64]) -> bool {
    use crate::mem::dma::order;
    let quiesce = events.iter().position(|event| *event == order::QUIESCE);
    let free = events.iter().position(|event| *event == order::DMA_FREE);
    match (quiesce, free) {
        (Some(q), Some(f)) => q < f,
        (None, Some(_)) => false,
        _ => true,
    }
}

/// The general allocator must never hand out a pool frame that a live DMA
/// buffer holds, however much general memory is allocated.
pub fn dma_general_allocator_skips_live_pool() -> Result<(), String> {
    let fx = Fixture::new()?;
    let base = idle_pool();
    let dev = add_device(Spec::nic(None))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    let mut bus = 0u64;
    let buffer = expect_ok(dma_alloc(handle, 64 * 4096, 0, &mut bus), "dma_alloc")?;
    let (low, high) = (base.base, base.base + base.total_pages * 4096);

    // Drain the general allocator completely, then give everything back.
    let mut taken = Vec::new();
    while let Some(frame) = mem::alloc_frame() {
        taken.push(frame);
    }
    let strays = taken
        .iter()
        .filter(|frame| (low..high).contains(&frame.as_u64()))
        .count();
    for frame in taken {
        mem::free_frame(frame);
    }
    check!(strays == 0, "{strays} live pool frames were handed out");
    close(buffer)?;
    check!(pool().free_pages == base.free_pages, "pool not restored");
    leave(&fx);
    Ok(())
}

/// A driver that keeps allocating and closing DMA buffers is never stopped by
/// the per-claim record bound (16 live buffers), only by live ones.
pub fn dma_records_are_recycled() -> Result<(), String> {
    let fx = Fixture::new()?;
    let base = idle_pool();
    let dev = add_device(Spec::nic(None))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    for round in 0..64 {
        let mut bus = 0u64;
        let buffer = expect_ok(dma_alloc(handle, 4096, 0, &mut bus), "dma_alloc")?;
        check!(close(buffer).is_ok(), "round {round}: close failed");
    }
    check!(pool().free_pages == base.free_pages, "pool not restored");
    check!(usage(Resource::DmaMemory) == 0, "charge leaked");
    leave(&fx);
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_dma_general_allocator_skips_live_pool",
        dma_general_allocator_skips_live_pool,
    ),
    ("dev_dma_records_are_recycled", dma_records_are_recycled),
    ("dev_dma_lifetime_transfer", dma_lifetime_transfer),
    ("dev_dma_share_only", dma_share_only),
    ("dev_dma_teardown_releases_all", dma_teardown_releases_all),
    ("dev_dma_busmaster_ordering", dma_busmaster_ordering),
];
