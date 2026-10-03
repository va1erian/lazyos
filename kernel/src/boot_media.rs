//! Which firmware booted us: UEFI or legacy BIOS (docs/real-pc-boot-plan.md H0).
//!
//! `bootloader` 0.11 has no explicit flag for it, but each of its two stages
//! reports the firmware's own memory types in a different variant: the UEFI
//! stage turns every non-conventional UEFI memory type into
//! [`MemoryRegionKind::UnknownUefi`] (runtime services, ACPI, MMIO, reserved),
//! and the BIOS stage turns every non-usable E820 type into
//! [`MemoryRegionKind::UnknownBios`] (the reserved BIOS area and ROM below
//! 1 MiB at the least). Real firmware always reports some of these, so the
//! variant that dominates names the firmware. The map is firmware data: the
//! count is bounded by the slice and a map with neither is reported as
//! [`Media::Unknown`], never guessed.
//!
//! [`record`] prints the `BOOT:MEDIA:<uefi|bios|unknown>` line once and keeps
//! the verdict for later readers ([`get`]).

use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use core::sync::atomic::{AtomicU8, Ordering};

/// The firmware the bootloader ran under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Media {
    Uefi,
    Bios,
    Unknown,
}

impl Media {
    /// The marker word (`BOOT:MEDIA:<word>`).
    pub fn as_str(self) -> &'static str {
        match self {
            Media::Uefi => "uefi",
            Media::Bios => "bios",
            Media::Unknown => "unknown",
        }
    }

    fn from_u8(raw: u8) -> Media {
        match raw {
            1 => Media::Uefi,
            2 => Media::Bios,
            _ => Media::Unknown,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Media::Uefi => 1,
            Media::Bios => 2,
            Media::Unknown => 0,
        }
    }
}

static MEDIA: AtomicU8 = AtomicU8::new(0);

/// Classify a memory map. Pure: the test suite feeds it synthetic maps.
pub fn detect(regions: &[MemoryRegion]) -> Media {
    let (mut uefi, mut bios) = (0usize, 0usize);
    for region in regions {
        match region.kind {
            MemoryRegionKind::UnknownUefi(_) => uefi += 1,
            MemoryRegionKind::UnknownBios(_) => bios += 1,
            _ => {}
        }
    }
    match uefi.cmp(&bios) {
        core::cmp::Ordering::Greater => Media::Uefi,
        core::cmp::Ordering::Less => Media::Bios,
        core::cmp::Ordering::Equal => Media::Unknown,
    }
}

/// Classify the boot memory map, print `BOOT:MEDIA:<word>` and remember it.
pub fn record(regions: &[MemoryRegion]) -> Media {
    let media = detect(regions);
    MEDIA.store(media.to_u8(), Ordering::Relaxed);
    serial_println!("BOOT:MEDIA:{}", media.as_str());
    media
}

/// The verdict [`record`] stored ([`Media::Unknown`] before it ran).
#[allow(dead_code)] // for hwreport and the on-screen boot log (H1)
pub fn get() -> Media {
    Media::from_u8(MEDIA.load(Ordering::Relaxed))
}
