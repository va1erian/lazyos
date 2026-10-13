//! A disk's life across plug and unplug: slots are reused once nothing
//! refers to the old disk, an operation started on an earlier generation
//! never reaches a later one, and the raw claim a filesystem daemon takes
//! is exclusive, generation-bound, gated and dropped with its task.

use super::*;
use crate::block::provider::{RawError, MAX_REQUEST_BYTES};
use crate::ipc::credentials::CAP_FS_PROVIDER;

const ENOENT: i64 = 2;
const EPERM: i64 = 1;
const EBUSY: i64 = 16;
const ENODEV: i64 = 19;
const EINVAL: i64 = 22;
const EROFS: i64 = 30;
const ESTALE: i64 = 116;
const ESRCH_: i64 = 3;

pub(super) fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// A daemon task (`_mountd` with `CAP_FS_PROVIDER`), made current; returns
/// its slot.
pub(super) fn daemon_task() -> Result<usize, String> {
    task::harness::switch_current(task::KERNEL_TASK);
    let slot = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
    credentials::set(
        slot,
        Cred::new(
            mounttable::MOUNTD_UID,
            mounttable::MOUNTD_UID,
            CAP_FS_PROVIDER,
            0,
            0,
        ),
    );
    // The program `mountd` starts it from: the claim gate looks at it.
    task::linuxstate::record_program(slot, fhs::bin::FATFUSE);
    task::harness::switch_current(slot);
    Ok(slot)
}

pub(super) fn disk_id() -> usize {
    with_fake(|fake| fake.disk)
}

pub(super) fn owner() -> usize {
    with_fake(|fake| fake.owner)
}

/// Make the fake serve a brand new, empty disk in slot `id` (the provider
/// registered it again).
fn fresh_store() {
    with_fake(|fake| {
        fake.data = Store::new(SECTORS as usize * SECTOR_SIZE);
    });
}

pub(super) fn generation_of(id: usize) -> Option<u64> {
    provider::list()
        .iter()
        .find(|info| info.id == id)
        .map(|info| info.generation)
}

pub fn dead_slot_is_reused() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        let (id, me) = (disk_id(), owner());
        let first = generation_of(id).ok_or("not listed")?;
        let out = [0x5Au8; SECTOR_SIZE];
        disk.write_sectors(3, &out)
            .map_err(|e| format!("write: {e:?}"))?;
        provider::remove(id, me).map_err(|e| format!("remove: {e:?}"))?;
        check!(generation_of(id).is_none(), "a dead disk is listed");
        // The next registration takes the dead disk's slot, and nothing of
        // the old one is visible through it.
        let again = provider::register(me, SECTORS, true).map_err(|e| format!("{e:?}"))?;
        check!(again == id, "registered in slot {again}, wanted {id}");
        fresh_store();
        let second = generation_of(id).ok_or("not listed again")?;
        check!(second == first + 1, "generation {first} then {second}");
        let device = crate::block::device(&format!("usb{id}")).ok_or("not in the registry")?;
        let mut back = [0xFFu8; SECTOR_SIZE];
        device
            .read_sectors(3, &mut back)
            .map_err(|e| format!("read: {e:?}"))?;
        check!(
            back.iter().all(|&b| b == 0),
            "the new disk shows the old disk's sector"
        );
        check!(device.is_writable(), "the new disk is read-only");
        Ok(())
    })();
    teardown();
    result
}

pub fn live_scanned_and_claimed_slots_are_kept() -> Result<(), String> {
    let _ = setup(Mode::Normal)?;
    let result = (|| {
        let (first, me) = (disk_id(), owner());
        // The first disk is scanned for partitions, as the late mount does:
        // views and a mount may refer to it for good.
        let scanned = provider::take_unscanned();
        check!(scanned.len() == 1, "{} disks to scan", scanned.len());
        // A dead, unscanned, unclaimed disk gives its slot to the next one.
        let second = provider::register(me, SECTORS, true).map_err(|e| format!("{e:?}"))?;
        check!(second != first, "a live disk's slot was taken");
        provider::remove(second, me).map_err(|e| format!("{e:?}"))?;
        let third = provider::register(me, SECTORS, true).map_err(|e| format!("{e:?}"))?;
        check!(third == second, "slot {third}, wanted {second}");
        // Claimed by a daemon, a dead disk keeps its slot.
        let daemon = daemon_task()?;
        let generation = generation_of(third).ok_or("third not listed")?;
        provider::claim(third, generation, daemon).map_err(|e| format!("{e:?}"))?;
        provider::remove(third, me).map_err(|e| format!("{e:?}"))?;
        provider::remove(first, me).map_err(|e| format!("{e:?}"))?;
        let fourth = provider::register(me, SECTORS, true).map_err(|e| format!("{e:?}"))?;
        check!(
            fourth != first && fourth != third,
            "a scanned or claimed slot was reused: {fourth}"
        );
        // Released, it is free for the next registration (the lowest).
        provider::release(third, daemon).map_err(|e| format!("{e:?}"))?;
        provider::remove(fourth, me).map_err(|e| format!("{e:?}"))?;
        let fifth = provider::register(me, SECTORS, true).map_err(|e| format!("{e:?}"))?;
        check!(fifth == third, "slot {fifth}, wanted {third}");
        Ok(())
    })();
    teardown();
    result
}

pub fn stale_generation_never_reaches_a_new_disk() -> Result<(), String> {
    let _ = setup(Mode::Normal)?;
    let result = (|| {
        let (id, me) = (disk_id(), owner());
        let old = generation_of(id).ok_or("not listed")?;
        let mut sector = [0u8; SECTOR_SIZE];
        provider::read_as_generation(id, old, 0, &mut sector)
            .map_err(|e| format!("read at its own generation: {e:?}"))?;
        provider::remove(id, me).map_err(|e| format!("{e:?}"))?;
        provider::register(me, SECTORS, true).map_err(|e| format!("{e:?}"))?;
        fresh_store();
        let served = with_fake(|fake| fake.served);
        // An operation that began before the unplug is Io, and the new
        // disk's provider never sees it.
        let mut buf = [0xEEu8; SECTOR_SIZE];
        expect_err(
            provider::read_as_generation(id, old, 0, &mut buf),
            BlockError::Io,
            "a stale read",
        )?;
        expect_err(
            provider::write_as_generation(id, old, 0, &[1u8; SECTOR_SIZE]),
            BlockError::Io,
            "a stale write",
        )?;
        check!(
            with_fake(|fake| fake.served) == served,
            "a stale request reached the new disk's provider"
        );
        check!(
            with_fake(|fake| fake.data.matches(0, &[0u8; SECTOR_SIZE])),
            "a stale write landed on the new disk"
        );
        Ok(())
    })();
    teardown();
    result
}

pub fn claim_cycle() -> Result<(), String> {
    let _ = setup(Mode::Normal)?;
    let result = (|| {
        let (id, me) = (disk_id(), owner());
        let generation = generation_of(id).ok_or("not listed")?;
        let daemon = daemon_task()?;
        // The wrong generation is stale; the right one is taken once.
        check!(
            provider::claim(id, generation + 1, daemon) == Err(RawError::Stale),
            "a claim on another generation"
        );
        check!(
            provider::claim(id + 3, 1, daemon) == Err(RawError::NoDisk),
            "a claim on no disk"
        );
        provider::claim(id, generation, daemon).map_err(|e| format!("{e:?}"))?;
        let other = daemon_task()?;
        check!(
            provider::claim(id, generation, other) == Err(RawError::Busy),
            "a second claim"
        );
        check!(
            provider::raw_read(id, other, 0, &mut [0u8; SECTOR_SIZE]) == Err(RawError::NotOwner),
            "a read without the claim"
        );
        check!(
            provider::release(id, other) == Err(RawError::NotOwner),
            "released by a stranger"
        );
        let mut listed = provider::list();
        listed.retain(|info| info.id == id);
        check!(listed.len() == 1 && listed[0].claimed, "the listing");
        // Raw I/O reaches the provider's store, whole sectors only.
        let data: Vec<u8> = (0..4 * SECTOR_SIZE).map(|i| (i * 7) as u8).collect();
        provider::raw_write(id, daemon, 10, &data).map_err(|e| format!("write: {e:?}"))?;
        check!(
            with_fake(|fake| fake.data.matches(10 * SECTOR_SIZE, &data)),
            "the provider stored other bytes"
        );
        let mut back = vec![0u8; data.len()];
        provider::raw_read(id, daemon, 10, &mut back).map_err(|e| format!("read: {e:?}"))?;
        check!(back == data, "read back other bytes");
        provider::raw_flush(id, daemon).map_err(|e| format!("flush: {e:?}"))?;
        check!(with_fake(|fake| fake.flushes) == 1, "the flush did not arrive");
        check!(
            provider::raw_read(id, daemon, 0, &mut [0u8; 100]) == Err(RawError::Range),
            "a partial sector"
        );
        check!(
            provider::raw_read(id, daemon, SECTORS, &mut [0u8; SECTOR_SIZE]) == Err(RawError::Range),
            "past the end"
        );
        let mut big = vec![0u8; MAX_REQUEST_BYTES + SECTOR_SIZE];
        check!(
            provider::raw_read(id, daemon, 0, &mut big) == Err(RawError::Range),
            "more than a request"
        );
        // The medium goes: ENODEV-class, and the claim still holds the slot.
        provider::remove(id, me).map_err(|e| format!("{e:?}"))?;
        check!(
            provider::raw_read(id, daemon, 0, &mut back) == Err(RawError::Gone),
            "a read of a dead disk"
        );
        check!(
            provider::claim(id, generation, other) == Err(RawError::Stale),
            "claimed a dead disk"
        );
        provider::release(id, daemon).map_err(|e| format!("release: {e:?}"))?;
        check!(
            provider::release(id, daemon) == Err(RawError::NotOwner),
            "released twice"
        );
        Ok(())
    })();
    teardown();
    result
}

pub fn claim_dies_with_its_task() -> Result<(), String> {
    let _ = setup(Mode::Normal)?;
    let result = (|| {
        let id = disk_id();
        let generation = generation_of(id).ok_or("not listed")?;
        let daemon = daemon_task()?;
        provider::claim(id, generation, daemon).map_err(|e| format!("{e:?}"))?;
        provider::teardown_task(daemon);
        let next = daemon_task()?;
        provider::claim(id, generation, next).map_err(|e| format!("not free after death: {e:?}"))?;
        Ok(())
    })();
    teardown();
    result
}

/// A disk the kernel mounted a volume through (the late `/home` mount) is
/// the kernel's: no daemon claims it, it is listed as taken, and its slot is
/// never reused.
pub fn kernel_mounted_disk_is_pinned() -> Result<(), String> {
    let _ = setup(Mode::Normal)?;
    let result = (|| {
        let (id, me) = (disk_id(), owner());
        let generation = generation_of(id).ok_or("not listed")?;
        let daemon = daemon_task()?;
        let early = daemon_task()?;
        provider::claim(id, generation, early).map_err(|e| format!("{e:?}"))?;
        provider::mark_in_use(&format!("usb{id}p1"));
        check!(
            provider::raw_read(id, early, 0, &mut [0u8; SECTOR_SIZE]) == Err(RawError::NotOwner),
            "a claim taken before the kernel mount survived it"
        );
        check!(
            provider::claim(id, generation, daemon) == Err(RawError::Busy),
            "a daemon claimed a kernel-mounted disk"
        );
        let listed = provider::list();
        let info = listed.iter().find(|info| info.id == id).ok_or("not listed")?;
        check!(info.claimed, "a kernel-mounted disk is listed as available");
        provider::remove(id, me).map_err(|e| format!("{e:?}"))?;
        let again = provider::register(me, SECTORS, true).map_err(|e| format!("{e:?}"))?;
        check!(again != id, "a kernel-mounted disk's slot was reused");
        Ok(())
    })();
    teardown();
    result
}

/// A provider keeps the handle of the disk it registered. Once that disk is
/// dead and the slot is the next one's, the old handle serves, completes and
/// removes nothing, though both are the same task: `usbd` registers the next
/// stick before it has noticed the old one is gone.
pub fn stale_provider_handle_is_refused() -> Result<(), String> {
    let _ = setup(Mode::Normal)?;
    let result = (|| {
        let (id, me) = (disk_id(), owner());
        let old = provider::handle(id);
        provider::remove(id, me).map_err(|e| format!("{e:?}"))?;
        let again = provider::register(me, SECTORS, true).map_err(|e| format!("{e:?}"))?;
        check!(again == id, "slot {again}, wanted {id}");
        let new = provider::handle(id);
        check!(new != old, "the handle did not change with the generation");
        let call = |op, handle: u64| crate::block::provider::sys::dispatch(op, handle, 0, 0, 0);
        use crate::block::provider::sys::op as provider_op;
        task::harness::switch_current(me);
        let mut record = [0u64; 4];
        let next = crate::block::provider::sys::dispatch(
            provider_op::NEXT,
            old,
            record.as_mut_ptr() as u64,
            0,
            0,
        );
        check!(next == failed(ESRCH_), "NEXT on the old handle: {next:#x}");
        check!(
            call(provider_op::COMPLETE, old) == failed(ESRCH_),
            "COMPLETE on the old handle"
        );
        check!(
            call(provider_op::REMOVE, old) == failed(ESRCH_),
            "REMOVE on the old handle"
        );
        check!(
            provider::list().iter().any(|info| info.id == id),
            "the old handle killed the new disk"
        );
        check!(
            call(provider_op::REMOVE, new) == 0,
            "the new handle could not remove its disk"
        );
        Ok(())
    })();
    teardown();
    result
}

/// Plug and unplug one stick over and over, with a claim, a write and a
/// read each time: the slot is always the same, nothing of an earlier
/// generation is ever seen, and the generation counts every time.
pub fn churn() -> Result<(), String> {
    let _ = setup(Mode::Normal)?;
    let result = (|| {
        let (id, me) = (disk_id(), owner());
        let mut generation = generation_of(id).ok_or("not listed")?;
        let daemon = daemon_task()?;
        let mut sector = [0u8; SECTOR_SIZE];
        for round in 0..400u64 {
            task::harness::switch_current(daemon);
            provider::claim(id, generation, daemon).map_err(|e| format!("claim {round}: {e:?}"))?;
            // Nothing of an earlier generation is visible.
            provider::raw_read(id, daemon, 17, &mut sector)
                .map_err(|e| format!("read {round}: {e:?}"))?;
            check!(
                sector.iter().all(|&b| b == 0),
                "round {round}: an earlier disk's sector is visible"
            );
            sector.fill(round as u8 | 1);
            provider::raw_write(id, daemon, 17, &sector)
                .map_err(|e| format!("write {round}: {e:?}"))?;
            provider::release(id, daemon).map_err(|e| format!("release {round}: {e:?}"))?;
            // Unplug, plug the next stick (a blank one) into the same slot.
            provider::remove(id, me).map_err(|e| format!("remove {round}: {e:?}"))?;
            let again = provider::register(me, SECTORS, true)
                .map_err(|e| format!("register {round}: {e:?}"))?;
            check!(again == id, "round {round}: slot {again}");
            fresh_store();
            let next = generation_of(id).ok_or("not listed")?;
            check!(next == generation + 1, "round {round}: generation {next}");
            generation = next;
        }
        Ok(())
    })();
    teardown();
    result
}
