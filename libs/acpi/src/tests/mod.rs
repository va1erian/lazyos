//! Host tests: QEMU's real tables under SeaBIOS and OVMF, a synthetic modern
//! PC, and hostile variants of both.

use crate::fuzz::{seal, Image};
use crate::gas::{AddressSpace, Gas};
use crate::{fadt, Error, Platform};

pub(crate) mod golden;
mod hostile;

use golden::*;

fn discover(image: &Image) -> Platform {
    Platform::discover(image, image.rsdp).expect("discover")
}

#[test]
fn every_golden_parses_completely() {
    for (name, image) in all() {
        let platform = discover(&image);
        assert_eq!(platform.bad_entries, 0, "{name}");
        assert_eq!(
            platform.rsdp.revision, 0,
            "{name}: QEMU builds an ACPI 1.0 RSDP"
        );
        assert_eq!(&platform.root.signature, b"RSDT", "{name}");
        let fadt = platform.fadt.as_ref().expect(name);
        let madt = platform.madt.as_ref().expect(name);
        assert!(platform.dsdt.is_ok(), "{name}: DSDT {:?}", platform.dsdt);
        assert_eq!(fadt.dsdt, Some(platform.dsdt.unwrap().phys), "{name}");
        // SeaBIOS puts the PIIX4 and ICH9 PM blocks at 0x600; OVMF moves
        // PIIX4's to 0xB000. The timer is at +8, PM1a control at +4.
        let pm_base = if name == "pc-ovmf" { 0xB000 } else { 0x600 };
        let timer = fadt.pm_timer.expect(name);
        assert_eq!(timer.block.space, AddressSpace::Io, "{name}");
        assert_eq!(timer.block.port(4), Some(pm_base + 8), "{name}");
        let control = fadt.pm1a_control.and_then(|g| g.port(2));
        assert_eq!(control, Some(pm_base + 4), "{name}");
        assert_eq!(fadt.century, 0x32, "{name}");
        assert!(!fadt.hw_reduced(), "{name}");
        assert_eq!(madt.lapic_address, 0xFEE0_0000, "{name}");
        assert!(madt.pcat_compat(), "{name}");
        assert_eq!(madt.processors, 1, "{name}");
        assert_eq!(madt.first_apic_id, Some(0), "{name}");
        let ioapic = madt.ioapics.as_slice()[0];
        assert_eq!(
            (ioapic.address, ioapic.gsi_base),
            (0xFEC0_0000, 0),
            "{name}"
        );
        // QEMU routes the PIT (ISA IRQ0) to GSI 2.
        assert_eq!(madt.isa_gsi(0).0, 2, "{name}");
        match platform.hpet {
            Ok(hpet) => {
                assert!(!name.contains("nohpet"));
                assert_eq!(hpet.address, 0xFED0_0000, "{name}");
                assert!(hpet.comparators() >= 3, "{name}");
            }
            Err(error) => {
                assert!(name.contains("nohpet"), "{name}: HPET {error:?}");
                assert_eq!(error, Error::NotFound);
            }
        }
    }
}

#[test]
fn q35_fadt_has_the_reset_register() {
    for name in ["q35-seabios", "q35-ovmf"] {
        let platform = discover(&dump(name));
        let fadt = platform.fadt.unwrap();
        assert_eq!(fadt.table.revision, 3, "{name}");
        let reset = fadt.reset.expect(name);
        assert_eq!(reset.register.port(1), Some(0xCF9), "{name}");
        assert_eq!(reset.value, 0x0F, "{name}");
        // ICH9 has the fixed-feature 8042 bit set.
        assert!(fadt.iapc_boot_arch & fadt::boot_arch::I8042 != 0, "{name}");
    }
    // The ACPI 1.0 FADT of `pc` has no reset register field at all.
    let fadt = discover(&dump("pc-seabios")).fadt.unwrap();
    assert_eq!((fadt.table.length, fadt.reset), (116, None));
}

#[test]
fn synthetic_modern_prefers_the_xsdt() {
    let platform = discover(&synthetic_modern());
    assert_eq!(platform.rsdp.revision, 2);
    assert_eq!(platform.rsdp.xsdt, Some(XSDT));
    assert_eq!(&platform.root.signature, b"XSDT");
    assert!(platform.madt.is_ok() && platform.hpet.is_ok());

    let fadt = platform.fadt.unwrap();
    let timer = fadt.pm_timer.unwrap();
    assert_eq!((timer.block.port(4), timer.width32), (Some(0x1808), true));
    assert_eq!(timer.elapsed(0xFFFF_FFF0, 0x10), 0x20);
    assert_eq!(
        fadt.pm1a_control,
        Some(Gas::read(&synthetic_modern(), FADT + 172).unwrap().unwrap())
    );
    assert_eq!(fadt.reset.unwrap().value, 6);
    assert_eq!(fadt.dsdt, Some(DSDT));
    assert_eq!(fadt.minor_version, 5);
    assert_eq!(platform.dsdt.unwrap().length, 100);

    let madt = platform.madt.unwrap();
    // Two enabled/online-capable x2APIC entries plus one, and one enabled
    // legacy entry; the disabled one is not counted.
    assert_eq!(madt.processors, 4);
    assert_eq!(madt.first_apic_id, Some(0));
    assert_eq!(madt.isa_gsi(9), (9, 0x0D));
    assert_eq!(madt.isa_gsi(4), (4, 0));
    assert_eq!(madt.lapic_nmis.len(), 2);
    assert_eq!(madt.lapic_nmis.as_slice()[1].processor, u32::MAX);

    let hpet = platform.hpet.unwrap();
    assert_eq!((hpet.comparators(), hpet.counter64()), (8, true));
    assert_eq!(hpet.min_tick, 0x80);
}

#[test]
fn a_24_bit_pm_timer_wraps_at_24_bits() {
    let timer = fadt::PmTimer {
        block: Gas::io(0x608, 4).unwrap(),
        width32: false,
    };
    assert_eq!(timer.elapsed(0x00FF_FFF0, 0x0000_0010), 0x20);
    // Bits above 24 (a read that returned garbage there) are ignored.
    assert_eq!(timer.elapsed(0xAB00_0000, 0xCD00_0005), 5);
}

#[test]
fn hpet_period_limits() {
    use crate::hpet::{frequency, period_fs};
    // QEMU's HPET: 10 ns (100 MHz).
    assert_eq!(period_fs(10_000_000u64 << 32), Some(10_000_000));
    assert_eq!(frequency(10_000_000), 100_000_000);
    // Intel PCH: 69.841279 ns (14.318 MHz).
    assert_eq!(frequency(69_841_279), 14_318_179);
    assert_eq!(period_fs(0), None);
    assert_eq!(period_fs(100_000_001u64 << 32), None);
}

#[test]
fn seal_round_trips_through_the_parser() {
    let mut fadt = modern_fadt();
    fadt[60] ^= 0x55;
    seal(&mut fadt);
    let mut image = synthetic_modern();
    *image.table_mut(b"FACP").unwrap() = fadt;
    assert!(discover(&image).fadt.is_ok());
}
