//! RF power (dBm) to RSSI (0..100) conversion (upstream `phy.c`:
//! `rtw_phy_power_2_db`, `rtw_phy_db_2_linear`, `rtw_phy_linear_2_db`,
//! `rtw_phy_rf_power_2_rssi`, with `db_invert_table`).
//!
//! The paths are averaged in the linear domain, through a table of
//! 10^(dB/10) values with 3 fractional bits for the small ones.

const FRAC_BITS: u32 = 3;

/// Upstream `db_invert_table`, copied as is (including its `[8][5]` entry
/// of 1000000 where the series would give 10000000: the conversion must
/// round exactly as upstream's does).
const DB_INVERT_TABLE: [[u64; 8]; 12] = [
    [10, 13, 16, 20, 25, 32, 40, 50],
    [64, 80, 101, 128, 160, 201, 256, 318],
    [401, 505, 635, 800, 1007, 1268, 1596, 2010],
    [316, 398, 501, 631, 794, 1000, 1259, 1585],
    [1995, 2512, 3162, 3981, 5012, 6310, 7943, 10000],
    [12589, 15849, 19953, 25119, 31623, 39811, 50119, 63098],
    [79433, 100000, 125893, 158489, 199526, 251189, 316228, 398107],
    [501187, 630957, 794328, 1000000, 1258925, 1584893, 1995262, 2511886],
    [3162278, 3981072, 5011872, 6309573, 7943282, 1000000, 12589254, 15848932],
    [19952623, 25118864, 31622777, 39810717, 50118723, 63095734, 79432823, 100000000],
    [125892541, 158489319, 199526232, 251188643, 316227766, 398107171, 501187234, 630957345],
    [794328235, 1000000000, 1258925412, 1584893192, 1995262315, 2511886432, 3162277660, 3981071706],
];

/// Upstream `rtw_phy_power_2_db`: dBm to a 0..100 scale (100 at 0 dBm and
/// above, 0 outside -100..20).
fn power_2_db(power: i8) -> u8 {
    if power <= -100 || power >= 20 {
        0
    } else if power >= 0 {
        100
    } else {
        (100 + i16::from(power)) as u8
    }
}

fn db_2_linear(power_db: u8) -> u64 {
    if power_db < 1 {
        return 1;
    }
    let power_db = power_db.min(96);
    let i = usize::from((power_db - 1) >> 3);
    let j = usize::from(power_db - 1) - (i << 3);
    let linear = DB_INVERT_TABLE[i][j];
    if i > 2 {
        linear << FRAC_BITS
    } else {
        linear
    }
}

fn linear_2_db(linear: u64) -> u8 {
    let mut found = None;
    'outer: for i in 0..12 {
        for j in 0..8 {
            let hit = if i <= 2 {
                (linear << FRAC_BITS) <= DB_INVERT_TABLE[i][j]
            } else {
                linear <= DB_INVERT_TABLE[i][j]
            };
            if hit {
                found = Some((i, j));
                break 'outer;
            }
        }
    }
    let Some((mut i, mut j)) = found else {
        return 96;
    };
    // Round to the nearer of the two neighbouring table entries. The
    // subtractions mirror upstream's unsigned arithmetic (wrapping), which
    // only matters across the table's non-monotonic row 2 to 3 seam.
    if !(i == 0 && j == 0) {
        let t = &DB_INVERT_TABLE;
        if j == 0 {
            let (pi, pj) = (if i != 3 { i - 1 } else { 2 }, 7);
            if t[i][0].wrapping_sub(linear) > linear.wrapping_sub(t[pi][pj]) {
                i = pi;
                j = pj;
            }
        } else if t[i][j].wrapping_sub(linear) > linear.wrapping_sub(t[i][j - 1]) {
            j -= 1;
        }
    }
    ((i << 3) + j + 1) as u8
}

/// Upstream `rtw_phy_rf_power_2_rssi`: the RSSI of the first `path_num`
/// entries of `rf_power`, averaged in the linear domain.
pub fn rf_power_2_rssi(rf_power: &[i8], path_num: usize) -> u8 {
    let n = path_num.min(rf_power.len());
    let mut sum: u64 = rf_power[..n].iter().map(|&p| db_2_linear(power_2_db(p))).sum();
    sum = (sum + (1 << (FRAC_BITS - 1))) >> FRAC_BITS;
    match n {
        2 => sum >>= 1,
        3 => sum = (sum + (sum << 1) + (sum << 3)) >> 5,
        4 => sum >>= 2,
        _ => {}
    }
    linear_2_db(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected values from a line-by-line Python model of upstream's C
    // (unsigned arithmetic included), not from this code.
    #[test]
    fn one_path_matches_upstream_model() {
        for dbm in -74i8..=-5 {
            let want = if dbm == -30 { 60 } else { (100 + i16::from(dbm)) as u8 };
            assert_eq!(rf_power_2_rssi(&[dbm], 1), want, "dbm {dbm}");
        }
        let edges: [(i8, u8); 12] = [
            (-110, 1),
            (-99, 1),
            (-98, 3),
            (-96, 5),
            (-89, 12),
            (-88, 12),
            (-79, 22),
            (-76, 24),
            (-75, 25),
            (-4, 96),
            (0, 96),
            (20, 1),
        ];
        for (dbm, want) in edges {
            assert_eq!(rf_power_2_rssi(&[dbm], 1), want, "dbm {dbm}");
        }
    }

    #[test]
    fn two_paths_average_linearly() {
        assert_eq!(rf_power_2_rssi(&[-50, -50], 2), 50);
        assert_eq!(rf_power_2_rssi(&[-40, -60], 2), 57);
        assert_eq!(rf_power_2_rssi(&[-20, -30], 2), 77);
        assert_eq!(rf_power_2_rssi(&[-70, -80], 2), 27);
    }
}
