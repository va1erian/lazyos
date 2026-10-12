//! The simulated air of `wifisim` (docs/wifi-prerequisites-plan.md section
//! 3.6): how an 802.11 frame travels between the guest's simulated radio
//! and the host's fake access points (`tools/wifi/fake_ap.py`), and the
//! software CCMP the simulated radio does where a chip would use hardware.
//!
//! # Wire format
//!
//! Each 802.11 frame (from the MAC header, no FCS; with its MIC when
//! protected) rides in one Ethernet frame on a second virtio-net card:
//!
//! ```text
//! Ethernet:  dst ff:ff:ff:ff:ff:ff   src <sender's card>   type 0x88B5
//! Air header (8 octets):
//!   0  version      1
//!   1  band         0 = 2.4 GHz, 1 = 5 GHz (hw.v1 `Band` ordinals)
//!   2  channel      802.11 channel number within the band
//!   3  rssi         i8 dBm: the signal the receiver "hears" (the AP sets it;
//!                   the station sends 0)
//!   4  flags        bit 0: sent at a basic rate
//!   5  reserved     0
//!   6  length       u16 big endian: octets of 802.11 frame that follow
//! 802.11 frame (length octets)
//! ```
//!
//! 0x88B5 is IEEE 802's "Local Experimental EtherType 1", reserved for
//! exactly this kind of private protocol. A receiver hears a frame only on
//! the band and channel it is tuned to, which is how the fake APs model
//! channels. Everything about the format is checked on receipt: a frame
//! that is not version 1, whose length does not match, or whose band is
//! unknown is dropped by [`decode`].

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod ccmp;

use alloc::vec::Vec;

/// IEEE 802 Local Experimental EtherType 1.
pub const ETHERTYPE: u16 = 0x88B5;
pub const VERSION: u8 = 1;
/// The air header after the Ethernet header.
pub const HEADER_LEN: usize = 8;
/// The Ethernet header.
pub const ETH_HEADER: usize = 14;
/// `flags` bit: sent at a basic rate.
pub const FLAG_BASIC_RATE: u8 = 1;
/// The air card's Ethernet payload limit (a 1500-octet MTU).
pub const CARD_MTU: usize = 1500;
/// The largest 802.11 frame the air carries, MIC included.
pub const MAX_FRAME: usize = CARD_MTU - HEADER_LEN;

/// What the air header says about a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Air {
    pub band: u8,
    pub channel: u8,
    pub rssi_dbm: i8,
    pub flags: u8,
}

/// Why an Ethernet frame is not an air frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AirError {
    /// Another EtherType, or under the two headers.
    NotAir,
    /// A version this code does not speak.
    Version,
    /// The length field disagrees with the frame.
    Length,
    /// A band other than 2.4 or 5 GHz.
    Band,
    /// An 802.11 frame over [`MAX_FRAME`].
    TooLong,
}

/// The Ethernet frame carrying `frame` from the card `src`.
pub fn encode(src: &[u8; 6], air: &Air, frame: &[u8]) -> Result<Vec<u8>, AirError> {
    if frame.len() > MAX_FRAME {
        return Err(AirError::TooLong);
    }
    let mut out = Vec::with_capacity(ETH_HEADER + HEADER_LEN + frame.len());
    out.extend_from_slice(&[0xFF; 6]);
    out.extend_from_slice(src);
    out.extend_from_slice(&ETHERTYPE.to_be_bytes());
    out.extend_from_slice(&[
        VERSION,
        air.band,
        air.channel,
        air.rssi_dbm as u8,
        air.flags,
        0,
    ]);
    out.extend_from_slice(&(frame.len() as u16).to_be_bytes());
    out.extend_from_slice(frame);
    Ok(out)
}

/// The air header and the 802.11 frame of a received Ethernet frame.
/// Trailing octets past `length` (Ethernet padding of a short frame) are
/// ignored; fewer than `length` is an error.
pub fn decode(eth: &[u8]) -> Result<(Air, &[u8]), AirError> {
    let start = ETH_HEADER + HEADER_LEN;
    if eth.len() < start || eth[12..14] != ETHERTYPE.to_be_bytes() {
        return Err(AirError::NotAir);
    }
    let h = &eth[ETH_HEADER..start];
    if h[0] != VERSION {
        return Err(AirError::Version);
    }
    if h[1] > 1 {
        return Err(AirError::Band);
    }
    let len = usize::from(u16::from_be_bytes([h[6], h[7]]));
    if len > MAX_FRAME {
        return Err(AirError::TooLong);
    }
    let frame = eth.get(start..start + len).ok_or(AirError::Length)?;
    let air = Air {
        band: h[1],
        channel: h[2],
        rssi_dbm: h[3] as i8,
        flags: h[4],
    };
    Ok((air, frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: [u8; 6] = [0x02, 0x57, 0x53, 0, 0, 1];

    #[test]
    fn round_trip_and_layout() {
        let air = Air {
            band: 0,
            channel: 6,
            rssi_dbm: -42,
            flags: FLAG_BASIC_RATE,
        };
        let eth = encode(&CARD, &air, b"frame").unwrap();
        assert_eq!(&eth[..6], &[0xFF; 6]);
        assert_eq!(&eth[6..12], &CARD);
        assert_eq!(&eth[12..22], &[0x88, 0xB5, 1, 0, 6, 0xD6, 1, 0, 0, 5]);
        assert_eq!(decode(&eth), Ok((air, &b"frame"[..])));
        let mut padded = eth.clone();
        padded.extend_from_slice(&[0; 20]);
        assert_eq!(decode(&padded), Ok((air, &b"frame"[..])));
    }

    #[test]
    fn refusals() {
        let air = Air {
            band: 1,
            channel: 36,
            rssi_dbm: 0,
            flags: 0,
        };
        let eth = encode(&CARD, &air, b"abc").unwrap();
        assert_eq!(decode(&eth[..21]), Err(AirError::NotAir));
        let mut other = eth.clone();
        other[13] = 0;
        assert_eq!(decode(&other), Err(AirError::NotAir));
        let mut version = eth.clone();
        version[14] = 2;
        assert_eq!(decode(&version), Err(AirError::Version));
        let mut band = eth.clone();
        band[15] = 2;
        assert_eq!(decode(&band), Err(AirError::Band));
        assert_eq!(decode(&eth[..eth.len() - 1]), Err(AirError::Length));
        let mut huge = eth.clone();
        huge[20..22].copy_from_slice(&0xFFFFu16.to_be_bytes());
        assert_eq!(decode(&huge), Err(AirError::TooLong));
        assert_eq!(
            encode(&CARD, &air, &[0; MAX_FRAME + 1]),
            Err(AirError::TooLong)
        );
        assert!(encode(&CARD, &air, &[0; MAX_FRAME]).is_ok());
    }
}
