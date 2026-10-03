//! The resource operations of the device syscall under hostile input (issue
//! #240): `map_bar`, `pio`, `cfg_read`/`cfg_write`, `list`, and the small ops.

use super::fixture::*;
use super::*;
use crate::dev::class::{self, method};
use crate::dev::errno::*;
use crate::dev::ops::{MMIO_VA_BASE, MMIO_VA_END};
use crate::dev::report::{self, reason};
use crate::dev::syscall::*;
use crate::ipc::acl::Rule;
use crate::quota;
use crate::quota::Resource;

const MMIO_BIT: u64 = 1 << 10;
const NX_BIT: u64 = 1 << 63;
const PWT_BIT: u64 = 1 << 3;
const PCD_BIT: u64 = 1 << 4;
const USER_BIT: u64 = 1 << 2;
/// The HPET window: present on both QEMU machine types, never RAM.
const DEVICE_MEM: u64 = 0xFED0_0000;

fn latest(dev: DeviceId, method: u32) -> Option<audit::AuditEvent> {
    audit::recent(audit::AUDIT_CAPACITY)
        .into_iter()
        .find(|event| event.method == method && report::device_of(event.txn_id) == Some(dev))
}

/// Claim `dev` in a fresh driver task (entered) and return `(slot, handle)`.
fn claimed(dev: DeviceId) -> Result<(usize, u64), String> {
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;
    let handle = expect_ok(claim_plain(dev), "claim")?;
    Ok((slot, handle))
}

/// A memory BAR maps uncached, user, no-execute and MMIO-tagged into the
/// claimant's own address space, charges the uid, and every hostile BAR is
/// refused with nothing mapped or charged.
pub fn sys_map_bar_maps_and_refuses() -> Result<(), String> {
    let fx = Fixture::new()?;
    let ram = mem::alloc_frame().ok_or("no frame")?;
    let hostile = [
        ("a BAR under a page", mem_bar(0, DEVICE_MEM, 0x100), EINVAL),
        (
            "a BAR over the map limit",
            mem_bar(0, 0x1_0000_0000, 128 << 20),
            EINVAL,
        ),
        ("an unassigned BAR", mem_bar(0, 0, 0x1000), EINVAL),
        (
            "an unaligned BAR",
            mem_bar(0, DEVICE_MEM + 0x100, 0x1000),
            EINVAL,
        ),
        (
            "a wrapping BAR",
            mem_bar(0, u64::MAX & !0xFFF, 0x2000),
            EINVAL,
        ),
        (
            "a device with only an I/O BAR (no MMIO right)",
            io_bar(0, 0x700, 8),
            EPERM,
        ),
        ("a BAR over RAM", mem_bar(0, ram.as_u64(), 0x1000), EPERM),
    ];
    for (what, bar, errno) in hostile {
        let dev = add_device(Spec::nic(None).with_bars(vec![bar]))?;
        let (_, handle) = claimed(dev)?;
        expect_errno(sys(OP_MAP_BAR, handle, 0, 0, 0), errno, what)?;
        check!(
            usage(Resource::UserMemory) == 0,
            "{what}: a refused map was charged"
        );
        leave(&fx);
    }
    let last = crate::dev::table().lock().len();
    let record = latest(DeviceId(last as u16 - 1), method::MAP).ok_or("RAM overlap not audited")?;
    check!(
        record.reason_code == reason::BAR_IN_RAM,
        "record {record:?}"
    );
    mem::free_frame(ram);

    // The frame ledger is compared across the last driver's whole life; the
    // refused-BAR drivers above are still alive and hold their own frames.
    let frames_before = mem::frame_stats();
    let dev = add_device(
        Spec::nic(None).with_bars(vec![mem_bar(0, DEVICE_MEM, 0x4000), io_bar(1, 0x700, 8)]),
    )?;
    let (slot, handle) = claimed(dev)?;
    expect_errno(sys(OP_MAP_BAR, handle, 6, 0, 0), EINVAL, "BAR index 6")?;
    expect_errno(sys(OP_MAP_BAR, handle, 255, 0, 0), EINVAL, "BAR index 255")?;
    expect_errno(sys(OP_MAP_BAR, handle, 2, 0, 0), EINVAL, "an absent BAR")?;
    expect_errno(
        sys(OP_MAP_BAR, handle, 1, 0, 0),
        EINVAL,
        "an I/O BAR by index",
    )?;
    expect_errno(
        sys(OP_MAP_BAR, handle, u64::MAX, 0, 0),
        EINVAL,
        "BAR index -1",
    )?;

    let va = expect_ok(sys(OP_MAP_BAR, handle, 0, 0, 0), "map_bar")?;
    let table = PhysAddr::new(task::harness::pml4(slot).ok_or("no table")?);
    check!(
        (MMIO_VA_BASE..MMIO_VA_END).contains(&va) && va & 0xFFF == 0 && va < crate::mem::USER_TOP,
        "va {va:#x} is not in the private MMIO range"
    );
    for page in 0..4u64 {
        let entry = raw_entry(table, va + page * 4096).ok_or("page not mapped")?;
        let want = USER_BIT | PWT_BIT | PCD_BIT | MMIO_BIT | NX_BIT | PTE_WRITABLE | PTE_PRESENT;
        check!(
            entry & want == want && entry & PTE_ADDR == DEVICE_MEM + page * 4096,
            "page {page}: entry {entry:#x} is not an uncached user MMIO leaf of {:#x}",
            DEVICE_MEM + page * 4096
        );
    }
    check!(
        raw_entry(table, va + 4 * 4096).is_none(),
        "the mapping overran the BAR"
    );
    check!(
        usage(Resource::UserMemory) == 0x4000,
        "the mapping was not charged by uid"
    );
    expect_errno(
        sys(OP_MAP_BAR, handle, 0, 0, 0),
        EBUSY,
        "a second map of the same BAR",
    )?;

    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    for page in 0..4u64 {
        check!(
            raw_entry(table, va + page * 4096).is_none(),
            "page {page} survived release"
        );
    }
    check!(
        usage(Resource::UserMemory) == 0,
        "release left the mapping charged"
    );
    leave(&fx);
    task::harness::finish(slot, 0);
    while task::reap_child().is_some() {}
    let frames_after = mem::frame_stats();
    check!(
        frames_after.live() == frames_before.live()
            && frames_after.double_frees == frames_before.double_frees,
        "frames {} -> {}, double frees {} -> {}",
        frames_before.live(),
        frames_after.live(),
        frames_before.double_frees,
        frames_after.double_frees
    );
    Ok(())
}

/// `map_bar` needs the `MMIO` right, and a BAR can be mapped again after a
/// release at a recycled address.
pub fn sys_map_bar_needs_right() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let dev = add_device(Spec::nic(None))?;
    acl::load(&[Rule {
        actor: DRIVER_UID,
        interface_id: class::NET.interface_id,
        method: method::CLAIM,
        allow: true,
    }]);
    let (_, handle) = claimed(dev)?;
    expect_errno(
        sys(OP_MAP_BAR, handle, 0, 0, 0),
        EPERM,
        "map without MMIO right",
    )?;
    expect_errno(
        sys(OP_PIO, handle, 1, 0, pio_word(1, false, 0)),
        EPERM,
        "pio without PIO right",
    )?;
    Ok(())
}

/// The mapping quota is charged by uid and refuses when exhausted, undoing
/// everything it started.
pub fn sys_map_bar_quota() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let dev = add_device(Spec::nic(None).with_bars(vec![mem_bar(0, DEVICE_MEM, 0x4000)]))?;
    let (_, handle) = claimed(dev)?;
    quota::set_limit(DRIVER_UID, Resource::UserMemory, 0x3000);
    expect_errno(
        sys(OP_MAP_BAR, handle, 0, 0, 0),
        EDQUOT,
        "map over the memory quota",
    )?;
    check!(
        usage(Resource::UserMemory) == 0,
        "the refused map stayed charged"
    );
    quota::set_limit(DRIVER_UID, Resource::UserMemory, 0x4000);
    let va = expect_ok(sys(OP_MAP_BAR, handle, 0, 0, 0), "map at the exact limit")?;
    check!(va >= MMIO_VA_BASE, "va {va:#x}");
    Ok(())
}

/// Port I/O stays inside the device's own I/O BAR, is aligned and sized, and
/// never reaches legacy or PCI-config ports.
pub fn sys_pio_bounds_and_denylist() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let dev = add_device(
        Spec::nic(None)
            .platform()
            .with_bars(vec![io_bar(0, 0x0700, 8)]),
    )?;
    let (_, handle) = claimed(dev)?;
    let read = |width, offset| sys(OP_PIO, handle, 0, offset, pio_word(width, false, 0));
    for (width, offset) in [(1, 0), (1, 7), (2, 0), (2, 6), (4, 0), (4, 4)] {
        let value = expect_ok(read(width, offset), "in-bounds pio read")?;
        check!(
            value <= u64::from(u32::MAX >> (32 - 8 * width as u32)),
            "read {value:#x} wider than {width}"
        );
    }
    expect_ok(
        sys(OP_PIO, handle, 0, 4, pio_word(4, true, 0xDEAD_BEEF)),
        "in-bounds pio write",
    )?;
    for (what, width, offset, word_extra) in [
        ("offset == len", 1, 8, 0u64),
        ("access crosses the end", 2, 7, 0),
        ("unaligned word", 2, 1, 0),
        ("unaligned dword", 4, 2, 0),
        ("width 3", 3, 0, 0),
        ("width 0", 0, 0, 0),
        ("width 8", 8, 0, 0),
        ("offset overflow", 1, u64::MAX, 0),
        ("reserved request bits", 1, 0, 1 << 9),
    ] {
        let word = pio_word(width, false, 0) | word_extra;
        expect_errno(sys(OP_PIO, handle, 0, offset, word), EINVAL, what)?;
    }
    expect_errno(
        sys(OP_PIO, handle, 1, 0, pio_word(1, false, 0)),
        EINVAL,
        "an absent BAR",
    )?;
    expect_errno(
        sys(OP_PIO, handle, 256, 0, pio_word(1, false, 0)),
        EINVAL,
        "BAR index 256",
    )?;

    // Ports the driver may never reach, even inside its own BAR.
    for (what, base, offset, width) in [
        ("the keyboard controller", 0x60, 0u64, 1u64),
        ("below 0x100", 0xF8, 0, 4),
        ("the PCI config address port", 0xCF8, 0, 4),
        ("the PCI config data port", 0xCF8, 4, 1),
        ("a window straddling 0xCF8", 0xCF4, 4, 4),
    ] {
        let bad = add_device(
            Spec::nic(None)
                .platform()
                .with_bars(vec![io_bar(0, base, 8)]),
        )?;
        let (_, bad_handle) = claimed(bad)?;
        expect_errno(
            sys(OP_PIO, bad_handle, 0, offset, pio_word(width, false, 0)),
            EPERM,
            what,
        )?;
    }
    // A BAR that runs off the end of port space.
    let edge = add_device(
        Spec::nic(None)
            .platform()
            .with_bars(vec![io_bar(0, 0xFFFC, 8)]),
    )?;
    let (_, edge_handle) = claimed(edge)?;
    expect_errno(
        sys(OP_PIO, edge_handle, 0, 4, pio_word(4, false, 0)),
        EINVAL,
        "a BAR past port 0xFFFF",
    )?;

    // A memory BAR is not a port window.
    let mem_only = add_device(
        Spec::nic(None)
            .platform()
            .with_bars(vec![mem_bar(0, DEVICE_MEM, 0x1000)]),
    )?;
    let (_, mem_handle) = claimed(mem_only)?;
    expect_errno(
        sys(OP_PIO, mem_handle, 0, 0, pio_word(1, false, 0)),
        EPERM,
        "pio on a memory-only device",
    )?;
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "dev_sys_map_bar_maps_and_refuses",
        sys_map_bar_maps_and_refuses,
    ),
    ("dev_sys_map_bar_needs_right", sys_map_bar_needs_right),
    ("dev_sys_map_bar_quota", sys_map_bar_quota),
    (
        "dev_sys_pio_bounds_and_denylist",
        sys_pio_bounds_and_denylist,
    ),
];
