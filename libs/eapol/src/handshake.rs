//! The pairwise half of the supplicant: message 1 to message 2, message 3 to
//! message 4 (IEEE 802.11-2020 12.7.6.2 to 12.7.6.5).
//!
//! Each handler validates everything first and mutates `self.state` only on
//! its last lines, so an `Err` never leaves a trace.

use alloc::vec::Vec;

use ieee80211::Cipher;

use crate::crypto::Crypto;
use crate::key::{info, KeyFrame};
use crate::keydata;
use crate::secret::Key;
use crate::supplicant::{Installed, Pending, Supplicant, MAX_PENDING};
use crate::{Action, Error};

/// The CCMP-128 temporal key length, also the Key Length field of message 3.
pub(crate) const CCMP_KEY_LEN: usize = 16;

impl<C: Crypto> Supplicant<C> {
    /// Message 1 (ANonce) to message 2 (SNonce, MIC, our RSN element).
    pub(crate) fn on_msg1(&mut self, frame: &KeyFrame) -> Result<Vec<Action>, Error> {
        if frame.nonce == [0; 32] {
            return Err(Error::ZeroAnonce);
        }
        let counter = frame.replay_counter();
        if self.state.rx_replay.is_some_and(|seen| counter <= seen) {
            return Err(Error::Replay);
        }
        if let Some(installed) = &self.state.installed {
            if installed.anonce == frame.nonce {
                return Err(Error::StaleAnonce);
            }
        }
        // Same ANonce as a candidate: a retransmitted message 1, so keep its
        // SNonce and PTK. Otherwise a fresh SNonce and PTK.
        let existing = self
            .state
            .pending
            .iter()
            .position(|p| p.anonce == frame.nonce);
        let (snonce, ptk, msg1_counter) = match existing {
            Some(i) => {
                let p = &self.state.pending[i];
                (p.snonce, p.ptk.clone(), p.msg1_counter.max(counter))
            }
            None => {
                let snonce = self.crypto.random_nonce();
                let ptk = self.crypto.derive_ptk(
                    self.cfg.akm,
                    &self.cfg.pmk,
                    &self.cfg.aa,
                    &self.cfg.spa,
                    &frame.nonce,
                    &snonce,
                );
                (snonce, ptk, counter)
            }
        };
        let msg2 = self.signed_reply(
            frame,
            info::PAIRWISE | info::MIC,
            snonce,
            self.cfg.assoc_rsn_ie.clone(),
            &Key::slice_of(&ptk, 0),
        )?;
        let candidate = Pending {
            anonce: frame.nonce,
            snonce,
            ptk,
            msg1_counter,
        };
        match existing {
            Some(i) => self.state.pending[i] = candidate,
            None => {
                // Message 1 is unauthenticated: keep a few candidates so a
                // forged one cannot displace the genuine one, and drop the
                // oldest when the bound is reached.
                if self.state.pending.len() >= MAX_PENDING {
                    self.state.pending.remove(0);
                }
                self.state.pending.push(candidate);
            }
        }
        Ok(alloc::vec![Action::Send(msg2)])
    }

    /// Message 3 to message 4, and the keys to install.
    ///
    /// Order of checks: candidate by ANonce, MIC (nothing in the frame is believed before
    /// it), replay counter, ANonce, key length, unwrap, key data, RSN element
    /// against the beacon's, GTK. A message 3 repeated after the PTK is
    /// installed only gets message 4 again.
    pub(crate) fn on_msg3(&mut self, frame: &KeyFrame) -> Result<Vec<Action>, Error> {
        // The candidate whose ANonce message 3 carries; its PTK verifies the
        // MIC. Others stay until a MIC-verified message 3 discards them.
        let candidate = self
            .state
            .pending
            .iter()
            .position(|p| p.anonce == frame.nonce);
        let (kck, kek, floor, installed) = match (candidate, &self.state.installed) {
            (Some(i), _) => {
                let p = &self.state.pending[i];
                (
                    Key::slice_of(&p.ptk, 0),
                    Key::slice_of(&p.ptk, 16),
                    self.state.rx_replay.max(Some(p.msg1_counter)),
                    false,
                )
            }
            (None, Some(i)) if i.anonce == frame.nonce => {
                (i.kck.clone(), i.kek.clone(), self.state.rx_replay, true)
            }
            (None, None) if self.state.pending.is_empty() => return Err(Error::Unexpected),
            (None, _) => return Err(Error::AnonceChanged),
        };
        self.verify_mic(frame, &kck)?;
        if floor.is_some_and(|seen| frame.replay_counter() <= seen) {
            return Err(Error::Replay);
        }
        let msg4 = self.signed_reply(
            frame,
            info::PAIRWISE | info::MIC | info::SECURE,
            [0; 32],
            Vec::new(),
            &kck,
        )?;
        if installed {
            // The AP missed message 4. Answer; install nothing.
            self.state.rx_replay = Some(frame.replay_counter());
            return Ok(alloc::vec![Action::Send(msg4)]);
        }
        if usize::from(frame.key_length) != CCMP_KEY_LEN {
            return Err(Error::BadKeyLength(frame.key_length));
        }
        let plain = self
            .crypto
            .key_unwrap(&kek, &frame.key_data)
            .ok_or(Error::BadKeyData)?;
        let data = keydata::parse(&plain)?;
        match data.rsn_ie {
            None => return Err(Error::MissingRsn),
            Some(ie) if ie != self.cfg.ap_rsn_ie.as_slice() => return Err(Error::RsnMismatch),
            Some(_) => {}
        }
        let gtk = data.gtk.ok_or(Error::MissingGtk)?;
        let key: [u8; CCMP_KEY_LEN] = gtk.key.try_into().map_err(|_| Error::BadGtk)?;
        let Some(chosen) = candidate else {
            return Err(Error::Unexpected);
        };
        let pending = self.state.pending.swap_remove(chosen);
        self.state.pending.clear();
        let tk = Key::slice_of(&pending.ptk, 32);
        self.state.rx_replay = Some(frame.replay_counter());
        self.state.installed = Some(Installed {
            kck,
            kek,
            anonce: pending.anonce,
            gtk: Some((gtk.index, Key::new(key))),
        });
        Ok(alloc::vec![
            Action::Send(msg4),
            Action::InstallPtk {
                cipher: Cipher::Ccmp128,
                tk,
            },
            Action::InstallGtk {
                index: gtk.index,
                tx: gtk.tx,
                key: Key::new(key),
                rsc: frame.rsc,
            },
            Action::Authorized,
        ])
    }
}
