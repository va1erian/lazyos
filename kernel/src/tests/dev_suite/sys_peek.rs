//! `dbgd`'s read-only hardware ops (`dev::peek`, device syscall ops 13 and
//! 14; `LAZYOS_DBGD=1` kernels only): who may call them, what the allowlist
//! lets through, what a claimed or quiesced device does, that no mapping or
//! allocation leaks, and a soak that mixes reads with claims and refusals.
//!
//! Synthetic devices sit at [`GHOST`] (config reads return all-ones), so the
//! config and rule checks touch no real function; the one case that reads
//! real registers uses QEMU's standard VGA when the machine has it.

use alloc::vec;
use alloc::vec::Vec;

use super::fixture::*;
use super::*;
use crate::dev::errno::*;
use crate::dev::peek;
use crate::dev::syscall::{OP_DBG_CONFIG_READ, OP_DBG_MMIO_READ};
use crate::ipc::credentials::Cred;
use pciprobe::{Policy, Range};

const DBGD: u32 = 914;
const FILL: u8 = 0xA5;
/// A bare address no machine decodes besides HPET: reads return the
/// controller's registers or all-ones, never fault.
const WINDOW: u64 = 0xFED0_0000;
/// The synthetic function's BDF: bus 0xFE, device 0, function 0.
const GHOST_BDF: u64 = 0xFE00;

/// Puts the policy back to "off" on every exit path.
struct Peek;

impl Peek {
    fn with(policy: Policy) -> Peek {
        peek::set_policy_for_test(policy);
        Peek
    }
}

impl Drop for Peek {
    fn drop(&mut self) {
        peek::reset_for_test();
    }
}

fn dbgd() -> Cred {
    Cred::new(DBGD, DBGD, 0, 0, 1)
}

fn range(vendor: u16, device: u16, bar: u8, start: u64, end: u64) -> Range {
    Range {
        vendor,
        device,
        bar,
        start,
        end,
    }
}

/// A refused read: the `-errno`.
#[derive(Debug)]
struct Fail(i64);

impl From<Fail> for String {
    fn from(fail: Fail) -> String {
        format!("the read failed with {}", fail.0)
    }
}

/// `config_read` into a fresh buffer: the bytes or the `-errno`.
fn config(bdf: u64, offset: u64, len: u64) -> Result<Vec<u8>, Fail> {
    let mut buf = vec![FILL; len.max(1) as usize + 8];
    let got = sys(
        OP_DBG_CONFIG_READ,
        bdf,
        offset,
        len,
        buf.as_mut_ptr() as u64,
    );
    if got < 0 {
        return Err(Fail(got));
    }
    assert_eq!(got as u64, len, "the length returned");
    assert!(
        buf[len as usize..].iter().all(|b| *b == FILL),
        "config_read wrote past its length"
    );
    buf.truncate(len as usize);
    Ok(buf)
}

/// `mmio_read` into a fresh buffer.
fn mmio(bdf: u64, bar: u64, offset: u64, len: u64) -> Result<Vec<u8>, Fail> {
    let mut buf = vec![FILL; len.max(1) as usize + 8];
    let got = sys(
        OP_DBG_MMIO_READ,
        bdf,
        offset,
        bar | len << 8,
        buf.as_mut_ptr() as u64,
    );
    if got < 0 {
        return Err(Fail(got));
    }
    assert_eq!(got as u64, len, "the length returned");
    assert!(
        buf[len as usize..].iter().all(|b| *b == FILL),
        "mmio_read wrote past its length"
    );
    buf.truncate(len as usize);
    Ok(buf)
}

fn expect_err<T: core::fmt::Debug>(
    got: Result<T, Fail>,
    errno: i64,
    what: &str,
) -> Result<(), String> {
    check!(
        matches!(&got, Err(Fail(e)) if *e == -errno),
        "{what}: {got:?}, expected -{errno}"
    );
    Ok(())
}

/// A synthetic function with chosen ids and BARs, appended like the others
/// and removed by the fixture.
fn add_with_ids(vendor: u16, device: u16, bars: Vec<Bar>) -> Result<DeviceId, String> {
    let mut resources = Resources::empty();
    for bar in bars {
        resources.set_bar(bar);
    }
    let info = DeviceInfo {
        id: DeviceId(0),
        bus: BusId::Pci(GHOST),
        vendor,
        device,
        subsystem_vendor: 0,
        subsystem_device: 0,
        class: 0x03,
        subclass: 0,
        prog_if: 0,
        revision: 0,
        resources,
    };
    crate::dev::table()
        .lock()
        .insert(info)
        .map_err(|error| format!("cannot add a synthetic device: {error:?}"))
}

/// Only `dbgd` (and root) may call; a labelled app and any driver uid are
/// refused before anything is looked up.
pub fn peek_callers() -> Result<(), String> {
    let fx = Fixture::new()?;
    add_device(Spec::nic(None))?;
    let _peek = Peek::with(Policy::with_ranges(
        false,
        &[range(0x1AF4, 0x1000, 0, 0, 0x100)],
    ));

    for (uid, label, want) in [
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
    // Root keeps ambient authority (the kernel task).
    config(GHOST_BDF, 0, 4).map_err(|e| format!("root refused: {e:?}"))?;
    Ok(())
}

/// Config space: bounds, the function must exist, and a real function's
/// first dword is its vendor and device id.
pub fn peek_config_bounds_and_identity() -> Result<(), String> {
    let fx = Fixture::new()?;
    add_device(Spec::nic(None))?;
    enter(spawn_driver(dbgd())?)?;
    check!(
        config(GHOST_BDF, 0, 256)? == vec![0xFF; 256],
        "an absent function's config space reads all-ones"
    );
    check!(config(GHOST_BDF, 255, 1)?.len() == 1, "the last byte");
    check!(config(GHOST_BDF, 3, 5)?.len() == 5, "an unaligned span");
    for (what, offset, len) in [
        ("zero length", 0, 0),
        ("past 256", 0, 257),
        ("starting past the end", 256, 1),
        ("a wrapping span", u64::MAX, 2),
    ] {
        expect_err(config(GHOST_BDF, offset, len), EINVAL, what)?;
    }
    expect_err(
        config(0xFD00, 0, 4),
        ENODEV,
        "an address nothing enumerated",
    )?;
    expect_err(config(0x1_0000, 0, 4), EINVAL, "a BDF over 16 bits")?;
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
        let BusId::Pci(a) = info.bus else {
            return Err(String::from("unreachable"));
        };
        let bdf = u64::from(a.bus) << 8 | u64::from(a.device) << 3 | u64::from(a.function);
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
/// listed range, alignment and length bounds, the GPU's GMBUS data register.
pub fn peek_mmio_allowlist() -> Result<(), String> {
    let fx = Fixture::new()?;
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
    expect_err(mmio(GHOST_BDF, 0, 0x11, 4), EINVAL, "an unaligned offset")?;
    expect_err(mmio(GHOST_BDF, 0, 0x10, 6), EINVAL, "an unaligned length")?;
    expect_err(mmio(GHOST_BDF, 0, 0x10, 0), EINVAL, "no length")?;
    expect_err(mmio(GHOST_BDF, 0, 0x10, 4100), EINVAL, "over 4 KiB")?;
    expect_err(mmio(GHOST_BDF, 6, 0x10, 4), EINVAL, "BAR 6")?;
    expect_err(mmio(0xFD00, 0, 0x10, 4), ENODEV, "no such function")?;
    Ok(())
}

/// With the built-in table the Kaby Lake GMBUS block and ELD port are holes
/// while a pipe register is reachable. The synthetic function's BAR is the
/// HPET window, so the permitted read is harmless on every machine.
pub fn peek_builtin_holes() -> Result<(), String> {
    let fx = Fixture::new()?;
    // The first device with these ids in the table is the one `find` picks;
    // the box's real GPU is not present in QEMU.
    add_with_ids(0x8086, 0x5916, vec![mem_bar(0, WINDOW, 16 << 20)])?;
    enter(spawn_driver(dbgd())?)?;
    let _peek = Peek::with(Policy::with_ranges(false, &[]));
    for gmbus in [0xC5100u64, 0xC5108, 0xC510C, 0xC5120] {
        expect_err(mmio(GHOST_BDF, 0, gmbus, 4), EACCES, "a GMBUS register")?;
    }
    expect_err(mmio(GHOST_BDF, 0, 0x65050, 4), EACCES, "the ELD data port")?;
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

/// A device a driver holds is refused unless the box allowed it; a BAR that
/// is not usable memory, lies past its end or overlaps RAM is `ENXIO` or
/// `ERANGE` and reads nothing.
pub fn peek_claims_and_bad_bars() -> Result<(), String> {
    let fx = Fixture::new()?;
    let ram = mem::alloc_frame().ok_or("no frame")?;
    let dev = add_with_ids(
        0x1AF4,
        0x1000,
        vec![
            mem_bar(0, WINDOW, 0x1000),
            io_bar(1, 0x700, 8),
            mem_bar(2, ram.as_u64(), 0x1000),
        ],
    )?;
    let ranges: Vec<Range> = (0..4)
        .map(|bar| range(0x1AF4, 0x1000, bar, 0, 0x4000))
        .collect();
    let driver = spawn_driver(driver_cred())?;
    let reader = spawn_driver(dbgd())?;

    enter(reader)?;
    {
        let _peek = Peek::with(Policy::with_ranges(false, &ranges));
        check!(
            mmio(GHOST_BDF, 0, 0, 16)?.len() == 16,
            "an unclaimed device"
        );
        expect_err(mmio(GHOST_BDF, 1, 0, 4), ENXIO, "an I/O BAR")?;
        expect_err(mmio(GHOST_BDF, 2, 0, 4), ENXIO, "a BAR over RAM")?;
        expect_err(mmio(GHOST_BDF, 3, 0, 4), ENXIO, "an absent BAR")?;
        expect_err(mmio(GHOST_BDF, 0, 0x1000, 4), ERANGE, "past the BAR's end")?;
        expect_err(mmio(GHOST_BDF, 0, 0xFFC, 8), ERANGE, "a span over the end")?;

        enter(driver)?;
        let handle = expect_ok(claim_plain(dev), "a driver claims the device")?;
        enter(reader)?;
        expect_err(mmio(GHOST_BDF, 0, 0, 4), EBUSY, "a claimed device")?;
        // Config space is read regardless of a claim.
        check!(
            config(GHOST_BDF, 0, 4)?.len() == 4,
            "config of a claimed device"
        );

        enter(driver)?;
        expect_ok(
            sys(crate::dev::syscall::OP_RELEASE, handle, 0, 0, 0),
            "release",
        )?;
        enter(reader)?;
        check!(mmio(GHOST_BDF, 0, 0, 4)?.len() == 4, "after the release");
    }
    // Allowed on a claimed device only when the box says so.
    enter(driver)?;
    let handle = expect_ok(claim_plain(dev), "claim again")?;
    enter(reader)?;
    {
        let _peek = Peek::with(Policy::with_ranges(true, &ranges));
        check!(
            mmio(GHOST_BDF, 0, 0, 4)?.len() == 4,
            "a claimed device, allowed"
        );
    }
    enter(driver)?;
    expect_ok(
        sys(crate::dev::syscall::OP_RELEASE, handle, 0, 0, 0),
        "release",
    )?;
    leave(&fx);
    mem::free_frame(ram);
    Ok(())
}

/// `apply_config` reads the three keys, ignores them without `diag.dbg=1`,
/// and a later read of a different text replaces them.
pub fn peek_config_lines() -> Result<(), String> {
    let fx = Fixture::new()?;
    add_with_ids(0x1AF4, 0x1000, vec![mem_bar(0, WINDOW, 0x1000)])?;
    enter(spawn_driver(dbgd())?)?;
    let _peek = Peek::with(Policy::off());
    peek::apply_config("diag.dbg.mmio=1\ndiag.dbg.mmio.allow=1af4:1000:0:0-100\n");
    expect_err(mmio(GHOST_BDF, 0, 0, 4), EACCES, "no diag.dbg=1")?;
    peek::apply_config("diag.dbg=1\ndiag.dbg.mmio=1\ndiag.dbg.mmio.allow=1af4:1000:0:0-100\n");
    check!(
        mmio(GHOST_BDF, 0, 0, 0x100)?.len() == 0x100,
        "the allow line"
    );
    expect_err(mmio(GHOST_BDF, 0, 0x100, 4), EACCES, "past the allow line")?;
    peek::apply_config("diag.dbg=1\n");
    expect_err(
        mmio(GHOST_BDF, 0, 0, 4),
        EACCES,
        "after the box turned it off",
    )?;
    leave(&fx);
    Ok(())
}

/// Real registers: QEMU's standard VGA, when the machine has it. The VBE
/// id register (BAR2 + 0x500) reads 0xB0C0..=0xB0C5, the input-status port
/// at 0x41a is a hole, and the framebuffer BAR reads without a fault.
pub fn peek_real_vga() -> Result<(), String> {
    let fx = Fixture::new()?;
    let vga = crate::dev::table()
        .lock()
        .iter()
        .find(|d| d.vendor == 0x1234 && d.device == 0x1111);
    let Some(info) = vga else {
        crate::serial_println!("PEEK:VGA:SKIP no 1234:1111 on this machine");
        return Ok(());
    };
    let BusId::Pci(a) = info.bus else {
        return Err(String::from("the VGA is not a PCI function"));
    };
    let bdf = u64::from(a.bus) << 8 | u64::from(a.device) << 3 | u64::from(a.function);
    enter(spawn_driver(dbgd())?)?;
    let _peek = Peek::with(Policy::with_ranges(false, &[]));
    let id = mmio(bdf, 2, 0x500, 4)?;
    let id = u32::from_le_bytes([id[0], id[1], id[2], id[3]]) & 0xFFFF;
    check!((0xB0C0..=0xB0C5).contains(&id), "the VBE id reads {id:#x}");
    expect_err(mmio(bdf, 2, 0x418, 4), EACCES, "the VGA input-status port")?;
    check!(mmio(bdf, 0, 0, 64)?.len() == 64, "the framebuffer BAR");
    // The same bytes twice: a plain register read has no side effect.
    check!(
        mmio(bdf, 2, 0x500, 4)? == mmio(bdf, 2, 0x500, 4)?,
        "a stable id"
    );
    leave(&fx);
    Ok(())
}

/// Thousands of reads, refusals, claims and releases: the mapped window
/// stops growing after the first round, and neither slab nor frames move.
pub fn peek_soak() -> Result<(), String> {
    const ROUNDS: u64 = 2000;
    let fx = Fixture::new()?;
    let dev = add_with_ids(0x1AF4, 0x1000, vec![mem_bar(0, WINDOW, 0x1000)])?;
    let _peek = Peek::with(Policy::with_ranges(
        false,
        &[
            range(0x1AF4, 0x1000, 0, 0, 0x80),
            range(0x1AF4, 0x1000, 0, 0x80, 0x100),
        ],
    ));
    let driver = spawn_driver(driver_cred())?;
    let reader = spawn_driver(dbgd())?;
    enter(reader)?;
    check!(mmio(GHOST_BDF, 0, 0, 0x100)?.len() == 0x100, "warm up");
    let pages = peek::mapped_pages_for_test();
    let slab = mem::slab::stats().live_bytes;
    let frames = mem::frame_stats().live();
    for round in 0..ROUNDS {
        match round % 5 {
            0 => {
                // Spans the two adjacent ranges.
                check!(
                    mmio(GHOST_BDF, 0, 0x7C, 8)?.len() == 8,
                    "round {round}: joined ranges"
                );
            }
            1 => expect_err(mmio(GHOST_BDF, 0, 0x100, 4), EACCES, "past the ranges")?,
            2 => {
                check!(
                    config(GHOST_BDF, (round % 252) & !3, 4)?.len() == 4,
                    "config"
                );
            }
            3 => expect_err(mmio(GHOST_BDF, 0, 1, 4), EINVAL, "unaligned")?,
            _ => {
                enter(driver)?;
                let handle = expect_ok(claim_plain(dev), "claim")?;
                enter(reader)?;
                expect_err(mmio(GHOST_BDF, 0, 0, 4), EBUSY, "claimed")?;
                enter(driver)?;
                expect_ok(
                    sys(crate::dev::syscall::OP_RELEASE, handle, 0, 0, 0),
                    "release",
                )?;
                enter(reader)?;
            }
        }
    }
    check!(
        peek::mapped_pages_for_test() == pages,
        "the mapped window grew from {pages} to {} pages",
        peek::mapped_pages_for_test()
    );
    check!(
        mem::slab::stats().live_bytes == slab,
        "slab grew across reads"
    );
    check!(
        mem::frame_stats().live() == frames,
        "frames grew across reads"
    );
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
    ("dev_peek_claims_and_bad_bars", peek_claims_and_bad_bars),
    ("dev_peek_config_lines", peek_config_lines),
    ("dev_peek_real_vga", peek_real_vga),
    ("dev_peek_soak", peek_soak),
];
