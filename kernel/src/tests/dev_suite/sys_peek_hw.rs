//! The device half of the `dev::peek` tests (`sys_peek` has the rules):
//! claimed and quiesced functions, BARs that cannot be read, a gone
//! function, QEMU's standard VGA as the one real target, the BDF in the
//! inventory, and a soak that mixes reads with claims and refusals.

use alloc::vec;
use alloc::vec::Vec;

use super::fixture::*;
use super::sys_peek_util::*;
use super::*;
use crate::dev::errno::*;
use crate::dev::inspect::INVENTORY_WORDS;
use crate::dev::peek;
use crate::dev::syscall::{OP_INVENTORY, OP_RELEASE};
use crate::ipc::credentials::Cred;
use pciprobe::{Policy, Range};

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
        expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
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
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    leave(&fx);
    mem::free_frame(ram);
    Ok(())
}

/// A function that reads all-ones (gone, link down) is `ENODEV` on every
/// MMIO read, not "decode on"; the synthetic function at the ghost address
/// is exactly that when nothing is assumed present.
pub fn peek_gone_function() -> Result<(), String> {
    let _fx = Fixture::new()?;
    add_with_ids(0x1AF4, 0x1000, vec![mem_bar(0, WINDOW, 0x1000)])?;
    enter(spawn_driver(dbgd())?)?;
    let _peek = Peek::real(Policy::with_ranges(
        false,
        &[range(0x1AF4, 0x1000, 0, 0, 0x100)],
    ));
    expect_err(
        mmio(GHOST_BDF, 0, 0, 4),
        ENODEV,
        "a function that reads all-ones",
    )?;
    // Config space still answers (all-ones is the truthful answer).
    check!(
        config(GHOST_BDF, 0, 4)? == vec![0xFF; 4],
        "config of a gone function"
    );
    Ok(())
}

/// The real function QEMU's standard VGA is, if the machine has it.
fn real_vga() -> Option<DeviceInfo> {
    crate::dev::table()
        .lock()
        .iter()
        .find(|d| d.vendor == 0x1234 && d.device == 0x1111)
}

/// Restores a real function's command register when the test ends, after
/// the fixture has released the claim (which quiesces it): declared before
/// the fixture, so it drops after it.
struct RestoreCommand {
    address: pci::Address,
    saved: u16,
}

impl Drop for RestoreCommand {
    fn drop(&mut self) {
        pci::write_command(self.address, self.saved);
    }
}

/// Real registers: QEMU's standard VGA. The VBE id register (BAR2 + 0x500)
/// reads 0xB0C0..=0xB0C5, the input-status port at 0x418 is a hole, the
/// framebuffer BAR is not in the built-in table (a memory-type alias of the
/// display path's mapping) and reads only when the box lists it.
pub fn peek_real_vga() -> Result<(), String> {
    let Some(info) = real_vga() else {
        crate::serial_println!("PEEK:VGA:SKIP no 1234:1111 on this machine");
        return Ok(());
    };
    let bdf = bdf_of(&info).ok_or("the VGA is not a PCI function")?;
    let fx = Fixture::new()?;
    enter(spawn_driver(dbgd())?)?;
    let _peek = Peek::real(Policy::with_ranges(false, &[]));
    let id = mmio(bdf, 2, 0x500, 4)?;
    let id = u32::from_le_bytes([id[0], id[1], id[2], id[3]]) & 0xFFFF;
    check!((0xB0C0..=0xB0C5).contains(&id), "the VBE id reads {id:#x}");
    expect_err(mmio(bdf, 2, 0x418, 4), EACCES, "the VGA input-status port")?;
    expect_err(mmio(bdf, 0, 0, 64), EACCES, "the framebuffer BAR, unlisted")?;
    // The same bytes twice: a plain register read has no side effect.
    check!(
        mmio(bdf, 2, 0x500, 4)? == mmio(bdf, 2, 0x500, 4)?,
        "a stable id"
    );
    drop(_peek);
    let _listed = Peek::real(Policy::with_ranges(
        false,
        &[range(0x1234, 0x1111, 0, 0, 0x100)],
    ));
    check!(mmio(bdf, 0, 0, 64)?.len() == 64, "the framebuffer, listed");
    leave(&fx);
    Ok(())
}

/// A claim quiesces the function (memory decode off): with the claimed
/// opt-in the read gets past the claim check and must stop at the decode
/// check with `ENXIO`, never reading a master-aborted bus.
pub fn peek_decode_off_is_enxio() -> Result<(), String> {
    let Some(info) = real_vga() else {
        crate::serial_println!("PEEK:DECODE:SKIP no 1234:1111 on this machine");
        return Ok(());
    };
    let BusId::Pci(address) = info.bus else {
        return Err(String::from("the VGA is not a PCI function"));
    };
    let bdf = bdf_of(&info).ok_or("no BDF")?;
    let _restore = RestoreCommand {
        address,
        saved: pci::command(address),
    };
    let fx = Fixture::new()?;
    let driver = spawn_driver(driver_cred())?;
    let reader = spawn_driver(dbgd())?;
    enter(driver)?;
    let handle = expect_ok(claim_plain(info.id), "a driver claims the VGA")?;
    check!(
        pci::command(address) & pci::COMMAND_MEMORY == 0,
        "the claim left memory decode on"
    );
    enter(reader)?;
    let _peek = Peek::real(Policy::with_ranges(true, &[]));
    expect_err(
        mmio(bdf, 2, 0x500, 4),
        ENXIO,
        "a claimed function with decode off",
    )?;
    // Config space is unaffected.
    check!(config(bdf, 0, 4)?.len() == 4, "config with decode off");
    enter(driver)?;
    expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
    leave(&fx);
    Ok(())
}

/// Inventory word 0 carries the PCI address in bits 40..=55 and a PCI flag
/// in bit 56; a platform device has neither.
pub fn peek_inventory_carries_the_address() -> Result<(), String> {
    let fx = Fixture::new()?;
    let pci_dev = add_device(Spec::nic(None))?;
    let platform = add_device(Spec::nic(None).platform())?;
    let total = crate::dev::table().lock().len();
    enter(spawn_driver(Cred::new(1000, 1000, 0, 0, 1))?)?;
    let mut rows = vec![0u64; total * INVENTORY_WORDS];
    let count = expect_ok(
        sys(OP_INVENTORY, rows.as_mut_ptr() as u64, total as u64, 0, 0),
        "inventory",
    )?;
    check!(count == total as u64, "inventory has {count} of {total}");
    let word = |id: DeviceId| rows[usize::from(id.0) * INVENTORY_WORDS];
    check!(
        word(pci_dev) >> 56 & 1 == 1 && (word(pci_dev) >> 40) & 0xFFFF == GHOST_BDF,
        "synthetic PCI row {:#x}",
        word(pci_dev)
    );
    check!(
        word(platform) >> 40 == 0,
        "a platform device has an address: {:#x}",
        word(platform)
    );
    // Every real PCI function's row agrees with the table.
    let table: Vec<DeviceInfo> = crate::dev::table().lock().iter().collect();
    for info in table {
        let row = rows[usize::from(info.id.0) * INVENTORY_WORDS];
        match bdf_of(&info) {
            Some(bdf) => check!(
                row >> 56 & 1 == 1 && (row >> 40) & 0xFFFF == bdf,
                "device {} row {row:#x}, bdf {bdf:#x}",
                info.id.0
            ),
            None => check!(row >> 56 & 1 == 0, "platform device {}", info.id.0),
        }
        check!(
            row & 0xFFFF == u64::from(info.id.0),
            "id bits of {}",
            info.id.0
        );
    }
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
            3 => expect_err(mmio(GHOST_BDF, 0, 1, 4), EDOM, "unaligned")?,
            _ => {
                enter(driver)?;
                let handle = expect_ok(claim_plain(dev), "claim")?;
                enter(reader)?;
                expect_err(mmio(GHOST_BDF, 0, 0, 4), EBUSY, "claimed")?;
                enter(driver)?;
                expect_ok(sys(OP_RELEASE, handle, 0, 0, 0), "release")?;
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
    ("dev_peek_claims_and_bad_bars", peek_claims_and_bad_bars),
    ("dev_peek_gone_function", peek_gone_function),
    ("dev_peek_real_vga", peek_real_vga),
    ("dev_peek_decode_off_is_enxio", peek_decode_off_is_enxio),
    (
        "dev_peek_inventory_carries_the_address",
        peek_inventory_carries_the_address,
    ),
    ("dev_peek_soak", peek_soak),
];
