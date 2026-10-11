//! The security CAM (upstream `sec.c`, `sec.h`): hardware key slots,
//! written with `rtw_sec_write_cam`, cleared with `rtw_sec_clear_cam`, and
//! the engine switched on with `rtw_sec_enable_sec_engine`. The chip does
//! the CCMP arithmetic; software builds the CCMP header (PN) and checks
//! replays (upstream sets `IEEE80211_KEY_FLAG_GENERATE_IV`).
//!
//! Owner: the core/MAC work package.
//!
//! An entry is 8 words written through `RTW_SEC_WRITE_REG` +
//! `RTW_SEC_CMD_REG`: word 0 holds key index, type, group flag, valid and
//! the first two address bytes; word 1 the rest of the address; words 2-5
//! the 16 key bytes; 6-7 zero.

use crate::error::{Error, Result};
use crate::hci::Hci;
use crate::mac::regs::{CR, MAC_SEC_EN};

pub const SEC_CMD_REG: u32 = 0x670;
pub const SEC_WRITE_REG: u32 = 0x674;
pub const SEC_READ_REG: u32 = 0x678;
pub const SEC_CONFIG: u32 = 0x680;

const CAM_ENTRY_SHIFT: u32 = 3;
/// With default key search, entries 0-3 are the group keys by key index.
pub const SEC_DEFAULT_KEY_NUM: u8 = 4;
const CMD_WRITE_ENABLE: u32 = 1 << 16;
const CMD_POLLING: u32 = 1 << 31;

const TX_UNI_USE_DK: u16 = 1 << 0;
const RX_UNI_USE_DK: u16 = 1 << 1;
const TX_DEC_EN: u16 = 1 << 2;
const RX_DEC_EN: u16 = 1 << 3;
const TX_BC_USE_DK: u16 = 1 << 6;
const RX_BC_USE_DK: u16 = 1 << 7;

/// Entries in the CAM (`RTW_MAX_SEC_CAM_NUM`).
pub const MAX_SEC_CAM_NUM: usize = 32;

/// Hardware key types (upstream `enum rtw_hw_key_type`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum HwKeyType {
    #[default]
    None = 0,
    Wep40 = 1,
    Tkip = 2,
    Aes = 4,
    Wep104 = 5,
}

impl HwKeyType {
    /// Key bytes the cipher needs (TKIP: the 16-byte temporal key; its MIC
    /// keys stay in software, upstream asks mac80211 to generate the MMIC).
    pub fn key_len(self) -> usize {
        match self {
            HwKeyType::None => 0,
            HwKeyType::Wep40 => 5,
            HwKeyType::Wep104 => 13,
            HwKeyType::Tkip | HwKeyType::Aes => 16,
        }
    }

    /// The descriptor's `sec_type` for a frame the chip encrypts with this
    /// key (upstream `rtw_tx_pkt_info_update_sec`).
    pub fn tx_sec_type(self) -> u8 {
        match self {
            HwKeyType::None => 0,
            HwKeyType::Wep40 | HwKeyType::Wep104 | HwKeyType::Tkip => 0x01,
            HwKeyType::Aes => 0x03,
        }
    }
}

/// One CAM entry as the driver remembers it (upstream `struct rtw_cam_entry`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CamEntry {
    pub valid: bool,
    pub group: bool,
    pub addr: [u8; 6],
    pub hw_key_type: HwKeyType,
    pub key_idx: u8,
}

/// The CAM (upstream `struct rtw_sec_desc`).
#[derive(Clone, Debug)]
pub struct Cam {
    /// Bit *n* set: entry *n* holds a key (`cam_map`).
    pub used: u64,
    /// Group keys live in entries 0-3 by key index (`default_key_search`).
    pub default_key_search: bool,
    pub total_cam_num: u8,
    pub entries: [CamEntry; MAX_SEC_CAM_NUM],
}

impl Default for Cam {
    fn default() -> Cam {
        Cam {
            used: 0,
            default_key_search: false,
            total_cam_num: crate::rtw8822c::spec::TOTAL_CAM_NUM,
            entries: [CamEntry::default(); MAX_SEC_CAM_NUM],
        }
    }
}

/// A key to install (what `rtw_sec_write_cam` reads from mac80211's
/// `ieee80211_key_conf` and `ieee80211_sta`).
#[derive(Clone, Copy, Debug)]
pub struct KeyConf<'a> {
    pub hw_key_type: HwKeyType,
    /// The 802.11 key index (0-3).
    pub key_idx: u8,
    /// Pairwise (`IEEE80211_KEY_FLAG_PAIRWISE`) or group.
    pub pairwise: bool,
    /// The peer for a pairwise key; `None` writes the broadcast address.
    pub peer: Option<[u8; 6]>,
    pub key: &'a [u8],
}

impl Cam {
    /// Upstream `rtw_sec_get_free_cam`: the first free entry, past the four
    /// default-key entries when default key search is on.
    /// `MAX_SEC_CAM_NUM` when full.
    pub fn get_free_cam(&self) -> usize {
        let start = if self.default_key_search { SEC_DEFAULT_KEY_NUM as usize } else { 0 };
        (start..MAX_SEC_CAM_NUM).find(|&i| self.used & (1 << i) == 0).unwrap_or(MAX_SEC_CAM_NUM)
    }

    /// Upstream `rtw_sec_write_cam`: program entry `hw_key_idx` (bounds
    /// checked here; upstream trusts its caller).
    pub fn write_cam<H: Hci>(&mut self, hci: &mut H, key: &KeyConf<'_>, hw_key_idx: usize) -> Result<()> {
        if hw_key_idx >= MAX_SEC_CAM_NUM || hw_key_idx >= usize::from(self.total_cam_num) {
            return Err(Error::Full("security cam"));
        }
        let need = key.hw_key_type.key_len();
        if key.key.len() < need || key.key_idx > 3 {
            return Err(Error::Invalid("key"));
        }
        // The CAM always takes 16 key bytes; shorter keys are zero padded.
        let mut k = [0u8; 16];
        let n = key.key.len().min(16);
        k[..n].copy_from_slice(&key.key[..n]);

        let entry = CamEntry {
            valid: true,
            group: !key.pairwise,
            addr: key.peer.unwrap_or([0xff; 6]),
            hw_key_type: key.hw_key_type,
            key_idx: key.key_idx,
        };
        self.used |= 1 << hw_key_idx;
        self.entries[hw_key_idx] = entry;

        let write_cmd = CMD_WRITE_ENABLE | CMD_POLLING;
        let addr = (hw_key_idx as u32) << CAM_ENTRY_SHIFT;
        let a = entry.addr.map(u32::from);
        for i in (0..8u32).rev() {
            let content = match i {
                0 => {
                    u32::from(key.key_idx & 0x3)
                        | ((key.hw_key_type as u32 & 0x7) << 2)
                        | (u32::from(entry.group) << 6)
                        | (u32::from(entry.valid) << 15)
                        | (a[0] << 16)
                        | (a[1] << 24)
                }
                1 => a[2] | (a[3] << 8) | (a[4] << 16) | (a[5] << 24),
                6 | 7 => 0,
                _ => {
                    let j = ((i - 2) << 2) as usize;
                    u32::from_le_bytes([k[j], k[j + 1], k[j + 2], k[j + 3]])
                }
            };
            hci.write32(SEC_WRITE_REG, content);
            hci.write32(SEC_CMD_REG, write_cmd | (addr + i));
        }
        Ok(())
    }

    /// Upstream `rtw_sec_clear_cam`: invalidate entry `hw_key_idx` (word 0
    /// written as zero).
    pub fn clear_cam<H: Hci>(&mut self, hci: &mut H, hw_key_idx: usize) -> Result<()> {
        if hw_key_idx >= MAX_SEC_CAM_NUM {
            return Err(Error::Invalid("cam index"));
        }
        self.used &= !(1 << hw_key_idx);
        self.entries[hw_key_idx] = CamEntry::default();
        let addr = (hw_key_idx as u32) << CAM_ENTRY_SHIFT;
        hci.write32(SEC_WRITE_REG, 0);
        hci.write32(SEC_CMD_REG, CMD_WRITE_ENABLE | CMD_POLLING | addr);
        Ok(())
    }

    /// Upstream `rtw_sec_enable_sec_engine`: security engine on, TX/RX
    /// decryption on, default keys for unicast and broadcast.
    pub fn enable_sec_engine<H: Hci>(&mut self, hci: &mut H) {
        self.default_key_search = true;
        let ctrl = hci.read16(CR) | MAC_SEC_EN;
        hci.write16(CR, ctrl);

        let mut sec_config = hci.read16(SEC_CONFIG) | TX_DEC_EN | RX_DEC_EN;
        if self.default_key_search {
            sec_config |= TX_UNI_USE_DK | RX_UNI_USE_DK | TX_BC_USE_DK | RX_BC_USE_DK;
        }
        hci.write16(SEC_CONFIG, sec_config);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeHci;

    /// The (data, command) pairs written to the CAM, in order.
    fn cam_writes(f: &FakeHci) -> std::vec::Vec<(u32, u32)> {
        let w = f.writes();
        w.chunks(2).filter(|p| p.len() == 2).map(|p| (p[0].2, p[1].2)).collect()
    }

    #[test]
    fn ccmp_pairwise_key_layout_matches_upstream() {
        let mut f = FakeHci::default();
        let mut cam = Cam::default();
        cam.enable_sec_engine(&mut f);
        f.log.clear();
        let key: std::vec::Vec<u8> = (0x10..0x20).collect();
        let peer = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
        let kc = KeyConf { hw_key_type: HwKeyType::Aes, key_idx: 0, pairwise: true, peer: Some(peer), key: &key };
        let idx = cam.get_free_cam();
        assert_eq!(idx, 4, "pairwise keys go past the default-key entries");
        cam.write_cam(&mut f, &kc, idx).unwrap();

        let w = cam_writes(&f);
        assert_eq!(w.len(), 8);
        // Words go 7 down to 0, command = enable | polling | (idx << 3) + i.
        for (n, (_, cmd)) in w.iter().enumerate() {
            assert_eq!(*cmd, 0x8001_0000 | ((4 << 3) + (7 - n as u32)));
        }
        assert_eq!(w[0].0, 0);
        assert_eq!(w[1].0, 0);
        assert_eq!(w[2].0, 0x1F1E_1D1C); // word 5: key[12..16]
        assert_eq!(w[5].0, 0x1312_1110); // word 2: key[0..4]
        assert_eq!(w[6].0, 0x5544_3322); // word 1: addr[2..6]
        // word 0: keyidx 0 | AES (4) << 2 | group 0 | valid << 15 | addr[0..2] << 16
        assert_eq!(w[7].0, (4 << 2) | (1 << 15) | (0x02 << 16) | (0x11 << 24));
        assert_eq!(cam.used, 1 << 4);
    }

    #[test]
    fn group_key_uses_its_index_and_broadcast() {
        let mut f = FakeHci::default();
        let mut cam = Cam::default();
        let key = [0xAAu8; 16];
        let kc = KeyConf { hw_key_type: HwKeyType::Aes, key_idx: 1, pairwise: false, peer: None, key: &key };
        cam.write_cam(&mut f, &kc, 1).unwrap();
        let w0 = cam_writes(&f)[7].0;
        assert_eq!(w0, 1 | (4 << 2) | (1 << 6) | (1 << 15) | (0xff << 16) | (0xff << 24));
        assert!(cam.entries[1].group);
    }

    #[test]
    fn clear_and_bounds() {
        let mut f = FakeHci::default();
        let mut cam = Cam::default();
        cam.enable_sec_engine(&mut f);
        let key = [1u8; 16];
        let kc = KeyConf { hw_key_type: HwKeyType::Aes, key_idx: 0, pairwise: true, peer: Some([2; 6]), key: &key };
        for _ in 4..MAX_SEC_CAM_NUM {
            let i = cam.get_free_cam();
            cam.write_cam(&mut f, &kc, i).unwrap();
        }
        assert_eq!(cam.get_free_cam(), MAX_SEC_CAM_NUM);
        assert!(matches!(cam.write_cam(&mut f, &kc, MAX_SEC_CAM_NUM), Err(Error::Full(_))));
        f.log.clear();
        cam.clear_cam(&mut f, 9).unwrap();
        assert_eq!(f.writes(), std::vec![(4, SEC_WRITE_REG, 0), (4, SEC_CMD_REG, 0x8001_0000 | (9 << 3))]);
        assert_eq!(cam.get_free_cam(), 9);
        assert!(cam.clear_cam(&mut f, 99).is_err());
        let short = KeyConf { key: &key[..5], ..kc };
        assert!(matches!(cam.write_cam(&mut f, &short, 9), Err(Error::Invalid(_))));
    }

    #[test]
    fn engine_enable_sets_default_key_search() {
        let mut f = FakeHci::default();
        let mut cam = Cam::default();
        cam.enable_sec_engine(&mut f);
        assert!(cam.default_key_search);
        assert_eq!(f.peek32(CR) & u32::from(MAC_SEC_EN), u32::from(MAC_SEC_EN));
        assert_eq!(f.peek32(SEC_CONFIG) & 0xffff, 0xCF);
    }
}
