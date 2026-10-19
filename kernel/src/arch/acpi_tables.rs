//! The firmware's ACPI tables, read once through `libs/acpi` over the
//! physical-memory map (docs/real-pc-boot-plan.md H2).
//!
//! `main` hands over the RSDP address from the boot info before anything
//! else consumes it; the first [`platform`] call walks the tables. The timer
//! uses the FADT's PM timer, the MADT's local APIC address and the HPET; the
//! parsed FADT (reset register, PM1 control blocks) and the validated DSDT
//! stay here for the power-off code.

use core::sync::atomic::{AtomicU64, Ordering};

use alloc::string::String;

use ::acpi::{Error, PhysMem, Platform};
use spin::Once;
use x86_64::PhysAddr;

use crate::mem;

/// RSDP physical address from the bootloader (0: none).
static RSDP: AtomicU64 = AtomicU64::new(0);
static PLATFORM: Once<Option<Platform>> = Once::new();

/// Physical ranges a table pointer may never make us read: legacy VGA memory
/// and the chipset MMIO window (I/O APIC, HPET, local APIC, flash), where a
/// read can have side effects. No firmware puts a table there.
const FORBIDDEN: [(u64, u64); 2] = [(0xA_0000, 0xC_0000), (0xFEC0_0000, 0x1_0000_0000)];

/// Record the RSDP address the bootloader found (BIOS: the EBDA/ROM scan;
/// UEFI: the ACPI 2.0 configuration table, i.e. the XSDP).
pub fn set_rsdp(addr: Option<u64>) {
    RSDP.store(addr.unwrap_or(0), Ordering::Relaxed);
}

/// Physical memory through the kernel's physical-memory mapping, refusing
/// any byte that is not mapped or lies in a forbidden range.
struct PhysMap;

impl PhysMem for PhysMap {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        let len = buf.len() as u64;
        // Physical addresses are at most 52 bits (`PhysAddr::new` panics on more).
        let Some(end) = addr.checked_add(len).filter(|&end| end <= 1 << 52) else {
            return false;
        };
        if FORBIDDEN.iter().any(|&(lo, hi)| addr < hi && end > lo) {
            return false;
        }
        if !mem::mmio::phys_mapped(addr, len) {
            return false;
        }
        let src = mem::phys_to_virt(PhysAddr::new(addr)).as_ptr::<u8>();
        // SAFETY: every byte of `[addr, end)` is mapped (checked above) and is
        // ordinary memory outside the device windows; firmware tables are not
        // written concurrently, and `buf` is a distinct kernel buffer.
        unsafe { core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), buf.len()) };
        true
    }
}

/// The parsed tables, or `None` when there is no usable RSDP. Walks the
/// tables on the first call; needs only the physical-memory map.
pub fn platform() -> Option<&'static Platform> {
    PLATFORM
        .call_once(|| {
            let rsdp = RSDP.load(Ordering::Relaxed);
            match Platform::discover(&PhysMap, rsdp) {
                Ok(platform) => {
                    log(&platform);
                    Some(platform)
                }
                Err(error) => {
                    crate::serial_println!("HW:ACPI:ABSENT rsdp={rsdp:#x} ({error:?})");
                    None
                }
            }
        })
        .as_ref()
}

/// The `\_S5` sleep type from the DSDT: a bounded byte scan, run when a
/// power-off is planned (it can read the whole table, so not at boot).
pub fn find_s5() -> Result<::acpi::SleepType, Error> {
    let platform = platform().ok_or(Error::NotFound)?;
    let dsdt = platform.dsdt.as_ref().map_err(|error| *error)?;
    ::acpi::s5::find_s5(&PhysMap, dsdt)
}

fn verdict<T>(result: &Result<T, Error>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(Error::NotFound) => "none",
        Err(_) => "bad",
    }
}

/// One summary line, plus one line per refused table so a real machine's
/// log says which table cost which feature.
fn log(p: &Platform) {
    let root = core::str::from_utf8(&p.root.signature).unwrap_or("?");
    crate::serial_println!(
        "HW:ACPI:PRESENT rev={} root={root} fadt={} madt={} hpet={} dsdt={} bad_entries={}",
        p.rsdp.revision,
        verdict(&p.fadt),
        verdict(&p.madt),
        verdict(&p.hpet),
        verdict(&p.dsdt),
        p.bad_entries
    );
    for (name, error) in [
        ("FADT", p.fadt.as_ref().err()),
        ("MADT", p.madt.as_ref().err()),
        ("HPET", p.hpet.as_ref().err()),
        ("DSDT", p.dsdt.as_ref().err()),
    ] {
        if let Some(error) = error.filter(|e| **e != Error::NotFound) {
            crate::serial_println!("acpi: {name} refused: {error:?}");
        }
    }
    if let Ok(madt) = &p.madt {
        crate::serial_println!(
            "acpi: MADT lapic={:#x} cpus={} apic_ids={} ioapics={} overrides={} pcat={}",
            madt.lapic_address,
            madt.processors,
            apic_id_list(madt),
            madt.ioapics.len(),
            madt.overrides.len(),
            madt.pcat_compat()
        );
    }
}

/// The MADT's processor APIC IDs as a bracketed boot-log list (`[0,1,2]`,
/// `+N` past the recorded capacity): a diagnostic, so a real board's
/// topology is in the log the SMP bring-up (docs/smp-plan.md S8.2) is judged
/// from.
fn apic_id_list(madt: &::acpi::Madt) -> String {
    use core::fmt::Write as _;
    let mut out = String::from("[");
    for (index, id) in madt.apic_ids.as_slice().iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(out, "{id}");
    }
    out.push(']');
    let dropped = madt.apic_ids.dropped();
    if dropped > 0 {
        let _ = write!(out, "+{dropped}");
    }
    out
}
