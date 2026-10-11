//! The efuse TX power index table of one RF path (upstream `struct
//! rtw_txpwr_idx` and its `rtw_2g_*`/`rtw_5g_*` members, `main.h`).
//!
//! 42 packed bytes per path. The differences are signed 4-bit fields
//! (little-endian bitfields: the first declared field is the low nibble).

/// Bytes of one path's table.
pub const TXPWR_IDX_LEN: usize = 42;

/// `rtw_2g_1s_pwr_idx_diff` / `rtw_5g_ht_1s_pwr_idx_diff`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diff1s {
    pub ofdm: i8,
    pub bw20: i8,
}

/// `rtw_2g_ns_pwr_idx_diff` (2.4 GHz) / `rtw_5g_ht_ns_pwr_idx_diff`
/// (5 GHz, which has no `cck`/`ofdm`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffNs {
    pub bw20: i8,
    pub bw40: i8,
    pub cck: i8,
    pub ofdm: i8,
}

/// `rtw_5g_ofdm_ns_pwr_idx_diff`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffOfdmNs {
    pub ofdm_3s: i8,
    pub ofdm_2s: i8,
    pub ofdm_4s: i8,
}

/// `rtw_5g_vht_ns_pwr_idx_diff`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffVht {
    pub bw160: i8,
    pub bw80: i8,
}

/// One path's power index table (`struct rtw_txpwr_idx`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxPwrIdx {
    pub cck_base_2g: [u8; 6],
    pub bw40_base_2g: [u8; 5],
    pub ht_1s_2g: Diff1s,
    /// `ht_2s_diff`, `ht_3s_diff`, `ht_4s_diff`.
    pub ht_ns_2g: [DiffNs; 3],
    pub bw40_base_5g: [u8; 14],
    pub ht_1s_5g: Diff1s,
    /// `ht_2s_diff` to `ht_4s_diff` (only `bw20`/`bw40` are set).
    pub ht_ns_5g: [DiffNs; 3],
    pub ofdm_ns_5g: DiffOfdmNs,
    /// `vht_1s_diff` to `vht_4s_diff`.
    pub vht_ns_5g: [DiffVht; 4],
}

fn lo(b: u8) -> i8 {
    ((b << 4) as i8) >> 4
}

fn hi(b: u8) -> i8 {
    (b as i8) >> 4
}

impl TxPwrIdx {
    /// Decode one path's 42 bytes.
    pub fn parse(b: &[u8; TXPWR_IDX_LEN]) -> TxPwrIdx {
        let mut t = TxPwrIdx::default();
        t.cck_base_2g.copy_from_slice(&b[0..6]);
        t.bw40_base_2g.copy_from_slice(&b[6..11]);
        t.ht_1s_2g = Diff1s { ofdm: lo(b[11]), bw20: hi(b[11]) };
        for (n, d) in t.ht_ns_2g.iter_mut().enumerate() {
            let (x, y) = (b[12 + 2 * n], b[13 + 2 * n]);
            *d = DiffNs { bw20: lo(x), bw40: hi(x), cck: lo(y), ofdm: hi(y) };
        }
        t.bw40_base_5g.copy_from_slice(&b[18..32]);
        t.ht_1s_5g = Diff1s { ofdm: lo(b[32]), bw20: hi(b[32]) };
        for (n, d) in t.ht_ns_5g.iter_mut().enumerate() {
            let x = b[33 + n];
            *d = DiffNs { bw20: lo(x), bw40: hi(x), cck: 0, ofdm: 0 };
        }
        t.ofdm_ns_5g = DiffOfdmNs { ofdm_3s: lo(b[36]), ofdm_2s: hi(b[36]), ofdm_4s: lo(b[37]) };
        for (n, d) in t.vht_ns_5g.iter_mut().enumerate() {
            let x = b[38 + n];
            *d = DiffVht { bw160: lo(x), bw80: hi(x) };
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nibbles_are_signed_low_first() {
        let mut b = [0u8; TXPWR_IDX_LEN];
        b[0] = 0x2d;
        b[10] = 0x30;
        b[11] = 0xf2; // ofdm +2, bw20 -1
        b[12] = 0x8e; // 2s: bw20 -2, bw40 -8
        b[13] = 0x71; // 2s: cck +1, ofdm +7
        b[18] = 0x28;
        b[31] = 0x29;
        b[32] = 0x1f; // ofdm -1, bw20 +1
        b[36] = 0xe3; // ofdm_3s +3, ofdm_2s -2
        b[37] = 0x0c; // ofdm_4s -4
        b[41] = 0xa5; // vht_4s: bw160 +5, bw80 -6
        let t = TxPwrIdx::parse(&b);
        assert_eq!(t.cck_base_2g[0], 0x2d);
        assert_eq!(t.bw40_base_2g[4], 0x30);
        assert_eq!(t.ht_1s_2g, Diff1s { ofdm: 2, bw20: -1 });
        assert_eq!(t.ht_ns_2g[0], DiffNs { bw20: -2, bw40: -8, cck: 1, ofdm: 7 });
        assert_eq!(t.bw40_base_5g[0], 0x28);
        assert_eq!(t.bw40_base_5g[13], 0x29);
        assert_eq!(t.ht_1s_5g, Diff1s { ofdm: -1, bw20: 1 });
        assert_eq!(t.ofdm_ns_5g, DiffOfdmNs { ofdm_3s: 3, ofdm_2s: -2, ofdm_4s: -4 });
        assert_eq!(t.vht_ns_5g[3], DiffVht { bw160: 5, bw80: -6 });
    }
}
