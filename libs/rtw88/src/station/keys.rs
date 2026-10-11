//! Keys (upstream `mac80211.c` `rtw_ops_set_key`).
//!
//! The CAM holds the key and the chip encrypts and decrypts, but the IV is
//! the host's: upstream sets `IEEE80211_KEY_FLAG_GENERATE_IV`, so the
//! soft-MAC builds the CCMP header (PN) of every protected frame it hands
//! to [`Dev::tx_data`] and checks the PN of every frame it receives. CCMP
//! management frames (802.11w) are encrypted in software
//! (`IEEE80211_KEY_FLAG_SW_MGMT_TX`); TKIP's Michael MIC is software's too
//! (`IEEE80211_KEY_FLAG_GENERATE_MMIC`).
//!
//! Upstream leaves deep power save before touching the CAM
//! (`rtw_leave_lps_deep`); this driver never enters it, so there is nothing
//! to leave.

use crate::dev::Dev;
use crate::error::{Error, Result};
use crate::hci::Hci;
use crate::mac;
use crate::sec::{HwKeyType, KeyConf};

/// The ciphers the CAM takes (upstream's `key->cipher` switch). CMAC, GMAC,
/// GCMP and CCMP-256 are refused, as upstream (`-EOPNOTSUPP`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cipher {
    Wep40,
    Wep104,
    Tkip,
    Ccmp,
}

impl Cipher {
    fn hw_key_type(self) -> HwKeyType {
        match self {
            Cipher::Wep40 => HwKeyType::Wep40,
            Cipher::Wep104 => HwKeyType::Wep104,
            Cipher::Tkip => HwKeyType::Tkip,
            Cipher::Ccmp => HwKeyType::Aes,
        }
    }
}

impl<H: Hci> Dev<H> {
    /// Upstream `rtw_ops_set_key(SET_KEY)`: a pairwise key (for `peer`, the
    /// AP) takes the first free CAM entry past the default keys; a group key
    /// (`peer` `None`) takes entry `key_idx`. Returns the CAM entry, which
    /// [`Dev::del_key`] takes back.
    pub fn set_key(&mut self, cipher: Cipher, key_idx: u8, peer: Option<[u8; 6]>, key: &[u8]) -> Result<u8> {
        if key_idx > 3 {
            return Err(Error::Invalid("key index"));
        }
        let pairwise = peer.is_some();
        let hw_key_idx = if pairwise { self.sec.get_free_cam() } else { usize::from(key_idx) };
        // Upstream compares with `>`, letting index 32 through when the CAM
        // is full; `>=` keeps the write inside the table.
        if hw_key_idx >= usize::from(self.sec.total_cam_num) {
            return Err(Error::Full("security cam"));
        }
        let conf = KeyConf { hw_key_type: cipher.hw_key_type(), key_idx, pairwise, peer, key };
        self.sec.write_cam(&mut self.hci, &conf, hw_key_idx)?;
        Ok(hw_key_idx as u8)
    }

    /// Upstream `rtw_ops_set_key(DISABLE_KEY)`: let the queues drain, then
    /// clear the entry.
    pub fn del_key(&mut self, hw_key_idx: u8) -> Result<()> {
        // contract: upstream also flushes the bus queues
        // (`rtw_hci_flush_all_queues`); `Hci` has no flush yet, the MAC
        // flush below waits for the chip's side.
        mac::flush_queues(self, false);
        self.sec.clear_cam(&mut self.hci, usize::from(hw_key_idx))
    }

    /// The key type of CAM entry `hw_key_idx`, for the TX descriptor.
    pub fn key_type(&self, hw_key_idx: u8) -> Option<HwKeyType> {
        let e = self.sec.entries.get(usize::from(hw_key_idx))?;
        e.valid.then_some(e.hw_key_type)
    }
}
