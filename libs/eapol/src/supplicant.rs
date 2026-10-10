//! The supplicant: configuration, state, and the dispatch of a received frame.

use alloc::vec::Vec;

use ieee80211::{Akm, Cipher, Rsn};

use crate::crypto::{Crypto, PTK_LEN};
use crate::key::{self, info, KeyFrame};
use crate::secret::Key;
use crate::{Action, Error};

/// What `wlanmd` knows when the association completes.
pub struct Config {
    /// The AKM chosen: [`Akm::Psk`] or [`Akm::PskSha256`].
    pub akm: Akm,
    pub pairwise: Cipher,
    pub group: Cipher,
    /// The pairwise master key, from `keyd`.
    pub pmk: Key<32>,
    /// The BSSID (authenticator address, AA).
    pub aa: [u8; 6],
    /// Our address (supplicant address, SPA).
    pub spa: [u8; 6],
    /// The RSN element, whole, from the AP's beacon or probe response. Message
    /// 3 must carry exactly these octets.
    pub ap_rsn_ie: Vec<u8>,
    /// The RSN element, whole, sent in the association request. Message 2
    /// carries it back.
    pub assoc_rsn_ie: Vec<u8>,
}

/// Where the handshake stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Waiting for message 1.
    Start,
    /// Message 2 sent; waiting for message 3.
    WaitMsg3,
    /// The pairwise key is installed; group handshakes are accepted. (A PTK
    /// rekey in progress also reports this.)
    Established,
}

/// Message 1 candidates kept at once (see [`Supplicant::input`]).
pub const MAX_PENDING: usize = 4;

#[derive(Clone, Debug)]
#[cfg_attr(any(test, feature = "fuzz"), derive(PartialEq))]
pub(crate) struct Pending {
    pub anonce: [u8; 32],
    pub snonce: [u8; 32],
    pub ptk: Key<PTK_LEN>,
    /// The highest replay counter seen in a message 1 for this ANonce.
    pub msg1_counter: u64,
}

#[derive(Clone, Debug)]
#[cfg_attr(any(test, feature = "fuzz"), derive(PartialEq))]
pub(crate) struct Installed {
    pub kck: Key<16>,
    pub kek: Key<16>,
    pub anonce: [u8; 32],
    /// The group key last delivered, to suppress reinstalling it.
    pub gtk: Option<(u8, Key<16>)>,
}

/// Everything that changes while the handshake runs. [`Supplicant::input`]
/// leaves it untouched on `Err`; tests compare it before and after.
#[derive(Clone, Debug, Default)]
#[cfg_attr(any(test, feature = "fuzz"), derive(PartialEq))]
pub struct State {
    pub(crate) pending: Vec<Pending>,
    pub(crate) installed: Option<Installed>,
    /// The highest replay counter of a MIC-verified frame.
    pub(crate) rx_replay: Option<u64>,
}

impl State {
    /// The highest replay counter of a MIC-verified frame so far.
    pub fn rx_replay(&self) -> Option<u64> {
        self.rx_replay
    }

    /// How many message 1 candidates wait for message 3 (at most
    /// [`MAX_PENDING`]).
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// A PTK derived from a message 1 is waiting for message 3.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// The WPA2-Personal supplicant for one association.
pub struct Supplicant<C: Crypto> {
    pub(crate) cfg: Config,
    pub(crate) crypto: C,
    pub(crate) state: State,
}

/// The three authenticator messages the supplicant answers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    PairwiseMsg1,
    PairwiseMsg3,
    GroupMsg1,
}

const MSG1: u16 = info::PAIRWISE | info::ACK;
const MSG3: u16 =
    info::PAIRWISE | info::INSTALL | info::ACK | info::MIC | info::SECURE | info::ENCRYPTED;
const GROUP1: u16 = info::ACK | info::MIC | info::SECURE | info::ENCRYPTED;

fn classify(flags: u16) -> Option<Kind> {
    match flags {
        MSG1 => Some(Kind::PairwiseMsg1),
        MSG3 => Some(Kind::PairwiseMsg3),
        GROUP1 => Some(Kind::GroupMsg1),
        _ => None,
    }
}

impl<C: Crypto> Supplicant<C> {
    /// Check `config` and start. Refused: an AKM other than 2 or 6, a cipher
    /// other than CCMP-128 (each with its own error naming it), an AP that
    /// does not offer the choice or requires management frame protection, and
    /// an association element that does not state exactly the choice.
    pub fn new(config: Config, crypto: C) -> Result<Supplicant<C>, Error> {
        if !matches!(config.akm, Akm::Psk | Akm::PskSha256) {
            return Err(Error::UnsupportedAkm(config.akm));
        }
        for cipher in [config.pairwise, config.group] {
            if cipher != Cipher::Ccmp128 {
                return Err(Error::UnsupportedCipher(cipher));
            }
        }
        let ap = Rsn::parse_ie(&config.ap_rsn_ie).map_err(|_| Error::NotOffered)?;
        let offered = ap.akms.contains(&config.akm)
            && ap.pairwise.contains(&config.pairwise)
            && ap.group == config.group;
        if !offered {
            return Err(Error::NotOffered);
        }
        if ap.caps.mfp_required() {
            return Err(Error::PmfRequired);
        }
        let assoc = Rsn::parse_ie(&config.assoc_rsn_ie).map_err(|_| Error::AssocIeMismatch)?;
        let exact = assoc.akms == [config.akm]
            && assoc.pairwise == [config.pairwise]
            && assoc.group == config.group;
        if !exact {
            return Err(Error::AssocIeMismatch);
        }
        Ok(Supplicant {
            cfg: config,
            crypto,
            state: State::default(),
        })
    }

    pub fn stage(&self) -> Stage {
        if self.state.installed.is_some() {
            Stage::Established
        } else if !self.state.pending.is_empty() {
            Stage::WaitMsg3
        } else {
            Stage::Start
        }
    }

    /// The mutable handshake state, for tests that compare it.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// The crypto, for tests that pin or inspect it.
    pub fn crypto(&self) -> &C {
        &self.crypto
    }

    /// The key descriptor version this AKM uses (12.7.2).
    pub(crate) fn expected_version(&self) -> u8 {
        match self.cfg.akm {
            Akm::PskSha256 => key::VERSION_AES_CMAC_AES,
            _ => key::VERSION_HMAC_SHA1_AES,
        }
    }

    /// Process one received EAPOL-Key PDU (from the EAPOL version octet).
    ///
    /// On `Ok`, the actions to perform, in order. On `Err` nothing changed:
    /// see [`Error::is_fatal`] for whether to drop the frame or deauthenticate.
    pub fn input(&mut self, pdu: &[u8]) -> Result<Vec<Action>, Error> {
        let frame = KeyFrame::parse(pdu)?;
        if frame.descriptor != key::DESCRIPTOR_RSN {
            return Err(Error::UnsupportedDescriptor(frame.descriptor));
        }
        if frame.version() != self.expected_version() {
            return Err(Error::BadVersion(frame.version()));
        }
        match classify(frame.flags()) {
            Some(Kind::PairwiseMsg1) => self.on_msg1(&frame),
            Some(Kind::PairwiseMsg3) => self.on_msg3(&frame),
            Some(Kind::GroupMsg1) => self.on_group_msg1(&frame),
            None => Err(Error::BadKeyInfo(frame.info)),
        }
    }

    /// Check the MIC of `frame` under `kck` in constant time.
    pub(crate) fn verify_mic(&self, frame: &KeyFrame, kck: &Key<16>) -> Result<(), Error> {
        let expected = self.crypto.mic(self.cfg.akm, kck, &frame.mic_input()?);
        if lazyos_crypto::wifi::mic_eq(&expected, &frame.mic) {
            Ok(())
        } else {
            Err(Error::BadMic)
        }
    }

    /// Build a reply to `to` with `flags`, sign it with `kck` and serialise.
    pub(crate) fn signed_reply(
        &self,
        to: &KeyFrame,
        flags: u16,
        nonce: [u8; 32],
        key_data: Vec<u8>,
        kck: &Key<16>,
    ) -> Result<Vec<u8>, Error> {
        let mut reply = KeyFrame::zeroed(to.eapol_version);
        reply.info = u16::from(self.expected_version()) | flags;
        reply.replay = to.replay;
        reply.nonce = nonce;
        reply.key_data = key_data;
        reply.mic = self.crypto.mic(self.cfg.akm, kck, &reply.encode()?);
        reply.encode()
    }
}
