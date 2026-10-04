//! A device table sized for a real desktop (H1 of `docs/real-pc-boot-plan.md`):
//! a Z890 board exposes well over 32 PCI functions, so the functions late in
//! bus order (the xHCI controller, a storage controller) must still be
//! recorded, attached and deliverable, and 64-bit BARs above 4 GiB must map.

use super::fixture::*;
use super::*;
use crate::dev::errno::*;
use crate::dev::syscall::{OP_MAP_BAR, OP_RELEASE};
use crate::dev::{
    attach_all, intx, irq, Bar, BarKind, BusId, DevError, DeviceHandle, DeviceId, DeviceInfo,
    DeviceTable, Driver, Resources, TaskSlot, MAX_DEVICES,
};
use crate::quota;
use spin::Mutex;

fn device(class: u8, subclass: u8, prog_if: u8) -> DeviceInfo {
    DeviceInfo {
        id: DeviceId(0),
        bus: BusId::Platform,
        vendor: 0x8086,
        device: 0x7F00,
        subsystem_vendor: 0,
        subsystem_device: 0,
        class,
        subclass,
        prog_if,
        revision: 0,
        resources: Resources::empty(),
    }
}

/// An empty scratch device table, separate from the live one. At 128 slots
/// a table is too big to build on the test stack (`Mutex::new` copies it in
/// a debug build), so the cases share this static and it is cleared on
/// every call. The suite is single-threaded, so callers never overlap.
pub(super) fn scratch_table() -> &'static Mutex<DeviceTable> {
    static TABLE: Mutex<DeviceTable> = Mutex::new(DeviceTable::new());
    TABLE.lock().clear();
    &TABLE
}

/// Attaches to one class triple and counts its attaches.
struct ClassDriver(u8, u8, u8);

impl Driver for ClassDriver {
    fn name(&self) -> &'static str {
        "class-mock"
    }

    fn matches(&self, info: &DeviceInfo) -> bool {
        (info.class, info.subclass, info.prog_if) == (self.0, self.1, self.2)
    }

    fn attach(&self, _handle: DeviceHandle) -> Result<(), DevError> {
        Ok(())
    }
}

/// A desktop-sized table: the platform seed, 90 chipset functions, then a
/// storage controller and an xHCI controller at the end of bus order. Both
/// late functions are recorded and attached; the table only refuses past
/// [`MAX_DEVICES`], with `Full`.
pub fn table_holds_a_desktop() -> Result<(), String> {
    check!(
        MAX_DEVICES >= 96,
        "MAX_DEVICES {MAX_DEVICES} too small for a desktop"
    );
    let table = scratch_table();
    {
        let mut guard = table.lock();
        guard
            .insert(device(0x01, 0x01, 0x80))
            .map_err(|e| format!("{e:?}"))?;
        for i in 0..90u8 {
            guard
                .insert(device(0x06, 0x04, i))
                .map_err(|e| format!("filler {i}: {e:?}"))?;
        }
    }
    let storage = table
        .lock()
        .insert(device(0x01, 0x06, 0x01))
        .map_err(|e| format!("{e:?}"))?;
    let xhci = table
        .lock()
        .insert(device(0x0C, 0x03, 0x30))
        .map_err(|e| format!("{e:?}"))?;
    check!(xhci.0 as usize == 92, "xHCI landed in slot {}", xhci.0);
    let drivers: [&dyn Driver; 2] = [
        &ClassDriver(0x01, 0x06, 0x01),
        &ClassDriver(0x0C, 0x03, 0x30),
    ];
    check!(
        attach_all(table, &drivers) == 2,
        "the late functions did not both attach"
    );
    {
        let guard = table.lock();
        check!(
            guard.owner(storage) == Some(TaskSlot::KERNEL),
            "storage not claimed"
        );
        check!(
            guard.owner(xhci) == Some(TaskSlot::KERNEL),
            "xHCI not claimed"
        );
    }
    let mut guard = table.lock();
    while guard.len() < MAX_DEVICES {
        guard
            .insert(device(0x06, 0x04, 0xFF))
            .map_err(|e| format!("fill: {e:?}"))?;
    }
    check!(
        guard.insert(device(0x02, 0, 0)) == Err(DevError::Full),
        "a full table accepted another device"
    );
    check!(
        guard.get(DeviceId(MAX_DEVICES as u16 - 1)).is_some(),
        "last slot not readable"
    );
    Ok(())
}

/// Stress: claim and release every slot of a full table many times; each
/// release bumps exactly that slot's generation, ids stay their slots.
pub fn table_full_claim_soak() -> Result<(), String> {
    const ROUNDS: u32 = 500;
    let mut guard = scratch_table().lock();
    let table = &mut *guard;
    for i in 0..MAX_DEVICES {
        let id = table
            .insert(device(0x06, 0x04, i as u8))
            .map_err(|e| format!("{e:?}"))?;
        check!(id.0 as usize == i, "slot {i} got id {}", id.0);
    }
    for round in 0..ROUNDS {
        for i in 0..MAX_DEVICES {
            let id = DeviceId(i as u16);
            let handle = table
                .claim(id, TaskSlot(1 + i % 7))
                .map_err(|e| format!("r{round} claim {i}: {e:?}"))?;
            table
                .release(handle)
                .map_err(|e| format!("r{round} release {i}: {e:?}"))?;
        }
    }
    for i in 0..MAX_DEVICES {
        check!(
            table.generation(DeviceId(i as u16)) == Some(ROUNDS),
            "slot {i} generation"
        );
    }
    check!(table.owned() == 0, "claims leaked");
    Ok(())
}

/// Add synthetic devices until the next one lands at `id` or later.
fn pad_table_to(id: usize) -> Result<(), String> {
    while crate::dev::table().lock().len() < id {
        add_device(Spec::nic(None).with_class(0x06, 0x04))?;
    }
    Ok(())
}

/// Interrupt delivery for claims whose device ids are past 32 and 64 (the
/// claim bitmasks used to be `u32`): two sharing claimants on one line each
/// get one message and the line unmasks once both have acked.
pub fn irq_delivery_high_device_ids() -> Result<(), String> {
    let fx = Fixture::new()?;
    pad_table_to(70)?;
    let low = super::irq::rig(LINE_A, true, true)?;
    pad_table_to(MAX_DEVICES - 2)?;
    let high = super::irq::rig(LINE_A, true, true)?;
    check!(
        low.dev.0 >= 64 && high.dev.0 as usize >= MAX_DEVICES - 2,
        "ids {} {}",
        low.dev.0,
        high.dev.0
    );
    for round in 0..50u64 {
        irq::dispatch(LINE_A);
        intx::service_at(1000 + round * 10);
        check!(
            low.queued()? == 1 && high.queued()? == 1,
            "round {round}: not both notified"
        );
        check!(
            masked(LINE_A),
            "round {round}: line unmasked with acks owed"
        );
        enter(low.slot)?;
        let (_, dev, _, _) = take_irq(low.endpoint)?;
        check!(
            dev == u32::from(low.dev.0),
            "round {round}: message names {dev}"
        );
        expect_ok(low.ack(), "low ack")?;
        check!(masked(LINE_A), "round {round}: one ack unmasked the line");
        enter(high.slot)?;
        let (_, dev, _, _) = take_irq(high.endpoint)?;
        check!(
            dev == u32::from(high.dev.0),
            "round {round}: message names {dev}"
        );
        expect_ok(high.ack(), "high ack")?;
        check!(
            !masked(LINE_A),
            "round {round}: line still masked after both acks"
        );
    }
    leave(&fx);
    Ok(())
}

fn high_bar(base: u64, len: u64) -> Bar {
    Bar {
        index: 0,
        kind: BarKind::Mem,
        base,
        len,
        is_64: true,
        prefetchable: true,
    }
}

/// A 64-bit BAR above 4 GiB maps through the claim path (repeatedly, with no
/// frame or quota leak); one past the CPU's physical address width is
/// refused instead of panicking in `PhysAddr::new`.
pub fn map_bar_above_4g() -> Result<(), String> {
    const BASE: u64 = 0x40_0000_0000; // 256 GiB: a typical 64-bit window
    let fx = Fixture::new()?;
    let limit = crate::mem::mmio::phys_limit();
    check!(limit >= 1 << 36, "phys limit {limit:#x}");
    let hostile =
        add_device(Spec::nic(None).with_bars(vec![high_bar(0xFFF0_0000_0000_0000, 0x1000)]))?;
    let past = add_device(Spec::nic(None).with_bars(vec![high_bar(limit, 0x1000)]))?;
    let dev = add_device(Spec::nic(None).with_bars(vec![high_bar(BASE, 0x4000)]))?;
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    for (what, id) in [
        ("a 64-bit BAR past 52 bits", hostile),
        ("a BAR at MAXPHYADDR", past),
    ] {
        let handle = expect_ok(claim_plain(id), "claim")?;
        expect_errno(sys(OP_MAP_BAR, handle, 0, 0, 0), EINVAL, what)?;
        expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    }
    if BASE + 0x4000 > limit {
        // A CPU with fewer than 39 physical address bits: nothing above to map.
        leave(&fx);
        return Ok(());
    }
    let frames_before = mem::frame_stats();
    let table = PhysAddr::new(task::harness::pml4(slot).ok_or("no table")?);
    for round in 0..200 {
        let handle = expect_ok(claim_plain(dev), "claim")?;
        let va = expect_ok(sys(OP_MAP_BAR, handle, 0, 0, 0), "map_bar above 4 GiB")?;
        for page in 0..4u64 {
            let entry = raw_entry(table, va + page * 4096).ok_or("page not mapped")?;
            check!(
                entry & PTE_ADDR == BASE + page * 4096,
                "round {round} page {page}: leaf {entry:#x}"
            );
        }
        expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
        check!(
            raw_entry(table, va).is_none(),
            "round {round}: mapping survived release"
        );
    }
    check!(usage(quota::Resource::UserMemory) == 0, "quota leaked");
    leave(&fx);
    let frames_after = mem::frame_stats();
    check!(
        frames_after.double_frees == frames_before.double_frees
            && frames_after.invalid_frees == frames_before.invalid_frees,
        "teardown freed a device frame as RAM"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_table_holds_a_desktop", table_holds_a_desktop),
    ("dev_table_full_claim_soak", table_full_claim_soak),
    (
        "dev_irq_delivery_high_device_ids",
        irq_delivery_high_device_ids,
    ),
    ("dev_map_bar_above_4g", map_bar_above_4g),
];
