//! EAPOL-Key frames (IEEE 802.11-2020 12.7.2).
//!
//! The PDU starts at the EAPOL protocol version octet (the caller strips the
//! Ethernet header and ethertype 0x888E):
//!
//! ```text
//!  0  version        1  type (3 = EAPOL-Key)   2  body length (BE)
//!  4  descriptor     5  key information (BE)   7  key length (BE)
//!  9  replay counter (8)                      17  key nonce (32)
//! 49  key IV (16)                             65  key RSC (8)
//! 73  key ID / reserved (8)                   81  key MIC (16)
//! 97  key data length (BE)                    99  key data
//! ```
//!
//! Only 16-octet MICs are supported (AKM 2 and 6), which is what the fixed
//! offsets above assume. The body length must equal 95 plus the key data
//! length; octets after the PDU (Ethernet padding) are ignored.

use alloc::vec::Vec;

use crate::Error;

/// EAPOL packet type: Key.
pub const TYPE_KEY: u8 = 3;
/// Key descriptor type for RSN (WPA2); 254 is the WPA1 descriptor.
pub const DESCRIPTOR_RSN: u8 = 2;
pub const DESCRIPTOR_WPA1: u8 = 254;
/// Octets before the key data.
pub const FIXED_LEN: usize = 99;
pub const MIC_LEN: usize = 16;
const MIC_AT: usize = 81;
/// The body length counts everything after the 4-octet EAPOL header.
const HEADER_LEN: usize = 4;

/// Key information bits (12.7.2, Figure 12-33).
pub mod info {
    /// Key descriptor version, bits 0-2.
    pub const VERSION_MASK: u16 = 0x0007;
    /// Pairwise key (1) or group key (0).
    pub const PAIRWISE: u16 = 1 << 3;
    pub const INSTALL: u16 = 1 << 6;
    pub const ACK: u16 = 1 << 7;
    pub const MIC: u16 = 1 << 8;
    pub const SECURE: u16 = 1 << 9;
    pub const ERROR: u16 = 1 << 10;
    pub const REQUEST: u16 = 1 << 11;
    pub const ENCRYPTED: u16 = 1 << 12;
    pub const SMK: u16 = 1 << 13;
    /// Every flag whose value matters to the handshake; the version (its own
    /// check) and the reserved key index and top bits are left out.
    pub const FLAGS: u16 =
        PAIRWISE | INSTALL | ACK | MIC | SECURE | ERROR | REQUEST | ENCRYPTED | SMK;
}

/// The key descriptor version for an AKM: 2 (HMAC-SHA1-128 and AES key wrap)
/// for AKM 2, 3 (AES-128-CMAC and AES key wrap) for AKM 6.
pub const VERSION_HMAC_SHA1_AES: u8 = 2;
pub const VERSION_AES_CMAC_AES: u8 = 3;

/// A parsed EAPOL-Key frame.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyFrame {
    /// The EAPOL protocol version octet, echoed in replies.
    pub eapol_version: u8,
    pub descriptor: u8,
    pub info: u16,
    pub key_length: u16,
    pub replay: [u8; 8],
    pub nonce: [u8; 32],
    pub iv: [u8; 16],
    pub rsc: [u8; 8],
    pub key_id: [u8; 8],
    pub mic: [u8; MIC_LEN],
    pub key_data: Vec<u8>,
}

impl core::fmt::Debug for KeyFrame {
    // The nonce, MIC and key data are not secret on the air, but a log that
    // prints key data after decryption would be; keep the Debug short.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KeyFrame")
            .field("info", &format_args!("{:#06x}", self.info))
            .field("replay", &self.replay_counter())
            .field("key_data_len", &self.key_data.len())
            .finish_non_exhaustive()
    }
}

impl KeyFrame {
    /// A frame with every field zero, for the builders to fill in.
    pub fn zeroed(eapol_version: u8) -> KeyFrame {
        KeyFrame {
            eapol_version,
            descriptor: DESCRIPTOR_RSN,
            info: 0,
            key_length: 0,
            replay: [0; 8],
            nonce: [0; 32],
            iv: [0; 16],
            rsc: [0; 8],
            key_id: [0; 8],
            mic: [0; MIC_LEN],
            key_data: Vec::new(),
        }
    }

    pub fn replay_counter(&self) -> u64 {
        u64::from_be_bytes(self.replay)
    }

    pub fn version(&self) -> u8 {
        (self.info & info::VERSION_MASK) as u8
    }

    /// The flags that matter, compared with [`info::FLAGS`]'s mask.
    pub fn flags(&self) -> u16 {
        self.info & info::FLAGS
    }

    /// Parse an EAPOL-Key PDU. Errors: [`Error::Short`] (header or body cut
    /// short), [`Error::NotKey`] (an EAPOL packet that is not a Key frame),
    /// [`Error::BadLength`] (body length disagrees with the key data length).
    /// The descriptor type is *not* judged here.
    pub fn parse(pdu: &[u8]) -> Result<KeyFrame, Error> {
        let (&eapol_version, &packet_type) = match pdu {
            [v, t, ..] => (v, t),
            _ => return Err(Error::Short),
        };
        if packet_type != TYPE_KEY {
            return Err(Error::NotKey);
        }
        let body_len = usize::from(be16(pdu, 2)?);
        let total = HEADER_LEN + body_len;
        if pdu.len() < total || total < FIXED_LEN {
            return Err(Error::Short);
        }
        let key_data_len = usize::from(be16(pdu, 97)?);
        if total != FIXED_LEN + key_data_len {
            return Err(Error::BadLength);
        }
        Ok(KeyFrame {
            eapol_version,
            descriptor: pdu[4],
            info: be16(pdu, 5)?,
            key_length: be16(pdu, 7)?,
            replay: array(pdu, 9),
            nonce: array(pdu, 17),
            iv: array(pdu, 49),
            rsc: array(pdu, 65),
            key_id: array(pdu, 73),
            mic: array(pdu, MIC_AT),
            key_data: pdu[FIXED_LEN..total].to_vec(),
        })
    }

    /// Serialise. A key data longer than 65535 octets cannot be framed and
    /// yields [`Error::BadLength`].
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let key_data_len = u16::try_from(self.key_data.len()).map_err(|_| Error::BadLength)?;
        let body_len = u16::try_from(FIXED_LEN - HEADER_LEN + self.key_data.len())
            .map_err(|_| Error::BadLength)?;
        let mut out = Vec::with_capacity(FIXED_LEN + self.key_data.len());
        out.extend_from_slice(&[self.eapol_version, TYPE_KEY]);
        out.extend_from_slice(&body_len.to_be_bytes());
        out.push(self.descriptor);
        out.extend_from_slice(&self.info.to_be_bytes());
        out.extend_from_slice(&self.key_length.to_be_bytes());
        out.extend_from_slice(&self.replay);
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.iv);
        out.extend_from_slice(&self.rsc);
        out.extend_from_slice(&self.key_id);
        out.extend_from_slice(&self.mic);
        out.extend_from_slice(&key_data_len.to_be_bytes());
        out.extend_from_slice(&self.key_data);
        Ok(out)
    }

    /// The octets the MIC covers: the whole PDU with the MIC field zeroed.
    pub fn mic_input(&self) -> Result<Vec<u8>, Error> {
        let mut zeroed = self.clone();
        zeroed.mic = [0; MIC_LEN];
        zeroed.encode()
    }
}

fn be16(buf: &[u8], at: usize) -> Result<u16, Error> {
    match buf.get(at..at + 2) {
        Some(&[a, b]) => Ok(u16::from_be_bytes([a, b])),
        _ => Err(Error::Short),
    }
}

/// `N` octets at `at`; the caller has checked the length.
fn array<const N: usize>(buf: &[u8], at: usize) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(&buf[at..at + N]);
    out
}
