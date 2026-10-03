//! `boot_media::detect`: the firmware type read from the memory map's
//! firmware-specific region kinds (docs/real-pc-boot-plan.md H0).

use super::*;
use crate::boot_media::{detect, Media};
use bootloader_api::info::{MemoryRegion, MemoryRegionKind};

fn region(start: u64, end: u64, kind: MemoryRegionKind) -> MemoryRegion {
    MemoryRegion { start, end, kind }
}

/// The shapes the two bootloader stages produce, plus maps that name neither.
pub fn detects_each_firmware() -> Result<(), String> {
    let usable = MemoryRegionKind::Usable;
    let loader = MemoryRegionKind::Bootloader;
    // SeaBIOS: E820 reserved (2) below 1 MiB and at the top of 4 GiB.
    let bios = [
        region(0, 0x9_FC00, usable),
        region(0x9_FC00, 0xA_0000, MemoryRegionKind::UnknownBios(2)),
        region(0xF_0000, 0x10_0000, MemoryRegionKind::UnknownBios(2)),
        region(0x10_0000, 0x200_0000, loader),
        region(0x200_0000, 0x800_0000, usable),
        region(0xFFFC_0000, 0x1_0000_0000, MemoryRegionKind::UnknownBios(2)),
    ];
    check!(
        detect(&bios) == Media::Bios,
        "SeaBIOS map: {:?}",
        detect(&bios)
    );
    // OVMF: runtime services (5, 6), ACPI (9, 10), reserved (0), MMIO (11).
    let uefi = [
        region(0, 0xA_0000, usable),
        region(0x10_0000, 0x80_0000, usable),
        region(0x80_0000, 0x80_8000, MemoryRegionKind::UnknownUefi(10)),
        region(0x80_8000, 0x7E00_0000, usable),
        region(0x7E00_0000, 0x7E10_0000, MemoryRegionKind::UnknownUefi(6)),
        region(0x7E10_0000, 0x7E20_0000, MemoryRegionKind::UnknownUefi(5)),
        region(0x7F00_0000, 0x7F10_0000, MemoryRegionKind::UnknownUefi(9)),
        region(
            0xFFC0_0000,
            0x1_0000_0000,
            MemoryRegionKind::UnknownUefi(11),
        ),
    ];
    check!(
        detect(&uefi) == Media::Uefi,
        "OVMF map: {:?}",
        detect(&uefi)
    );
    check!(
        detect(&[]) == Media::Unknown,
        "an empty map named a firmware"
    );
    let plain = [
        region(0, 0x100_0000, usable),
        region(0x100_0000, 0x200_0000, loader),
    ];
    check!(
        detect(&plain) == Media::Unknown,
        "a map with no firmware kinds named a firmware"
    );
    // A hostile tie is not guessed at; a clear majority wins.
    let tie = [
        region(0, 1, MemoryRegionKind::UnknownUefi(0)),
        region(1, 2, MemoryRegionKind::UnknownBios(2)),
    ];
    check!(detect(&tie) == Media::Unknown, "a tie named a firmware");
    check!(
        Media::Uefi.as_str() == "uefi" && Media::Bios.as_str() == "bios",
        "marker words changed"
    );
    Ok(())
}

/// Soak: 20 000 random maps classify by the majority rule, and the count is
/// bounded by the map (a 4096-entry map is fine).
pub fn soak_random_maps() -> Result<(), String> {
    let mut state = 0xD1B5_4A32_D192_ED03u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut map = Vec::with_capacity(4096);
    for round in 0..20_000u32 {
        map.clear();
        let len = if round % 1000 == 0 {
            4096
        } else {
            (next() % 64) as usize
        };
        let (mut uefi, mut bios) = (0usize, 0usize);
        for index in 0..len as u64 {
            let kind = match next() % 4 {
                0 => {
                    uefi += 1;
                    MemoryRegionKind::UnknownUefi(next() as u32)
                }
                1 => {
                    bios += 1;
                    MemoryRegionKind::UnknownBios(next() as u32)
                }
                2 => MemoryRegionKind::Usable,
                _ => MemoryRegionKind::Bootloader,
            };
            map.push(region(index << 12, (index + 1) << 12, kind));
        }
        let want = match uefi.cmp(&bios) {
            core::cmp::Ordering::Greater => Media::Uefi,
            core::cmp::Ordering::Less => Media::Bios,
            core::cmp::Ordering::Equal => Media::Unknown,
        };
        check!(
            detect(&map) == want,
            "round {round}: {uefi} uefi / {bios} bios gave {:?}",
            detect(&map)
        );
    }
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("boot_media_detects_each_firmware", detects_each_firmware),
    ("boot_media_soak_random_maps", soak_random_maps),
];
