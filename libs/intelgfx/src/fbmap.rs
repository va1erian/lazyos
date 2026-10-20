//! Where a framebuffer's pages are, from its GGTT entries (read-only, like the rest of `adopt`).

use crate::adopt::{GGTT_ENTRIES, GGTT_OFFSET, MAX_FB_PAGES};
use crate::mmio::Mmio;

/// Where a framebuffer's pages are, from its GGTT entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FbMap {
    /// Pages the surface spans, and how many entries were read.
    pub pages: u32,
    pub present: u32,
    /// Physical address of the first and last page (0 when not present).
    pub first: u64,
    pub last: u64,
    /// Every entry present and each page the next after the last.
    pub contiguous: bool,
}

/// Read the GGTT entries of the surface at GGTT offset `surf`, `bytes` long
/// (at most [`MAX_FB_PAGES`] pages are walked).
///
/// A Gen8+ entry is 64 bits: present in bit 0, the page's physical address
/// in bits 38:12 (cache control bits sit below the address and are not
/// decoded). Entries are read as two dwords; a surface outside the table
/// reads nothing.
pub fn read_fb_map(mmio: &mut impl Mmio, surf: u32, bytes: u64) -> FbMap {
    let first_entry = surf >> 12;
    let pages = bytes.div_ceil(4096).min(u64::from(MAX_FB_PAGES)) as u32;
    let mut map = FbMap {
        pages,
        contiguous: pages > 0,
        ..FbMap::default()
    };
    if pages == 0 || first_entry >= GGTT_ENTRIES || pages > GGTT_ENTRIES - first_entry {
        map.contiguous = false;
        return map;
    }
    let mut previous = 0u64;
    for index in 0..pages {
        let at = GGTT_OFFSET + (first_entry + index) * 8;
        let low = mmio.read32(at);
        let high = mmio.read32(at + 4);
        let entry = u64::from(high) << 32 | u64::from(low);
        if entry & 1 == 0 {
            map.contiguous = false;
            continue;
        }
        let phys = entry & 0x7F_FFFF_F000;
        if map.present == 0 {
            map.first = phys;
        } else if phys != previous + 4096 {
            map.contiguous = false;
        }
        map.present += 1;
        map.last = phys;
        previous = phys;
    }
    map.contiguous &= map.present == pages;
    map
}
