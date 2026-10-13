//! The RFC 4330 client request and a validating reply decoder.
//!
//! The request carries a random 64-bit *nonce* in the transmit timestamp
//! field, as RFC 4330 section 5 recommends; a reply is accepted only when its
//! originate timestamp echoes it. That is the only defence an unauthenticated
//! exchange has against an off-path sender, so it is checked before anything
//! else in the reply is believed, including a kiss-o'-death code.

use core::num::NonZeroU64;

use crate::time::Timestamp;

/// A request or reply without extension fields.
pub const PACKET_LEN: usize = 48;

const VERSION: u8 = 4;
const MODE_CLIENT: u8 = 3;
const MODE_SERVER: u8 = 4;

/// The server's leap indicator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leap {
    None,
    AddSecond,
    DelSecond,
}

/// A kiss-o'-death code (stratum 0, ASCII reference id, RFC 4330 section 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kiss {
    /// `RATE`: ask less often.
    Rate,
    /// `DENY` or `RSTR`: this server will not serve us.
    Denied,
    /// Any other code.
    Other,
}

/// Why a reply was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// Shorter than 48 bytes.
    Short,
    /// A version other than 3 or 4.
    Version,
    /// Not a server-mode packet.
    Mode,
    /// The originate timestamp is not our nonce (an old, forged or foreign
    /// packet).
    Originate,
    /// The server says it will not answer (or not that often).
    Kiss(Kiss),
    /// Leap indicator 3: the server's clock is not synchronized.
    Unsynchronized,
    /// A stratum outside 1..=15.
    Stratum,
    /// A zero receive or transmit timestamp (the server never set them).
    ZeroTimestamp,
    /// The transmit timestamp precedes the receive timestamp.
    Order,
    /// A negative root delay.
    Malformed,
}

/// The fields of a reply the maths and the policy use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reply {
    pub leap: Leap,
    pub version: u8,
    pub stratum: u8,
    pub reference_id: [u8; 4],
    pub root_delay_ns: u64,
    pub root_dispersion_ns: u64,
    /// `t2`: when the server received the request.
    pub receive: Timestamp,
    /// `t3`: when the server sent the reply.
    pub transmit: Timestamp,
}

/// A client request: version 4, mode 3, everything zero but the nonce.
pub fn encode_request(nonce: NonZeroU64) -> [u8; PACKET_LEN] {
    let mut packet = [0u8; PACKET_LEN];
    packet[0] = (VERSION << 3) | MODE_CLIENT;
    packet[40..48].copy_from_slice(&nonce.get().to_be_bytes());
    packet
}

fn be32(packet: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([packet[at], packet[at + 1], packet[at + 2], packet[at + 3]])
}

fn timestamp(packet: &[u8], at: usize) -> Timestamp {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&packet[at..at + 8]);
    Timestamp::from_be_bytes(bytes)
}

/// 16.16 fixed point seconds to nanoseconds.
fn short_to_ns(value: u32) -> u64 {
    (u64::from(value) * 1_000_000_000) >> 16
}

/// Validate `packet` as the reply to the request that carried `nonce`.
/// Bytes past 48 (extension fields, a MAC) are ignored, never trusted.
pub fn decode_reply(packet: &[u8], nonce: NonZeroU64) -> Result<Reply, Reject> {
    if packet.len() < PACKET_LEN {
        return Err(Reject::Short);
    }
    let version = (packet[0] >> 3) & 7;
    if version != 3 && version != 4 {
        return Err(Reject::Version);
    }
    if packet[0] & 7 != MODE_SERVER {
        return Err(Reject::Mode);
    }
    if timestamp(packet, 24).0 != nonce.get() {
        return Err(Reject::Originate);
    }
    let stratum = packet[1];
    let reference_id = [packet[12], packet[13], packet[14], packet[15]];
    if stratum == 0 {
        return Err(Reject::Kiss(match &reference_id {
            b"RATE" => Kiss::Rate,
            b"DENY" | b"RSTR" => Kiss::Denied,
            _ => Kiss::Other,
        }));
    }
    let leap = match packet[0] >> 6 {
        0 => Leap::None,
        1 => Leap::AddSecond,
        2 => Leap::DelSecond,
        _ => return Err(Reject::Unsynchronized),
    };
    if stratum > 15 {
        return Err(Reject::Stratum);
    }
    let root_delay = be32(packet, 4);
    if root_delay & 0x8000_0000 != 0 {
        return Err(Reject::Malformed);
    }
    let receive = timestamp(packet, 32);
    let transmit = timestamp(packet, 40);
    if receive.is_zero() || transmit.is_zero() {
        return Err(Reject::ZeroTimestamp);
    }
    // Compared as a signed difference so a pair that straddles the era
    // rollover still orders correctly.
    if (transmit.0.wrapping_sub(receive.0) as i64) < 0 {
        return Err(Reject::Order);
    }
    Ok(Reply {
        leap,
        version,
        stratum,
        reference_id,
        root_delay_ns: short_to_ns(root_delay),
        root_dispersion_ns: short_to_ns(be32(packet, 8)),
        receive,
        transmit,
    })
}

#[cfg(test)]
pub(crate) mod build {
    //! A reply builder for the unit and seeded tests.

    use super::*;

    pub fn reply(nonce: u64, receive: Timestamp, transmit: Timestamp) -> [u8; PACKET_LEN] {
        let mut packet = [0u8; PACKET_LEN];
        packet[0] = (VERSION << 3) | MODE_SERVER;
        packet[1] = 2;
        packet[12..16].copy_from_slice(b"LAZY");
        packet[24..32].copy_from_slice(&nonce.to_be_bytes());
        packet[32..40].copy_from_slice(&receive.to_be_bytes());
        packet[40..48].copy_from_slice(&transmit.to_be_bytes());
        packet
    }
}

#[cfg(test)]
mod tests {
    use super::build::reply;
    use super::*;

    const NONCE: u64 = 0x0123_4567_89ab_cdef;

    fn nonce() -> NonZeroU64 {
        NonZeroU64::new(NONCE).unwrap()
    }

    fn good() -> [u8; PACKET_LEN] {
        reply(NONCE, Timestamp(10 << 32), Timestamp((10 << 32) + 5))
    }

    #[test]
    fn request_shape() {
        let packet = encode_request(nonce());
        assert_eq!(packet.len(), 48);
        assert_eq!(packet[0], 0x23, "LI 0, VN 4, mode 3");
        assert_eq!(&packet[40..48], &NONCE.to_be_bytes());
        assert!(packet[1..40].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_good_reply_decodes() {
        let r = decode_reply(&good(), nonce()).unwrap();
        assert_eq!(r.stratum, 2);
        assert_eq!(r.version, 4);
        assert_eq!(r.leap, Leap::None);
        assert_eq!(r.receive, Timestamp(10 << 32));
        assert_eq!(&r.reference_id, b"LAZY");
    }

    #[test]
    fn version_3_is_accepted_and_others_are_not() {
        for (v, ok) in [
            (0u8, false),
            (1, false),
            (2, false),
            (3, true),
            (4, true),
            (5, false),
            (7, false),
        ] {
            let mut p = good();
            p[0] = (p[0] & 0b1100_0111) | (v << 3);
            assert_eq!(decode_reply(&p, nonce()).is_ok(), ok, "version {v}");
        }
    }

    #[test]
    fn only_server_mode_is_accepted() {
        for mode in [0u8, 1, 2, 3, 5, 6, 7] {
            let mut p = good();
            p[0] = (p[0] & !7) | mode;
            assert_eq!(decode_reply(&p, nonce()), Err(Reject::Mode), "mode {mode}");
        }
    }

    #[test]
    fn the_nonce_must_be_echoed() {
        let mut p = good();
        p[31] ^= 1;
        assert_eq!(decode_reply(&p, nonce()), Err(Reject::Originate));
        assert_eq!(
            decode_reply(&good(), NonZeroU64::new(NONCE + 1).unwrap()),
            Err(Reject::Originate)
        );
    }

    #[test]
    fn kiss_codes_need_the_nonce_too() {
        let mut p = good();
        p[1] = 0;
        p[12..16].copy_from_slice(b"RATE");
        assert_eq!(decode_reply(&p, nonce()), Err(Reject::Kiss(Kiss::Rate)));
        p[12..16].copy_from_slice(b"DENY");
        assert_eq!(decode_reply(&p, nonce()), Err(Reject::Kiss(Kiss::Denied)));
        p[12..16].copy_from_slice(b"RSTR");
        assert_eq!(decode_reply(&p, nonce()), Err(Reject::Kiss(Kiss::Denied)));
        p[12..16].copy_from_slice(b"INIT");
        assert_eq!(decode_reply(&p, nonce()), Err(Reject::Kiss(Kiss::Other)));
        // A forged DENY from an off-path sender does not know the nonce.
        p[12..16].copy_from_slice(b"DENY");
        p[24] ^= 0xff;
        assert_eq!(decode_reply(&p, nonce()), Err(Reject::Originate));
    }

    #[test]
    fn alarm_and_bad_strata_are_refused() {
        let mut p = good();
        p[0] |= 0b1100_0000;
        assert_eq!(decode_reply(&p, nonce()), Err(Reject::Unsynchronized));
        let mut p = good();
        p[1] = 16;
        assert_eq!(decode_reply(&p, nonce()), Err(Reject::Stratum));
        p[1] = 255;
        assert_eq!(decode_reply(&p, nonce()), Err(Reject::Stratum));
        for s in 1..=15u8 {
            let mut p = good();
            p[1] = s;
            assert!(decode_reply(&p, nonce()).is_ok(), "stratum {s}");
        }
    }

    #[test]
    fn leap_indicators() {
        for (bits, leap) in [
            (0u8, Leap::None),
            (1, Leap::AddSecond),
            (2, Leap::DelSecond),
        ] {
            let mut p = good();
            p[0] |= bits << 6;
            assert_eq!(decode_reply(&p, nonce()).unwrap().leap, leap);
        }
    }

    #[test]
    fn timestamps_must_be_set_and_ordered() {
        let zero_rx = reply(NONCE, Timestamp::ZERO, Timestamp(5 << 32));
        assert_eq!(decode_reply(&zero_rx, nonce()), Err(Reject::ZeroTimestamp));
        let zero_tx = reply(NONCE, Timestamp(5 << 32), Timestamp::ZERO);
        assert_eq!(decode_reply(&zero_tx, nonce()), Err(Reject::ZeroTimestamp));
        let backwards = reply(NONCE, Timestamp(6 << 32), Timestamp(5 << 32));
        assert_eq!(decode_reply(&backwards, nonce()), Err(Reject::Order));
        // Across the era rollover the order is still forward.
        let wrap = reply(NONCE, Timestamp(u64::MAX - 10), Timestamp(10));
        assert!(decode_reply(&wrap, nonce()).is_ok());
    }

    #[test]
    fn negative_root_delay_is_malformed() {
        let mut p = good();
        p[4] = 0x80;
        assert_eq!(decode_reply(&p, nonce()), Err(Reject::Malformed));
    }

    #[test]
    fn root_values_are_16_16_seconds() {
        let mut p = good();
        p[4..8].copy_from_slice(&0x0001_8000u32.to_be_bytes()); // 1.5 s
        p[8..12].copy_from_slice(&0x0000_8000u32.to_be_bytes()); // 0.5 s
        let r = decode_reply(&p, nonce()).unwrap();
        assert_eq!(r.root_delay_ns, 1_500_000_000);
        assert_eq!(r.root_dispersion_ns, 500_000_000);
    }

    #[test]
    fn short_packets_and_trailing_bytes() {
        for len in 0..PACKET_LEN {
            assert_eq!(decode_reply(&good()[..len], nonce()), Err(Reject::Short));
        }
        let mut long = [0u8; 68];
        long[..48].copy_from_slice(&good());
        long[48..].fill(0xaa);
        assert_eq!(decode_reply(&long, nonce()).unwrap().stratum, 2);
    }
}
