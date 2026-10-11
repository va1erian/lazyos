//! Association control: `Join`, `SetState`, `SetKey`, `DelKey`, `Leave`,
//! and what ending an association does to the radio.

use alloc::vec::Vec;

use ieee80211::ccmp::{rsc_from_bytes, Replay};
use ieee80211::{is_group, Mac};

use crate::keys::Pairwise;
use crate::mac::{bss_traits, Bss, SoftMac, DEFAULT_INTERVAL_MS, REASON_LEAVING};
use crate::radio::{Channel, KeyInstall, KeyKind, Radio, StaState, Width, CIPHER_CCMP128};
use crate::Error;

/// `Join`'s request, as the wire carries it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JoinRequest {
    pub bssid: Vec<u8>,
    pub channel: u32,
    pub band: u32,
    pub width: u32,
}

/// `SetKey`'s arguments, as the wire carries them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyRequest<'a> {
    pub kind: u32,
    pub index: u32,
    pub cipher: u32,
    pub key: &'a [u8],
    pub rsc: &'a [u8],
    pub addr: &'a [u8],
}

impl<R: Radio> SoftMac<R> {    /// `Join`: tune to the BSS and filter to it, leaving any previous one.
    pub fn join(&mut self, caller: u64, request: &JoinRequest, now: u64) -> Result<(), Error> {
        self.owned(caller)?;
        let domain = self.domain.ok_or(Error::Again)?;
        let bssid: Mac = request
            .bssid
            .as_slice()
            .try_into()
            .map_err(|_| Error::Invalid)?;
        if is_group(&bssid) || bssid == [0; 6] {
            return Err(Error::Invalid);
        }
        let band = self.band_of(request.band, request.channel)?;
        let number = u8::try_from(request.channel).map_err(|_| Error::Invalid)?;
        domain.allows(band, number).ok_or(Error::Invalid)?;
        let width = Width::from_u32(request.width).ok_or(Error::Invalid)?;
        if self.scanner.running.is_some() {
            self.finish_scan(true, now);
        }
        self.disconnect(Some(REASON_LEAVING));
        let channel = Channel {
            band,
            number,
            width,
        };
        self.radio.tune(channel).map_err(|e| self.radio_failed(e))?;
        self.radio
            .set_bssid(Some(bssid), 0)
            .map_err(|e| self.radio_failed(e))?;
        let (qos, interval_ms) = self
            .scanner
            .latest_of(&bssid)
            .and_then(|entry| bss_traits(&entry.frame))
            .unwrap_or((false, DEFAULT_INTERVAL_MS));
        self.bss = Some(Bss {
            bssid,
            channel,
            aid: 0,
            qos,
            interval_ms,
            last_beacon: now,
            loss_reported: false,
            protected: false,
        });
        self.state = StaState::Idle;
        Ok(())
    }

    /// `SetState`.
    pub fn set_state(&mut self, caller: u64, state: u32, aid: u32) -> Result<(), Error> {
        self.owned(caller)?;
        let state = StaState::from_u32(state).ok_or(Error::Invalid)?;
        let Some(bss) = self.bss else {
            if state == StaState::Idle {
                self.state = state;
                return Ok(());
            }
            return Err(Error::NotConnected);
        };
        if state == StaState::Authorized && self.keys.pairwise.is_none() && self.keys.any() {
            return Err(Error::Invalid);
        }
        let aid = if state >= StaState::Associated {
            u16::try_from(aid)
                .ok()
                .filter(|aid| (1..=2007).contains(aid))
                .ok_or(Error::Invalid)?
        } else {
            0
        };
        if aid != bss.aid {
            self.radio
                .set_bssid(Some(bss.bssid), aid)
                .map_err(|e| self.radio_failed(e))?;
        }
        self.radio
            .set_state(state)
            .map_err(|e| self.radio_failed(e))?;
        if let Some(bss) = self.bss.as_mut() {
            bss.aid = aid;
        }
        self.state = state;
        if state != StaState::Authorized {
            self.rx_eth.clear();
        }
        Ok(())
    }

    /// `SetKey`: CCMP-128 pairwise (id 0 or 1) or group (id 1-3) keys for the
    /// joined BSS. IGTKs are refused: no management frame protection.
    pub fn set_key(&mut self, caller: u64, request: &KeyRequest<'_>) -> Result<(), Error> {
        self.owned(caller)?;
        let bss = self.bss.ok_or(Error::NotConnected)?;
        let kind = KeyKind::from_u32(request.kind).ok_or(Error::Invalid)?;
        let index = u8::try_from(request.index).map_err(|_| Error::Invalid)?;
        let index_ok = match kind {
            KeyKind::Pairwise => index <= 1,
            KeyKind::Group => (1..=3).contains(&index),
            KeyKind::Igtk => false,
        };
        let supported = self.info.ciphers & (1 << CIPHER_CCMP128) != 0;
        if !index_ok || request.cipher != CIPHER_CCMP128 || !supported || request.key.len() != 16
        {
            return Err(Error::Invalid);
        }
        let rsc = rsc_from_bytes(request.rsc).map_err(|_| Error::Invalid)?;
        if request.addr != bss.bssid {
            return Err(Error::Invalid);
        }
        let install = KeyInstall {
            kind,
            index,
            cipher: CIPHER_CCMP128,
            key: request.key,
            rsc,
            peer: bss.bssid,
        };
        self.radio
            .set_key(&install)
            .map_err(|e| self.radio_failed(e))?;
        match kind {
            KeyKind::Pairwise => {
                self.keys.pairwise = Some(Pairwise {
                    index,
                    tx_pn: 0,
                    replay: Replay::new(rsc),
                })
            }
            _ => self.keys.group[usize::from(index)] = Some(Replay::new(rsc)),
        }
        if let Some(bss) = self.bss.as_mut() {
            bss.protected = true;
        }
        Ok(())
    }

    /// `DelKey`: not an error when the slot is empty.
    pub fn del_key(&mut self, caller: u64, kind: u32, index: u32) -> Result<(), Error> {
        self.owned(caller)?;
        let kind = KeyKind::from_u32(kind).ok_or(Error::Invalid)?;
        let index = u8::try_from(index)
            .ok()
            .filter(|&i| i <= 5)
            .ok_or(Error::Invalid)?;
        self.radio
            .del_key(kind, index)
            .map_err(|e| self.radio_failed(e))?;
        match kind {
            KeyKind::Pairwise if self.keys.pairwise.is_some_and(|k| k.index == index) => {
                self.keys.pairwise = None
            }
            KeyKind::Group if index <= 3 => self.keys.group[usize::from(index)] = None,
            _ => {}
        }
        Ok(())
    }

    /// `Leave`: deauthenticate with `reason`, delete the keys, go idle.
    pub fn leave(&mut self, caller: u64, reason: u32) -> Result<(), Error> {
        self.owned(caller)?;
        let reason = u16::try_from(reason).map_err(|_| Error::Invalid)?;
        self.disconnect(Some(reason));
        Ok(())
    }

    /// Delete every key from the radio and the table.
    pub(crate) fn drop_keys(&mut self) {
        if let Some(key) = self.keys.pairwise {
            let _ = self.radio.del_key(KeyKind::Pairwise, key.index);
        }
        for index in 1..=3u8 {
            if self.keys.group[usize::from(index)].is_some() {
                let _ = self.radio.del_key(KeyKind::Group, index);
            }
        }
        self.keys.clear();
    }

    /// The AP ended the association (or we did): keys gone, link down, the
    /// radio stays on the BSS so the client can authenticate again.
    pub(crate) fn drop_association(&mut self) {
        self.drop_keys();
        self.state = StaState::Idle;
        let _ = self.radio.set_state(StaState::Idle);
        if let Some(bss) = self.bss.as_mut() {
            bss.aid = 0;
            bss.protected = false;
        }
        self.rx_eth.clear();
    }

    /// Leave the BSS, sending a deauthentication with `reason` when the AP
    /// knows us.
    pub(crate) fn disconnect(&mut self, reason: Option<u16>) {
        let Some(bss) = self.bss else {
            return;
        };
        if let Some(reason) = reason.filter(|_| self.state >= StaState::Authenticated) {
            self.send_deauth(&bss.bssid, reason);
        }
        self.drop_association();
        let _ = self.radio.set_bssid(None, 0);
        self.bss = None;
    }
}
