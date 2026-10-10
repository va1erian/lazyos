//! The group key handshake (IEEE 802.11-2020 12.7.7): message 1 from the AP
//! delivers a new GTK under the KEK; the supplicant answers with message 2.

use alloc::vec::Vec;

use crate::crypto::Crypto;
use crate::handshake::CCMP_KEY_LEN;
use crate::key::{info, KeyFrame};
use crate::keydata;
use crate::secret::Key;
use crate::supplicant::Supplicant;
use crate::{Action, Error};

impl<C: Crypto> Supplicant<C> {
    /// Group message 1 to group message 2, plus the new GTK unless it is the
    /// one already installed (a repeated delivery must not reset the chip's
    /// packet number window).
    pub(crate) fn on_group_msg1(&mut self, frame: &KeyFrame) -> Result<Vec<Action>, Error> {
        let Some(installed) = &self.state.installed else {
            return Err(Error::Unexpected);
        };
        self.verify_mic(frame, &installed.kck)?;
        let counter = frame.replay_counter();
        if self.state.rx_replay.is_some_and(|seen| counter <= seen) {
            return Err(Error::Replay);
        }
        let plain = self
            .crypto
            .key_unwrap(&installed.kek, &frame.key_data)
            .ok_or(Error::BadKeyData)?;
        let gtk = keydata::parse(&plain)?.gtk.ok_or(Error::MissingGtk)?;
        let key: [u8; CCMP_KEY_LEN] = gtk.key.try_into().map_err(|_| Error::BadGtk)?;
        let key = Key::new(key);
        let reply = self.signed_reply(
            frame,
            info::MIC | info::SECURE,
            [0; 32],
            Vec::new(),
            &installed.kck,
        )?;
        let same = installed.gtk.as_ref().is_some_and(|(index, held)| {
            *index == gtk.index && lazyos_crypto::wifi::mic_eq(held.expose(), key.expose())
        });
        let mut actions = alloc::vec![Action::Send(reply)];
        if !same {
            actions.push(Action::InstallGtk {
                index: gtk.index,
                tx: gtk.tx,
                key: key.clone(),
                rsc: frame.rsc,
            });
        }
        self.state.rx_replay = Some(counter);
        if let Some(installed) = &mut self.state.installed {
            installed.gtk = Some((gtk.index, key));
        }
        Ok(actions)
    }
}
