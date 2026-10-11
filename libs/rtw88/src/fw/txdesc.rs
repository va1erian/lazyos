//! TEMPORARY SHIM, remove at merge: a private copy of upstream
//! `rtw_tx_fill_tx_desc` (`tx.c`).
//!
//! contract: rtw88-pci owns `crate::tx::fill_tx_desc(&TxPktInfo, &mut [u8])`.
//! It did not exist in this worktree when the MAC package was written, so
//! the reserved-page download, the H2C packets and the station TX builders
//! call this copy. At merge, replace every `txdesc::fill_tx_desc` with
//! `crate::tx::fill_tx_desc` and delete this file; the host tests compare
//! descriptor words and will catch a disagreement.

use crate::tx::TxPktInfo;

/// `TX_DESC_QSEL_*` (upstream `enum rtw_tx_desc_queue_select`).
pub const QSEL_BEACON: u8 = 16;
pub const QSEL_HIGH: u8 = 17;
pub const QSEL_MGMT: u8 = 18;
pub const QSEL_H2C: u8 = 19;

/// `DESC_RATE24M`, the RTS rate upstream uses.
const DESC_RATE24M: u32 = 0x08;

fn put(desc: &mut [u8], word: usize, val: u32) {
    let at = word * 4;
    let old = u32::from_le_bytes([desc[at], desc[at + 1], desc[at + 2], desc[at + 3]]);
    desc[at..at + 4].copy_from_slice(&(old | val).to_le_bytes());
}

/// Any descriptor field value as a word.
fn b<T: Into<u32>>(v: T) -> u32 {
    v.into()
}

fn bits(val: u32, mask: u32) -> u32 {
    (val << mask.trailing_zeros()) & mask
}

/// Fill the first 40 bytes of the 48-byte descriptor `desc` (zeroed by the
/// caller) from `info`, word by word as upstream. Shorter buffers are left
/// untouched; the callers always pass a full descriptor.
pub fn fill_tx_desc(info: &TxPktInfo, desc: &mut [u8]) {
    if desc.len() < crate::tx::TX_PKT_DESC_SZ {
        return;
    }
    let more_data = info.qsel == QSEL_HIGH;

    put(
        desc,
        0,
        bits(info.tx_pkt_size, 0xFFFF)
            | bits(b(info.offset), 0x00FF_0000)
            | bits(b(info.bmc), 1 << 24)
            | bits(b(info.ls), 1 << 26)
            | bits(b(info.dis_qselseq), 1 << 31),
    );
    put(
        desc,
        1,
        bits(b(info.mac_id), 0xFF)
            | bits(b(info.qsel), 0x1F00)
            | bits(b(info.rate_id), 0x001F_0000)
            | bits(b(info.sec_type), 0x00C0_0000)
            | bits(b(info.pkt_offset), 0x1F00_0000)
            | bits(b(more_data), 1 << 29),
    );
    put(
        desc,
        2,
        bits(b(info.ampdu_en), 1 << 12)
            | bits(b(info.report), 1 << 19)
            | bits(b(info.ampdu_density), 0x0070_0000)
            | bits(b(info.bt_null), 1 << 23),
    );
    put(
        desc,
        3,
        bits(b(info.hw_ssn_sel), 0xC0)
            | bits(b(info.use_rate), 1 << 8)
            | bits(b(info.dis_rate_fallback), 1 << 10)
            | bits(b(info.rts), 1 << 12)
            | bits(b(info.nav_use_hdr), 1 << 15)
            | bits(b(info.ampdu_factor), 0x003E_0000),
    );
    put(desc, 4, bits(b(info.rate), 0x7F));
    put(
        desc,
        5,
        bits(b(info.short_gi), 1 << 4)
            | bits(b(info.bw), 0x60)
            | bits(b(info.ldpc), 1 << 7)
            | bits(b(info.stbc), 0x300),
    );
    put(desc, 6, bits(b(info.sn), 0xFFF));
    put(desc, 8, bits(b(info.en_hwseq), 1 << 15));
    put(desc, 9, bits(b(info.seq), 0x00FF_F000));
    if info.rts {
        put(desc, 4, bits(DESC_RATE24M, 0x1F00_0000));
        put(desc, 5, bits(1, 1 << 12));
    }
    if info.tim_offset != 0 {
        put(desc, 9, bits(1, 1 << 7) | bits(b(info.tim_offset), 0x7F));
    }
}
