//! PCI configuration access, `list`, and the small ops of the device syscall
//! under hostile input (issue #240).

use super::fixture::*;
use super::*;
use crate::dev::class::{self, method};
use crate::dev::errno::*;
use crate::dev::report::{self, reason};
use crate::dev::syscall::*;
use crate::dev::{pci, BusId};
use crate::ipc::acl::Rule;

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

/// Configuration reads are bounds-checked; writes reach only the masked command
/// register, bus mastering needs the `DMA` right, and nothing programs a BAR.
pub fn sys_cfg_bounds_and_mask() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let dev = add_device(Spec::nic(None))?;
    let (_, handle) = claimed(dev)?;
    let read = |offset, width| sys(OP_CFG_READ, handle, offset, width, 0);
    let write = |offset, width, value| sys(OP_CFG_WRITE, handle, offset, width, value);

    check!(
        read(0, 2) == 0xFFFF,
        "a config read of an absent function gave {:#x}",
        read(0, 2)
    );
    expect_ok(read(0xFC, 4), "the last dword")?;
    expect_ok(read(0xFF, 1), "the last byte")?;
    for (what, offset, width) in [
        ("width 3", 0, 3),
        ("width 0", 0, 0),
        ("unaligned word", 1, 2),
        ("unaligned dword", 2, 4),
        ("offset 256", 0x100, 4),
        ("crossing the end", 0xFE, 4),
        ("offset overflow", u64::MAX, 1),
    ] {
        expect_errno(read(offset, width), EINVAL, what)?;
    }

    for (what, offset, width) in [
        ("BAR0 programming", 0x10, 4),
        ("the status register", 0x06, 2),
        ("a dword over command+status", 0x04, 4),
        ("a byte of the command register", 0x04, 1),
        ("the interrupt line", 0x3C, 1),
        ("the cache line size", 0x0C, 1),
    ] {
        expect_errno(write(offset, width, 0), EPERM, what)?;
    }
    expect_errno(write(0x100, 2, 0), EINVAL, "a write past config space")?;
    expect_errno(
        write(0x04, 2, 0x1_0000),
        EINVAL,
        "a value wider than the register",
    )?;
    // Decode enables are fine; bus mastering needs DMA, which the default
    // (bootstrap) policy grants a NIC, so revoke it below.
    expect_ok(write(0x04, 2, 0x0003), "enable memory and I/O")?;
    expect_ok(
        write(0x04, 2, 0x0007),
        "enable bus master with the DMA right",
    )?;
    Ok(())
}

/// Without the `DMA` right a driver cannot switch bus mastering on (and the
/// attempt is audited); clearing it is always allowed.
pub fn sys_cfg_bus_master_needs_dma() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let dev = add_device(Spec::nic(None))?;
    let allow = |method| Rule {
        actor: DRIVER_UID,
        interface_id: class::NET.interface_id,
        method,
        allow: true,
    };
    acl::load(&[allow(method::CLAIM), allow(method::MAP)]);
    let (_, handle) = claimed(dev)?;
    let write = |value| sys(OP_CFG_WRITE, handle, 0x04, 2, value);
    expect_ok(write(0x0003), "decode enables without DMA")?;
    expect_errno(write(0x0007), EPERM, "bus master without DMA")?;
    expect_errno(write(0x0004), EPERM, "bus master alone without DMA")?;
    let record = latest(dev, method::DMA).ok_or("the DMA denial was not audited")?;
    check!(
        !record.allow && record.reason_code == reason::DMA_DENIED,
        "record {record:?}"
    );
    expect_ok(write(0x0000), "clearing everything")?;
    Ok(())
}

/// Config access needs the `CONFIG` right, which platform devices never get.
pub fn sys_cfg_needs_config_right() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let dev = add_device(Spec::nic(None).platform())?;
    let (_, handle) = claimed(dev)?;
    expect_errno(
        sys(OP_CFG_READ, handle, 0, 2, 0),
        EPERM,
        "cfg_read on a platform device",
    )?;
    expect_errno(
        sys(OP_CFG_WRITE, handle, 4, 2, 0),
        EPERM,
        "cfg_write on a platform device",
    )?;
    Ok(())
}

/// Against real hardware: claiming a real function quiesces it, `cfg_read`
/// returns what enumeration found, and the function is put back afterwards.
pub fn sys_cfg_live_function() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let bridge = crate::dev::table().lock().iter().find(|info| {
        matches!(info.bus, BusId::Pci(_))
            && info.class == crate::dev::class::PCI_CLASS_BRIDGE
            && info.subclass == 0
    });
    let Some(info) = bridge else {
        serial_println!("TEST:dev_sys_cfg_live_function:INFO:no host bridge to exercise");
        return Ok(());
    };
    let BusId::Pci(address) = info.bus else {
        return Ok(());
    };
    let saved = pci::command(address);
    let (_, handle) = claimed(info.id)?;
    let vendor = expect_ok(sys(OP_CFG_READ, handle, 0, 2, 0), "read vendor")?;
    let device = expect_ok(sys(OP_CFG_READ, handle, 2, 2, 0), "read device")?;
    let class_word = expect_ok(sys(OP_CFG_READ, handle, 8, 4, 0), "read class dword")?;
    let quiet = pci::command(address);
    serial_println!(
        "TEST:dev_sys_cfg_live_function:INFO:host bridge {:04x}:{:04x} command {saved:#x} -> {quiet:#x} after claim",
        info.vendor,
        info.device
    );
    let outcome: Result<(), String> = (|| {
        check!(
            vendor == u64::from(info.vendor) && device == u64::from(info.device),
            "cfg_read saw {vendor:04x}:{device:04x}, enumeration {:04x}:{:04x}",
            info.vendor,
            info.device
        );
        check!(
            (class_word >> 24) as u8 == info.class && (class_word >> 16) as u8 == info.subclass,
            "class dword {class_word:#x}"
        );
        check!(
            quiet & (pci::COMMAND_IO | pci::COMMAND_MEMORY | pci::COMMAND_BUS_MASTER) == 0,
            "claim left decode/bus-master enabled: {quiet:#x}"
        );
        Ok(())
    })();
    expect_ok(
        sys(OP_RELEASE, handle, 0, 0, 0),
        "release the real function",
    )?;
    // Put the function back exactly as we found it.
    pci::write_command(address, saved);
    outcome
}

/// `list` reports every device without physical BAR bases, honours the buffer
/// capacity, and is gated by the capability and the ACL.
pub fn sys_list_rows_and_gates() -> Result<(), String> {
    let fx = Fixture::new()?;
    let dev = add_device(
        Spec::nic(Some(LINE_A))
            .with_bars(vec![mem_bar(0, DEVICE_MEM, 0x4000), io_bar(2, 0x700, 8)]),
    )?;
    let total = crate::dev::table().lock().len();
    let slot = spawn_driver(driver_cred())?;
    enter(slot)?;

    let mut rows = vec![0xA5A5_A5A5_A5A5_A5A5u64; total * ROW_WORDS];
    let count = expect_ok(
        sys(OP_LIST, rows.as_mut_ptr() as u64, total as u64, 0, 0),
        "list",
    )?;
    check!(
        count == total as u64,
        "list reported {count} of {total} devices"
    );
    let row = &rows[dev.0 as usize * ROW_WORDS..][..ROW_WORDS];
    check!(row[0] == u64::from(dev.0), "row id {}", row[0]);
    check!(
        row[1] & 0xFFFF == 0x1AF4 && row[1] >> 16 & 0xFFFF == 0x1000,
        "ids {:#x}",
        row[1]
    );
    check!(row[2] & 0xFF == 0x02, "class word {:#x}", row[2]);
    check!(row[3] == class::NET.interface_id, "class id {:#x}", row[3]);
    check!(
        row[4] & row_flag::PCI != 0
            && row[4] & row_flag::OWNED == 0
            && row[4] & row_flag::IRQ_ROUTABLE != 0,
        "flags {:#x}",
        row[4]
    );
    check!(
        row[4] >> 8 & 0xFF == u64::from(LINE_A),
        "irq line {:#x}",
        row[4] >> 8
    );
    check!(
        row[6] & 0xF == 1 && (row[6] >> 8) & 0xF == 3,
        "bar meta {:#x}: BAR0 should be a present memory BAR and BAR2 a present I/O BAR",
        row[6]
    );
    check!(
        row[7] == 0x4000 && row[9] == 8,
        "bar lengths {:#x} {:#x}",
        row[7],
        row[9]
    );
    check!(
        !rows.iter().any(|word| *word == DEVICE_MEM),
        "a physical BAR base leaked to userspace"
    );

    // Ownership shows up in the row, with the generation.
    let handle = expect_ok(claim_plain(dev), "claim")?;
    let mut again = vec![0u64; total * ROW_WORDS];
    expect_ok(
        sys(OP_LIST, again.as_mut_ptr() as u64, total as u64, 0, 0),
        "list again",
    )?;
    let row = &again[dev.0 as usize * ROW_WORDS..][..ROW_WORDS];
    check!(
        row[4] & row_flag::OWNED != 0,
        "the claim is not shown as owned"
    );
    let _ = handle;

    // A short buffer gets a prefix and the true total; capacity 0 writes nothing.
    let mut one = vec![0xA5A5_A5A5_A5A5_A5A5u64; 3 * ROW_WORDS];
    let count = expect_ok(
        sys(OP_LIST, one.as_mut_ptr() as u64, 2, 0, 0),
        "list capacity 2",
    )?;
    check!(count == total as u64, "short list reported {count}");
    check!(
        one[..2 * ROW_WORDS]
            .iter()
            .any(|w| *w != 0xA5A5_A5A5_A5A5_A5A5)
            && one[2 * ROW_WORDS..]
                .iter()
                .all(|w| *w == 0xA5A5_A5A5_A5A5_A5A5),
        "list wrote past its capacity"
    );
    let mut none = vec![0xA5u64; ROW_WORDS];
    expect_ok(
        sys(OP_LIST, none.as_mut_ptr() as u64, 0, 0, 0),
        "list capacity 0",
    )?;
    check!(
        none.iter().all(|w| *w == 0xA5),
        "list wrote with capacity 0"
    );

    // Hostile pointers fault cleanly.
    {
        let _strict = Strict::on();
        expect_errno(
            sys(OP_LIST, rows.as_mut_ptr() as u64, total as u64, 0, 0),
            EFAULT,
            "list into kernel memory",
        )?;
        expect_errno(
            sys(OP_LIST, u64::MAX - 8, 4, 0, 0),
            EFAULT,
            "list at the top of memory",
        )?;
    }

    // Gates: capability, then policy.
    leave(&fx);
    let plain = spawn_driver(crate::ipc::credentials::Cred::new(DRIVER_UID, 1, 0, 0, 1))?;
    enter(plain)?;
    expect_errno(
        sys(OP_LIST, rows.as_mut_ptr() as u64, 1, 0, 0),
        EPERM,
        "list without CAP_DEV_CLAIM",
    )?;
    enter(slot)?;
    acl::load(&[Rule {
        actor: DRIVER_UID,
        interface_id: class::NET.interface_id,
        method: method::CLAIM,
        allow: true,
    }]);
    expect_errno(
        sys(OP_LIST, rows.as_mut_ptr() as u64, 1, 0, 0),
        EACCES,
        "list denied by policy",
    )?;
    Ok(())
}

/// The reserved and unknown operations fail cleanly.
pub fn sys_small_ops() -> Result<(), String> {
    let _fx = Fixture::new()?;
    let dev = add_device(Spec::nic(None))?;
    let (_, handle) = claimed(dev)?;
    expect_errno(sys(OP_DMA_ALLOC, handle, 4096, 0, 0), ENOSYS, "dma_alloc")?;
    expect_errno(sys(10, 0, 0, 0, 0), EINVAL, "op 10")?;
    expect_errno(sys(u64::MAX, 0, 0, 0, 0), EINVAL, "op -1")?;
    // The whole call goes through the syscall gate's routing too.
    let via_gate = process::dispatch_for_test(23, 10, 0, 0) as i64;
    check!(via_gate == -EINVAL, "syscall 23 routed to {via_gate}");
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_sys_cfg_bounds_and_mask", sys_cfg_bounds_and_mask),
    (
        "dev_sys_cfg_bus_master_needs_dma",
        sys_cfg_bus_master_needs_dma,
    ),
    ("dev_sys_cfg_needs_config_right", sys_cfg_needs_config_right),
    ("dev_sys_cfg_live_function", sys_cfg_live_function),
    ("dev_sys_list_rows_and_gates", sys_list_rows_and_gates),
    ("dev_sys_small_ops", sys_small_ops),
];
