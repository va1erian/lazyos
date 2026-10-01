//! The text multiplexer presents its frame in bands of rows, so interrupts
//! are never off for a whole-screen copy (`mux::bands`). The bands must
//! cover exactly the damaged rows, in order, without overlap, each within
//! the band size; the stress case checks that over thousands of shapes.

use super::*;
use crate::mux::bands;

/// Every row of `y..y + h` exactly once, in order, in bands of at most
/// `band` rows (a zero band acts as one).
fn check(y: usize, h: usize, band: usize) -> Result<(), String> {
    let mut next = y;
    for (start, rows) in bands(y, h, band) {
        check!(
            start == next,
            "bands({y}, {h}, {band}): gap or overlap at {start}"
        );
        check!(
            rows >= 1 && rows <= band.max(1),
            "bands({y}, {h}, {band}): band of {rows} rows"
        );
        next = start + rows;
    }
    check!(next == y + h, "bands({y}, {h}, {band}) ended at {next}");
    Ok(())
}

pub fn bands_cover_the_damage_exactly() -> Result<(), String> {
    check!(
        bands(0, 0, 16).next().is_none(),
        "an empty damage made a band"
    );
    check!(
        bands(0, 720, 16).count() == 45,
        "720 rows are not 45 bands of 16"
    );
    check!(
        bands(700, 30, 16).collect::<Vec<_>>() == [(700, 16), (716, 14)],
        "a short last band"
    );
    check!(
        bands(5, 3, 16).collect::<Vec<_>>() == [(5, 3)],
        "damage smaller than a band"
    );
    check!(bands(0, 3, 0).count() == 3, "a zero band is one row");
    check!(
        bands(usize::MAX - 2, 10, 16).collect::<Vec<_>>() == [(usize::MAX - 2, 2)],
        "rows past usize::MAX are not invented"
    );
    for (y, h, band) in [
        (0, 720, 16),
        (8, 704, 16),
        (3, 17, 16),
        (0, 1, 1),
        (10, 9, 4),
    ] {
        check(y, h, band)?;
    }
    Ok(())
}

/// Thousands of pseudo-random damage shapes and band sizes.
pub fn bands_stress_shapes() -> Result<(), String> {
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..20_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let y = (seed % 1080) as usize;
        let h = ((seed >> 16) % 1081) as usize;
        let band = ((seed >> 32) % 70) as usize;
        check(y, h, band)?;
    }
    Ok(())
}
