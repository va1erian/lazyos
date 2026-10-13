//! `dbgd`'s read-only hardware ops (`dev::peek`, device syscall ops 13 and
//! 14; `LAZYOS_DBGD=1` kernels only), the rules half: who may call them,
//! what config space and the allowlist let through, the built-in holes and
//! the `lazyos.cfg` lines. The device half (claims, decode, real registers,
//! the inventory address, a soak) is `sys_peek_hw`.
//!
//! Synthetic devices sit at [`GHOST`] (config reads return all-ones), so
//! these checks touch no real function.

use alloc::vec;

use super::fixture::*;
use super::sys_peek_util::*;
use super::*;
use crate::dev::errno::*;
use crate::dev::peek;
use crate::dev::syscall::OP_DBG_CONFIG_READ;
use crate::ipc::credentials::Cred;
use pciprobe::Policy;

/// Only `dbgd` may call: root, a driver uid, a plain user and a labelled
/// dbgd are all refused before anything is looked up. The kernel task is
/// accepted in the suite only.
pub fn peek_callers() -> Result<(), String> {
    let fx = Fixture::new()?;
    add_device(Spec::nic(None))?;
    let _peek = Peek::with(Policy::with_ranges(
        false,
        &[range(0x1AF4, 0x1000, 0, 0, 0x100)],
    ));

    for (uid, label, want) in [
        (0, 0, Some(EPERM)),
        (DRIVER_UID, 0, Some(EPERM)),
        (1000, 0, Some(EPERM)),
        (DBGD, 77, Some(EACCES)),
        (DBGD, 0, None),
    ] {
        enter(spawn_driver(Cred::new(uid, uid, 0, label, 1))?)?;
        match want {
            Some(errno) => {
                expect_err(config(GHOST_BDF, 0, 4), errno, "config_read by a stranger")?;
                expect_err(mmio(GHOST_BDF, 0, 0, 4), errno, "mmio_read by a stranger")?;
            }
            None => {
                config(GHOST_BDF, 0, 4).map_err(|e| format!("dbgd refused: {e:?}"))?;
                mmio(GHOST_BDF, 0, 0, 4).map_err(|e| format!("dbgd refused: {e:?}"))?;
            }
        }
        leave(&fx);
    }
    // The kernel task itself (the suite's caller) is accepted under test.
    config(GHOST_BDF, 0, 4).map_err(|e| format!("the kernel task refused: {e:?}"))?;
    Ok(())
}

/// Config space: off without `diag.dbg=1`, the 64-byte header by default,
/// the rest of the 256 bytes with the MMIO opt-in, bounds, the function must
/// exist, and a real function's first dword is its vendor and device id.
pub fn peek_config_bounds_and_identity() -> Result<(), String> {
    let fx = Fixture::new()?;
    add_device(Spec::nic(None))?;
    enter(spawn_driver(dbgd())?)?;
    {
        let _off = Peek::with(Policy::off());
        expect_err(config(GHOST_BDF, 0, 4), EACCES, "config without diag.dbg=1")?;
    }
    {
        let _header = Peek::with(Policy::config_only());
        check!(config(GHOST_BDF, 0, 64)?.len() == 64, "the header");
        expect_err(config(GHOST_BDF, 0x40, 4), EACCES, "past the header")?;
        expect_err(config(GHOST_BDF, 0, 65), EACCES, "a span over 0x40")?;
        expect_err(
            mmio(GHOST_BDF, 0, 0, 4),
            EACCES,
            "MMIO with only diag.dbg=1",
        )?;
    }
    let _peek = Peek::with(Policy::with_ranges(false, &[]));
    check!(
        config(GHOST_BDF, 0, 256)? == vec![0xFF; 256],
        "an absent function's config space reads all-ones"
    );
    check!(config(GHOST_BDF, 255, 1)?.len() == 1, "the last byte");
    check!(config(GHOST_BDF, 3, 5)?.len() == 5, "an unaligned span");
    check!(
        config(GHOST_BDF, 1, 6)?.len() == 6,
        "a span over two dwords"
    );
    for (what, offset, len) in [
        ("zero length", 0, 0),
        ("past 256", 0, 257),
        ("starting past the end", 256, 1),
        ("a wrapping span", u64::MAX, 2),
    ] {
        expect_err(config(GHOST_BDF, offset, len), EDOM, what)?;
    }
    expect_err(
        config(0xFD00, 0, 4),
        ENODEV,
        "an address nothing enumerated",
    )?;
    expect_err(config(0x1_0000, 0, 4), EDOM, "a BDF over 16 bits")?;
    // A faulting buffer is EFAULT, with nothing leaked.
    {
        let _strict = Strict::on();
        let mut kernel_buf = [0u8; 8];
        expect_errno(
            sys(
                OP_DBG_CONFIG_READ,
                GHOST_BDF,
                0,
                4,
                kernel_buf.as_mut_ptr() as u64,
            ),
            EFAULT,
            "a kernel pointer as the buffer",
        )?;
    }
    // A real function: the first dword is what the inventory says.
    leave(&fx);
    let real = crate::dev::table()
        .lock()
        .iter()
        .find(|d| matches!(d.bus, BusId::Pci(a) if a != GHOST));
    if let Some(info) = real {
        let bdf = bdf_of(&info).ok_or("not a PCI function")?;
        enter(spawn_driver(dbgd())?)?;
        let id = config(bdf, 0, 4)?;
        check!(
            id == [
                info.vendor as u8,
                (info.vendor >> 8) as u8,
                info.device as u8,
                (info.device >> 8) as u8
            ],
            "config dword 0 is {id:02x?}, the table says {:04x}:{:04x}",
            info.vendor,
            info.device
        );
    }
    Ok(())
}

/// The allowlist is the only way in: off by default, nothing outside a
/// listed range, alignment and length bounds.
pub fn peek_mmio_allowlist() -> Result<(), String> {
    let _fx = Fixture::new()?;
    add_with_ids(0x1AF4, 0x1000, vec![mem_bar(0, WINDOW, 0x1000)])?;
    enter(spawn_driver(dbgd())?)?;

    // Off: even a built-in range is refused.
    expect_err(mmio(GHOST_BDF, 0, 0, 4), EACCES, "MMIO with the tier off")?;

    // On, no ranges for this device.
    let _peek = Peek::with(Policy::with_ranges(false, &[]));
    expect_err(mmio(GHOST_BDF, 0, 0, 4), EACCES, "a device with no range")?;
    drop(_peek);

    let _peek = Peek::with(Policy::with_ranges(
        false,
        &[range(0x1AF4, 0x1000, 0, 0x10, 0x30)],
    ));
    check!(
        mmio(GHOST_BDF, 0, 0x10, 0x20)?.len() == 0x20,
        "the whole range"
    );
    check!(mmio(GHOST_BDF, 0, 0x2C, 4)?.len() == 4, "the last dword");
    expect_err(
        mmio(GHOST_BDF, 0, 0x0C, 4),
        EACCES,
        "a dword before the range",
    )?;
    expect_err(
        mmio(GHOST_BDF, 0, 0x30, 4),
        EACCES,
        "a dword after the range",
    )?;
    expect_err(
        mmio(GHOST_BDF, 0, 0x2C, 8),
        EACCES,
        "a span leaving the range",
    )?;
    expect_err(mmio(GHOST_BDF, 1, 0x10, 4), EACCES, "another BAR")?;
    expect_err(mmio(GHOST_BDF, 0, 0x11, 4), EDOM, "an unaligned offset")?;
    expect_err(mmio(GHOST_BDF, 0, 0x10, 6), EDOM, "an unaligned length")?;
    expect_err(mmio(GHOST_BDF, 0, 0x10, 0), EDOM, "no length")?;
    expect_err(mmio(GHOST_BDF, 0, 0x10, 4100), EDOM, "over 4 KiB")?;
    expect_err(mmio(GHOST_BDF, 6, 0x10, 4), EDOM, "BAR 6")?;
    expect_err(mmio(0xFD00, 0, 0x10, 4), ENODEV, "no such function")?;
    Ok(())
}

/// With the built-in table the Kaby Lake GMBUS block and the three ELD
/// ports are holes while a pipe register is reachable. The synthetic
/// function's BAR is the HPET window, so the permitted read is harmless.
pub fn peek_builtin_holes() -> Result<(), String> {
    let fx = Fixture::new()?;
    add_with_ids(0x8086, 0x5916, vec![mem_bar(0, WINDOW, 16 << 20)])?;
    enter(spawn_driver(dbgd())?)?;
    let _peek = Peek::with(Policy::with_ranges(false, &[]));
    for gmbus in [0xC5100u64, 0xC5108, 0xC510C, 0xC5120, 0xC512C] {
        expect_err(mmio(GHOST_BDF, 0, gmbus, 4), EACCES, "a GMBUS register")?;
    }
    for port in [0x65050u64, 0x65150, 0x65250] {
        for dword in [port, port + 4] {
            expect_err(mmio(GHOST_BDF, 0, dword, 4), EACCES, "an ELD data port")?;
        }
        expect_err(
            mmio(GHOST_BDF, 0, port - 4, 12),
            EACCES,
            "a span over an ELD port",
        )?;
        check!(
            mmio(GHOST_BDF, 0, port - 4, 4)?.len() == 4,
            "the dword before"
        );
        check!(
            mmio(GHOST_BDF, 0, port + 8, 4)?.len() == 4,
            "the dword after"
        );
    }
    expect_err(
        mmio(GHOST_BDF, 0, 0xC50F0, 0x40),
        EACCES,
        "a span over GMBUS",
    )?;
    check!(mmio(GHOST_BDF, 0, 0x70008, 4)?.len() == 4, "PIPECONF A");
    check!(
        mmio(GHOST_BDF, 0, 0x60000, 0x40)?.len() == 0x40,
        "transcoder timings"
    );
    leave(&fx);
    Ok(())
}

/// `apply_config` reads the keys, ignores them without `diag.dbg=1`, and a
/// later read of a different text replaces them.
pub fn peek_config_lines() -> Result<(), String> {
    let fx = Fixture::new()?;
    add_with_ids(0x1AF4, 0x1000, vec![mem_bar(0, WINDOW, 0x1000)])?;
    enter(spawn_driver(dbgd())?)?;
    let _peek = Peek::with(Policy::off());
    peek::apply_config("diag.dbg.mmio=1\ndiag.dbg.mmio.allow=1af4:1000:0:0-100\n");
    expect_err(mmio(GHOST_BDF, 0, 0, 4), EACCES, "no diag.dbg=1")?;
    expect_err(config(GHOST_BDF, 0, 4), EACCES, "no diag.dbg=1, config")?;
    peek::apply_config("diag.dbg=1\ndiag.dbg.mmio.allow=1af4:1000:0:0-100\n");
    check!(
        config(GHOST_BDF, 0, 64)?.len() == 64,
        "the header with diag.dbg=1"
    );
    expect_err(
        mmio(GHOST_BDF, 0, 0, 4),
        EACCES,
        "an allow line without the opt-in",
    )?;
    peek::apply_config("diag.dbg=1\ndiag.dbg.mmio=1\ndiag.dbg.mmio.allow=1af4:1000:0:0-100\n");
    check!(
        mmio(GHOST_BDF, 0, 0, 0x100)?.len() == 0x100,
        "the allow line"
    );
    check!(
        config(GHOST_BDF, 0, 256)?.len() == 256,
        "the whole config space"
    );
    expect_err(mmio(GHOST_BDF, 0, 0x100, 4), EACCES, "past the allow line")?;
    peek::apply_config("");
    expect_err(
        mmio(GHOST_BDF, 0, 0, 4),
        EACCES,
        "after the box turned it off",
    )?;
    leave(&fx);
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_peek_callers", peek_callers),
    (
        "dev_peek_config_bounds_and_identity",
        peek_config_bounds_and_identity,
    ),
    ("dev_peek_mmio_allowlist", peek_mmio_allowlist),
    ("dev_peek_builtin_holes", peek_builtin_holes),
    ("dev_peek_config_lines", peek_config_lines),
];
