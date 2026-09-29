//! Releasing claims when the owner dies, and the MMIO page-table bookkeeping
//! that must never treat a device frame as RAM (issue #240).

use super::fixture::*;
use super::irq::rig;
use super::*;
use crate::dev::claims::CLAIMS;
use crate::dev::class::method;
use crate::dev::errno::*;
use crate::dev::intx;
use crate::dev::report::{self, reason};
use crate::dev::syscall::*;
use crate::mem::mmio;
use crate::quota;
use crate::quota::Resource;

const DEVICE_MEM: u64 = 0xFED0_0000;
const VA: u64 = 0x0000_0030_0000_0000;

/// Kill `slot` through the real reap path (which runs `ipc::teardown_task`).
fn kill(slot: usize) -> Result<(), String> {
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(slot, 0);
    let reaped = task::reap_child().ok_or("the finished task was not reaped")?;
    check!(
        reaped.0 == slot,
        "reaped slot {} instead of {slot}",
        reaped.0
    );
    Ok(())
}

fn latest(dev: DeviceId, method: u32) -> Option<audit::AuditEvent> {
    audit::recent(audit::AUDIT_CAPACITY)
        .into_iter()
        .find(|event| event.method == method && report::device_of(event.txn_id) == Some(dev))
}

/// A task that dies holding mapped, armed, pending claims leaves nothing
/// behind: devices are free with advanced generations, IRQs masked, quotas and
/// handles returned, the MMIO VA recycled, frames untouched, and one audit
/// record per claim.
pub fn teardown_releases_everything() -> Result<(), String> {
    let fx = Fixture::new()?;
    let frames_before = mem::frame_stats();
    let handles_before = quota::usage(DRIVER_UID, Resource::Handles);
    let mapped =
        add_device(Spec::nic(Some(LINE_A)).with_bars(vec![mem_bar(0, DEVICE_MEM, 0x2000)]))?;
    let quiet = add_device(Spec::nic(None))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let (endpoint, _peer) = irq_channel()?;
    let first = expect_ok(claim_irq(mapped, endpoint, false), "claim mapped")?;
    let second = expect_ok(claim_plain(quiet), "claim quiet")?;
    expect_ok(sys(OP_IRQ_ENABLE, first, 0, 0, 0), "irq_enable")?;
    let va = expect_ok(sys(OP_MAP_BAR, first, 0, 0, 0), "map_bar")?;
    let _ = second;
    fire(LINE_A, 10);
    check!(masked(LINE_A), "the interrupt is not in flight");
    let generations = [table_state(mapped).1, table_state(quiet).1];
    leave(&fx);

    kill(slot)?;
    for (dev, generation) in [(mapped, generations[0]), (quiet, generations[1])] {
        check!(
            table_state(dev) == (None, generation + 1),
            "device {} after teardown: {:?}",
            dev.0,
            table_state(dev)
        );
        let record = latest(dev, method::RELEASE).ok_or("the teardown was not audited")?;
        check!(
            record.allow && record.reason_code == reason::TEARDOWN && record.actor_slot == slot,
            "teardown record {record:?}"
        );
    }
    check!(CLAIMS.lock().len() == 0, "claims survived their owner");
    check!(masked(LINE_A), "the dead claimant's line is still unmasked");
    check!(
        usage(Resource::DeviceClaims) == 0 && usage(Resource::UserMemory) == 0,
        "quota leaked: claims {} memory {}",
        usage(Resource::DeviceClaims),
        usage(Resource::UserMemory)
    );
    check!(
        quota::usage(DRIVER_UID, Resource::Handles) == handles_before,
        "handles leaked"
    );
    let frames_after = mem::frame_stats();
    check!(
        frames_after.live() == frames_before.live()
            && frames_after.double_frees == frames_before.double_frees,
        "frames {} -> {} (double frees {} -> {})",
        frames_before.live(),
        frames_after.live(),
        frames_before.double_frees,
        frames_after.double_frees
    );

    // The device is claimable again and the MMIO address range was recycled.
    let next = spawn_driver(driver_cred())?;
    enter(next)?;
    let handle = expect_ok(claim_plain(mapped), "re-claim after teardown")?;
    let again = expect_ok(sys(OP_MAP_BAR, handle, 0, 0, 0), "re-map")?;
    check!(
        again == va,
        "the MMIO range {va:#x} was not recycled ({again:#x})"
    );
    Ok(())
}

/// A dying claimant leaves every delivery round it was in: the line unmasks
/// as soon as the last live waiter is gone, and stays masked with no listener.
pub fn teardown_leaves_shared_rounds() -> Result<(), String> {
    let fx = Fixture::new()?;
    let start = 1_000u64;
    let timeouts_before = intx::counters().1;

    // The dead claimant is one of two waiters: the survivor still holds the line.
    let (a, b) = (rig(LINE_A, true, true)?, rig(LINE_A, true, true)?);
    fire(LINE_A, start);
    kill(b.slot)?;
    check!(masked(LINE_A), "the line unmasked with a live waiter");
    enter(a.slot)?;
    take_irq(a.endpoint)?;
    expect_ok(a.ack(), "survivor's ack")?;
    check!(!masked(LINE_A), "the survivor's ack did not end the round");

    // The dead claimant is the last waiter.
    let c = rig(LINE_A, true, true)?;
    fire(LINE_A, start + 10);
    enter(a.slot)?;
    take_irq(a.endpoint)?;
    expect_ok(a.ack(), "ack")?;
    check!(
        masked(LINE_A),
        "the line unmasked while claimant c owes an ack"
    );
    kill(c.slot)?;
    check!(
        !masked(LINE_A),
        "the last waiter's death did not release the line"
    );

    // No listener left: the line is masked.
    kill(a.slot)?;
    check!(masked(LINE_A), "a line with no claimant was left unmasked");
    check!(CLAIMS.lock().len() == 0, "claims leaked");
    intx::service_at(start + 10_000);
    check!(
        intx::counters().1 == timeouts_before,
        "a dead claimant caused a timeout"
    );
    leave(&fx);
    Ok(())
}

/// Teardown while the address space lives on (a thread group): the mapping
/// disappears from the live table, and the still-open handle fails closed.
pub fn teardown_unmaps_live_space() -> Result<(), String> {
    let fx = Fixture::new()?;
    let dev = add_device(Spec::nic(None).with_bars(vec![mem_bar(0, DEVICE_MEM, 0x3000)]))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    let va = expect_ok(sys(OP_MAP_BAR, handle, 0, 0, 0), "map")?;
    let table = task::harness::pml4(slot).ok_or("no table")?;
    check!(raw_entry(PhysAddr::new(table), va).is_some(), "not mapped");
    leave(&fx);

    crate::dev::teardown_task(slot, table);
    for page in 0..3u64 {
        check!(
            raw_entry(PhysAddr::new(table), va + page * 4096).is_none(),
            "page {page} still mapped after teardown"
        );
    }
    check!(usage(Resource::UserMemory) == 0, "mapping still charged");
    enter(slot)?;
    check!(
        handles::get(handle).is_ok(),
        "the handle entry vanished before the fabric teardown"
    );
    for op in [OP_MAP_BAR, OP_CFG_READ, OP_IRQ_ACK, OP_RELEASE] {
        expect_errno(
            sys(op, handle, 0, 2, 0),
            EBADF,
            "a handle used after teardown",
        )?;
    }
    Ok(())
}

/// MMIO leaves are never counted as RAM: unmap, free and fork all skip them.
pub fn mem_mmio_never_treated_as_ram() -> Result<(), String> {
    let before = mem::frame_stats();
    let table = mem::new_user_table().ok_or("no table")?;
    check!(mmio::map_mmio(table, VA, DEVICE_MEM, 8), "map_mmio failed");
    for page in 0..8u64 {
        let entry = raw_entry(table, VA + page * 4096).ok_or("page missing")?;
        check!(
            entry & (1 << 10) != 0 && entry & PTE_ADDR == DEVICE_MEM + page * 4096,
            "leaf {entry:#x}"
        );
    }
    // A normal page next to it, for fork to share.
    process::map_range(table, TEST_VA, TEST_VA + 4096).map_err(to_string)?;
    let child = mem::clone_user_table(table).ok_or("clone failed")?;
    check!(
        raw_entry(child, VA).is_none(),
        "fork inherited an MMIO mapping"
    );
    check!(
        raw_entry(child, TEST_VA).is_some() && raw_entry(table, VA).is_some(),
        "fork lost a normal page or the parent's MMIO"
    );
    mem::free_user_table(child);

    // Unmapping only clears the leaf whose frame it expects.
    check!(
        mmio::unmap_mmio(table, VA, DEVICE_MEM + 4096, 1) == 0,
        "unmap_mmio hit a mismatched frame"
    );
    check!(
        mmio::unmap_mmio(table, TEST_VA, 0, 1) == 0,
        "unmap_mmio cleared a RAM leaf"
    );
    check!(
        raw_entry(table, TEST_VA).is_some(),
        "a RAM page lost its mapping"
    );
    check!(
        mmio::unmap_mmio(table, VA, DEVICE_MEM, 4) == 4,
        "unmap_mmio did not clear its pages"
    );
    check!(
        raw_entry(table, VA).is_none() && raw_entry(table, VA + 4 * 4096).is_some(),
        "wrong pages cleared"
    );
    // The rest go with the table, which must not free device frames.
    mem::free_user_table(table);
    let after = mem::frame_stats();
    check!(
        after.live() == before.live() && after.double_frees == before.double_frees,
        "frames {} -> {} (double frees {} -> {})",
        before.live(),
        after.live(),
        before.double_frees,
        after.double_frees
    );
    Ok(())
}

/// `overlaps_ram` refuses ranges touching RAM, allows device windows.
pub fn mem_mmio_overlap_detection() -> Result<(), String> {
    let ram = mem::alloc_frame().ok_or("no frame")?;
    let base = ram.as_u64();
    check!(
        mmio::overlaps_ram(base, base + 4096),
        "a RAM frame was not detected"
    );
    check!(
        mmio::overlaps_ram(base - 4096, base + 1),
        "a range ending inside RAM was missed"
    );
    check!(
        mmio::overlaps_ram(base + 4095, base + 8192),
        "a range starting inside RAM was missed"
    );
    mem::free_frame(ram);
    check!(
        !mmio::overlaps_ram(DEVICE_MEM, DEVICE_MEM + 0x4000),
        "the HPET window looks like RAM"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_teardown_releases_everything",
        teardown_releases_everything,
    ),
    (
        "dev_teardown_leaves_shared_rounds",
        teardown_leaves_shared_rounds,
    ),
    ("dev_teardown_unmaps_live_space", teardown_unmaps_live_space),
    (
        "dev_mem_mmio_never_treated_as_ram",
        mem_mmio_never_treated_as_ram,
    ),
    ("dev_mem_mmio_overlap_detection", mem_mmio_overlap_detection),
];
