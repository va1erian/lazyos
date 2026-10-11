//! Software CCMP-128 for the simulated radio: what a chip's crypto engine
//! does with the keys `SetKey` installed, in software.
//!
//! The soft-MAC (`libs/wifimac`) hands the radio a protected frame with its
//! CCMP header already in place and the plaintext after it; [`SoftCcmp::seal`]
//! encrypts that body and appends the MIC. On receive, [`SoftCcmp::open`]
//! verifies and decrypts in place and **keeps** the header, the CCMP header
//! and the MIC (the soft-MAC checks the packet number and strips the MIC as
//! a trailer), which is what the RTL8822CE's hardware decryption delivers.
//! The key is picked as a chip picks it: by the CCMP header's key id, from
//! the group keys when the receiver address is a group address, from the
//! pairwise key otherwise.

use alloc::vec::Vec;

use ieee80211::ccmp::{self, MIC_LEN};
use ieee80211::is_group;
use lazyos_crypto::wifi::ccm::{ccm_open, ccm_seal, TK_LEN};

/// Why a frame could not be sealed or opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CcmpError {
    /// The frame has no readable MAC or CCMP header.
    Malformed,
    /// No key is installed for that receiver and key id.
    NoKey,
    /// The MIC did not verify (wrong key, wrong PN or address, tampering).
    Mic,
}

/// What CCM needs for one frame: the key, the nonce, the AAD and where the
/// body starts (after the CCMP header).
struct Inputs {
    key: [u8; TK_LEN],
    nonce: [u8; 13],
    aad: Vec<u8>,
    start: usize,
}

/// The installed keys, by key id.
#[derive(Default)]
pub struct SoftCcmp {
    pairwise: Option<(u8, [u8; TK_LEN])>,
    group: [Option<[u8; TK_LEN]>; 4],
}

impl Drop for SoftCcmp {
    fn drop(&mut self) {
        self.clear();
    }
}

impl SoftCcmp {
    /// Install a pairwise (`group` false, id 0 or 1) or group (id 0-3; the
    /// soft-MAC only ever installs 1-3) key, replacing the one in that slot.
    /// `false` for an id or length that does not fit.
    pub fn install(&mut self, group: bool, index: u8, key: &[u8]) -> bool {
        let Ok(tk) = <[u8; TK_LEN]>::try_from(key) else {
            return false;
        };
        match (group, index) {
            (false, 0..=1) => self.pairwise = Some((index, tk)),
            (true, 0..=3) => self.group[usize::from(index)] = Some(tk),
            _ => return false,
        }
        true
    }

    /// Remove a key; nothing happens for an empty slot.
    pub fn remove(&mut self, group: bool, index: u8) {
        if group {
            if let Some(slot) = self.group.get_mut(usize::from(index)) {
                if let Some(key) = slot.as_mut() {
                    key.fill(0);
                }
                *slot = None;
            }
        } else if let Some((id, key)) = self.pairwise.as_mut() {
            if *id == index {
                key.fill(0);
                self.pairwise = None;
            }
        }
    }

    pub fn clear(&mut self) {
        for index in 0..=3 {
            self.remove(true, index);
        }
        for index in 0..=1 {
            self.remove(false, index);
        }
    }

    fn key_for(&self, receiver: &[u8; 6], key_id: u8) -> Option<&[u8; TK_LEN]> {
        if is_group(receiver) {
            self.group.get(usize::from(key_id))?.as_ref()
        } else {
            self.pairwise
                .as_ref()
                .filter(|(id, _)| *id == key_id)
                .map(|(_, key)| key)
        }
    }

    /// The CCM inputs of `frame`.
    fn inputs(&self, frame: &[u8]) -> Result<Inputs, CcmpError> {
        let framing = ccmp::framing(frame).map_err(|_| CcmpError::Malformed)?;
        let start = framing.header_len + ccmp::HEADER_LEN;
        let header = frame
            .get(framing.header_len..start)
            .ok_or(CcmpError::Malformed)?;
        let (pn, key_id) = ccmp::parse_header(header).map_err(|_| CcmpError::Malformed)?;
        let receiver: [u8; 6] = frame[4..10].try_into().map_err(|_| CcmpError::Malformed)?;
        let key = *self.key_for(&receiver, key_id).ok_or(CcmpError::NoKey)?;
        Ok(Inputs {
            key,
            nonce: framing.nonce(pn),
            aad: framing.aad,
            start,
        })
    }

    /// Encrypt the body after the CCMP header and append the MIC.
    pub fn seal(&self, frame: &[u8]) -> Result<Vec<u8>, CcmpError> {
        let i = self.inputs(frame)?;
        let mut out = Vec::from(frame);
        let mic =
            ccm_seal(&i.key, &i.nonce, &i.aad, &mut out[i.start..]).map_err(|_| CcmpError::Malformed)?;
        out.extend_from_slice(&mic);
        Ok(out)
    }

    /// Verify the MIC at the end and decrypt the body in place; the MIC
    /// stays (it is the radio's trailer).
    pub fn open(&self, frame: &mut [u8]) -> Result<(), CcmpError> {
        let i = self.inputs(frame)?;
        let end = frame
            .len()
            .checked_sub(MIC_LEN)
            .filter(|&end| end >= i.start)
            .ok_or(CcmpError::Malformed)?;
        let (body, mic) = frame.split_at_mut(end);
        let mic: [u8; MIC_LEN] = (&*mic).try_into().map_err(|_| CcmpError::Malformed)?;
        ccm_open(&i.key, &i.nonce, &i.aad, &mut body[i.start..], &mic).map_err(|_| CcmpError::Mic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        let text: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
        text.chunks(2)
            .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    const HEADER: &str = "08 48 c3 2c 0f d2 e1 28 a5 7c 50 30 f1 84 44 08 ab ae a5 b8 fc ba 80 33";
    const CCMP: &str = "0c e7 00 20 76 97 03 b5";
    const PLAIN: &str = "f8 ba 1a 55 d0 2f 85 ae 96 7b b6 2f b6 cd a8 eb 7e 78 a0 50";
    const SEALED: &str = "f3 d0 a2 fe 9a 3d bf 23 42 a6 43 e4 32 46 e8 0c 3c 04 d0 19 78 45 ce 0b 16 f9 76 23";

    /// The vector's Address 1 (`0f:d2:...`) is a group address, so its key
    /// (id 0) is looked up as a group key.
    fn annex_j() -> (SoftCcmp, Vec<u8>) {
        let mut keys = SoftCcmp::default();
        assert!(keys.install(true, 0, &hex("c97c1f67ce371185514a8a19f2bdd52f")));
        let mut frame = hex(HEADER);
        frame.extend(hex(CCMP));
        frame.extend(hex(PLAIN));
        (keys, frame)
    }

    /// IEEE 802.11-2020 Annex J.6.4: the whole encrypted MPDU (without FCS).
    #[test]
    fn annex_j_mpdu() {
        let (keys, frame) = annex_j();
        let sealed = keys.seal(&frame).unwrap();
        let mut expected = hex(HEADER);
        expected.extend(hex(CCMP));
        expected.extend(hex(SEALED));
        assert_eq!(sealed, expected);
        let mut opened = sealed.clone();
        keys.open(&mut opened).unwrap();
        assert_eq!(&opened[..frame.len()], &frame[..]);
        assert_eq!(&opened[frame.len()..], &sealed[frame.len()..], "MIC kept");
    }

    #[test]
    fn a_wrong_key_pn_or_address_fails_the_mic() {
        let (keys, frame) = annex_j();
        let sealed = keys.seal(&frame).unwrap();
        let mut pn = sealed.clone();
        pn[24] ^= 1;
        let mut addr2 = sealed.clone();
        addr2[10] ^= 1;
        let mut body = sealed.clone();
        body[40] ^= 1;
        for mut bad in [pn, addr2, body] {
            assert_eq!(keys.open(&mut bad), Err(CcmpError::Mic));
        }
        let mut other = SoftCcmp::default();
        other.install(true, 0, &[0; 16]);
        assert_eq!(other.open(&mut sealed.clone()), Err(CcmpError::Mic));
    }

    #[test]
    fn keys_by_receiver_and_id() {
        let (mut keys, frame) = annex_j();
        let mut key_one = frame.clone();
        key_one[27] = 0x60;
        assert_eq!(keys.seal(&key_one), Err(CcmpError::NoKey));
        // An individual receiver needs the pairwise key of that id.
        let mut unicast = frame.clone();
        unicast[4] &= !1;
        assert_eq!(keys.seal(&unicast), Err(CcmpError::NoKey));
        assert!(keys.install(false, 0, &[3; 16]));
        let sealed = keys.seal(&unicast).unwrap();
        keys.open(&mut sealed.clone()).unwrap();
        keys.remove(false, 0);
        assert_eq!(keys.open(&mut sealed.clone()), Err(CcmpError::NoKey));
        assert!(!keys.install(true, 4, &[3; 16]));
        assert!(!keys.install(false, 2, &[3; 16]));
        assert!(!keys.install(false, 0, &[3; 15]));
        keys.clear();
        assert_eq!(keys.seal(&frame), Err(CcmpError::NoKey));
        assert_eq!(keys.seal(&frame[..30]), Err(CcmpError::Malformed));
        assert_eq!(keys.open(&mut hex(HEADER)), Err(CcmpError::Malformed));
    }
}
