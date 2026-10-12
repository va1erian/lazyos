//! Data frames (IEEE 802.11-2020 9.2.4, 9.3.2): the header, and the
//! conversion between an Ethernet frame and an 802.11 data frame that a
//! soft-MAC radio does in software (`libs/wifimac`).
//!
//! * [`DataHeader::parse`] reads the MAC header of a data frame: the three
//!   (or four) addresses, the sequence control, the QoS Control of a QoS
//!   subtype and the HT Control the Order bit adds.
//! * [`encap`] turns one Ethernet II frame into a data frame: a To-DS frame
//!   from a station or a From-DS frame from an access point, QoS or not, with
//!   room for a CCMP header the caller built ([`crate::ccmp`]), then the
//!   LLC/SNAP header (RFC 1042, or 802.1H bridge tunnel for AppleTalk ARP and
//!   IPX) and the payload.
//! * [`decap`] does the reverse for a received frame's body, an A-MSDU
//!   included, and refuses what a station must not pass up: a body that is not
//!   LLC/SNAP, an A-MSDU whose first subframe looks like an LLC header (the
//!   injection of "FragAttacks", CVE-2020-24588), a malformed subframe.
//!
//! The body handed to [`decap`] is the frame after its MAC header and
//! security header and before any MIC the radio kept: the caller strips those
//! (it knows whether the frame was protected). Nothing here allocates more
//! than the input's size, and nothing panics.

use alloc::vec::Vec;

use crate::{Error, Mac};

/// Frame control type values (bits 2-3 of the first octet).
pub const TYPE_MGMT: u8 = 0;
pub const TYPE_CONTROL: u8 = 1;
pub const TYPE_DATA: u8 = 2;

/// Data subtypes this library reads or builds.
pub const SUB_DATA: u8 = 0;
pub const SUB_NULL: u8 = 4;
pub const SUB_QOS_DATA: u8 = 8;
pub const SUB_QOS_NULL: u8 = 12;

/// Frame control flags (the second octet).
pub mod fc {
    pub const TO_DS: u8 = 0x01;
    pub const FROM_DS: u8 = 0x02;
    pub const MORE_FRAGMENTS: u8 = 0x04;
    pub const RETRY: u8 = 0x08;
    pub const POWER_MGMT: u8 = 0x10;
    pub const MORE_DATA: u8 = 0x20;
    pub const PROTECTED: u8 = 0x40;
    pub const ORDER: u8 = 0x80;
}

/// The Ethernet header: destination, source, EtherType.
pub const ETH_HEADER: usize = 14;
/// EAPOL's EtherType (IEEE 802.1X).
pub const ETHERTYPE_EAPOL: u16 = 0x888E;
/// The two EtherTypes 802.1H carries in a bridge-tunnel header.
pub const ETHERTYPE_AARP: u16 = 0x80F3;
pub const ETHERTYPE_IPX: u16 = 0x8137;
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_IPV6: u16 = 0x86DD;
const ETHERTYPE_VLAN: u16 = 0x8100;

/// LLC/SNAP header length: the six octets below plus the EtherType.
pub const LLC_SNAP_LEN: usize = 8;
/// RFC 1042 encapsulation: LLC AA AA 03, OUI 00 00 00.
pub const RFC1042: [u8; 6] = [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00];
/// 802.1H bridge tunnel: LLC AA AA 03, OUI 00 00 F8.
pub const BRIDGE_TUNNEL: [u8; 6] = [0xAA, 0xAA, 0x03, 0x00, 0x00, 0xF8];

/// QoS Control: the TID bits and the A-MSDU Present bit.
pub const QOS_TID_MASK: u16 = 0x000F;
pub const QOS_AMSDU_PRESENT: u16 = 1 << 7;

/// The largest MSDU an 802.11 frame carries (9.2.4.7.1, 2304 octets).
pub const MAX_MSDU: usize = 2304;
/// An A-MSDU subframe header: DA, SA, length (big endian).
pub const AMSDU_SUBHEADER: usize = 14;

/// A data frame's MAC header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataHeader {
    pub subtype: u8,
    /// Frame control flags octet ([`fc`]).
    pub flags: u8,
    pub duration: u16,
    pub addr1: Mac,
    pub addr2: Mac,
    pub addr3: Mac,
    /// Present only when both To DS and From DS are set (a mesh or WDS link).
    pub addr4: Option<Mac>,
    /// The raw sequence control field (sequence number and fragment number).
    pub sequence: u16,
    /// QoS Control of a QoS subtype.
    pub qos: Option<u16>,
    /// Octets of MAC header: 24, +6 for Address 4, +2 for QoS Control, +4 for
    /// HT Control (the Order bit on a QoS frame).
    pub len: usize,
}

fn le16(buf: &[u8], at: usize) -> Result<u16, Error> {
    match buf.get(at..at + 2) {
        Some(&[a, b]) => Ok(u16::from_le_bytes([a, b])),
        _ => Err(Error::Short),
    }
}

fn mac(buf: &[u8], at: usize) -> Result<Mac, Error> {
    buf.get(at..at + 6)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(Error::Short)
}

impl DataHeader {
    /// Parse the MAC header of a data frame. Refuses a version other than 0
    /// and any other frame type; the body is not looked at.
    pub fn parse(frame: &[u8]) -> Result<DataHeader, Error> {
        let fc0 = *frame.first().ok_or(Error::Short)?;
        if fc0 & 0x03 != 0 {
            return Err(Error::BadVersion);
        }
        if (fc0 >> 2) & 0x03 != TYPE_DATA {
            return Err(Error::NotData);
        }
        let flags = *frame.get(1).ok_or(Error::Short)?;
        let subtype = fc0 >> 4;
        let mut len = 24;
        let addr4 = if flags & (fc::TO_DS | fc::FROM_DS) == fc::TO_DS | fc::FROM_DS {
            len += 6;
            Some(mac(frame, 24)?)
        } else {
            None
        };
        // Subtypes 8..=15 carry QoS Control (9.2.4.1.3).
        let qos = if subtype & 0x08 != 0 {
            let qos = le16(frame, len)?;
            len += 2;
            if flags & fc::ORDER != 0 {
                len += 4;
            }
            Some(qos)
        } else {
            None
        };
        if frame.len() < len {
            return Err(Error::Short);
        }
        Ok(DataHeader {
            subtype,
            flags,
            duration: le16(frame, 2)?,
            addr1: mac(frame, 4)?,
            addr2: mac(frame, 10)?,
            addr3: mac(frame, 16)?,
            addr4,
            sequence: le16(frame, 22)?,
            qos,
            len,
        })
    }

    pub fn to_ds(&self) -> bool {
        self.flags & fc::TO_DS != 0
    }

    pub fn from_ds(&self) -> bool {
        self.flags & fc::FROM_DS != 0
    }

    pub fn protected(&self) -> bool {
        self.flags & fc::PROTECTED != 0
    }

    /// Null data and QoS null carry no MSDU (power management, keep-alive).
    pub fn is_null(&self) -> bool {
        self.subtype & 0x04 != 0
    }

    /// A fragment, or the first of several (the library does not reassemble).
    pub fn fragmented(&self) -> bool {
        self.flags & fc::MORE_FRAGMENTS != 0 || self.sequence & 0x000F != 0
    }

    /// The TID of a QoS frame.
    pub fn tid(&self) -> Option<u8> {
        self.qos.map(|qos| (qos & QOS_TID_MASK) as u8)
    }

    pub fn amsdu(&self) -> bool {
        self.qos.is_some_and(|qos| qos & QOS_AMSDU_PRESENT != 0)
    }

    /// The 12-bit sequence number.
    pub fn seq_number(&self) -> u16 {
        self.sequence >> 4
    }

    /// The BSSID: the receiver of a To-DS frame, the transmitter of a From-DS
    /// frame, Address 3 otherwise; `None` for a four-address frame.
    pub fn bssid(&self) -> Option<Mac> {
        match (self.to_ds(), self.from_ds()) {
            (true, false) => Some(self.addr1),
            (false, true) => Some(self.addr2),
            (false, false) => Some(self.addr3),
            (true, true) => None,
        }
    }

    /// The MSDU's destination address (9.3.2.1, table 9-30).
    pub fn destination(&self) -> Mac {
        if self.to_ds() {
            self.addr3
        } else {
            self.addr1
        }
    }

    /// The MSDU's source address.
    pub fn source(&self) -> Mac {
        match (self.to_ds(), self.from_ds()) {
            (_, false) => self.addr2,
            (false, true) => self.addr3,
            (true, true) => self.addr4.unwrap_or(self.addr2),
        }
    }
}

/// Which way a built frame travels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Station to access point: Address 1 is the BSSID, 2 the source, 3 the
    /// destination.
    ToDs,
    /// Access point to station: Address 1 is the destination, 2 the BSSID, 3
    /// the source.
    FromDs,
}

/// How [`encap`] builds the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encap {
    pub direction: Direction,
    pub bssid: Mac,
    /// Build a QoS data frame with this TID (0..=15), or plain data.
    pub tid: Option<u8>,
    /// The 12-bit sequence number.
    pub sequence: u16,
    /// A security header to place after the MAC header (a CCMP header from
    /// [`crate::ccmp::header`]); sets the Protected bit. The radio encrypts
    /// what follows it.
    pub security: Option<[u8; 8]>,
}

/// The EtherType of an Ethernet frame (`None` under 14 octets).
pub fn ethertype(eth: &[u8]) -> Option<u16> {
    match eth.get(12..14) {
        Some(&[a, b]) => Some(u16::from_be_bytes([a, b])),
        _ => None,
    }
}

/// Whether an Ethernet frame is EAPOL (the 4-way handshake travels as it).
pub fn is_eapol(eth: &[u8]) -> bool {
    ethertype(eth) == Some(ETHERTYPE_EAPOL)
}

/// The user priority (0..=7, the TID of a QoS frame) of an Ethernet frame:
/// the VLAN tag's priority, the precedence bits of an IPv4 TOS or IPv6
/// traffic class (as 802.11 maps DSCP to UP, table 10-1's rule of thumb),
/// voice (7) for EAPOL so the handshake is not queued behind bulk traffic,
/// best effort (0) otherwise.
pub fn user_priority(eth: &[u8]) -> u8 {
    let payload = eth.get(ETH_HEADER..).unwrap_or(&[]);
    match ethertype(eth) {
        Some(ETHERTYPE_EAPOL) => 7,
        Some(ETHERTYPE_VLAN) => payload.first().map_or(0, |b| b >> 5),
        Some(ETHERTYPE_IPV4) => payload.get(1).map_or(0, |tos| tos >> 5),
        Some(ETHERTYPE_IPV6) => match payload.get(..2) {
            Some(&[a, b]) => (((a & 0x0F) << 4) | (b >> 4)) >> 5,
            _ => 0,
        },
        _ => 0,
    }
}

/// Build a data frame carrying the Ethernet II frame `eth` (no FCS). Refused:
/// a frame under 14 octets ([`Error::Short`]), an 802.3 length field instead
/// of an EtherType, and an MSDU over [`MAX_MSDU`] ([`Error::BadEthernet`]).
pub fn encap(eth: &[u8], how: &Encap) -> Result<Vec<u8>, Error> {
    if eth.len() < ETH_HEADER {
        return Err(Error::Short);
    }
    let ethertype = ethertype(eth).ok_or(Error::Short)?;
    let payload = &eth[ETH_HEADER..];
    if ethertype < 0x0600 || payload.len() + LLC_SNAP_LEN > MAX_MSDU {
        return Err(Error::BadEthernet);
    }
    let dst: Mac = eth[0..6].try_into().map_err(|_| Error::Short)?;
    let src: Mac = eth[6..12].try_into().map_err(|_| Error::Short)?;
    let subtype = if how.tid.is_some() {
        SUB_QOS_DATA
    } else {
        SUB_DATA
    };
    let (flags, a1, a2, a3) = match how.direction {
        Direction::ToDs => (fc::TO_DS, how.bssid, src, dst),
        Direction::FromDs => (fc::FROM_DS, dst, how.bssid, src),
    };
    let protected = if how.security.is_some() {
        fc::PROTECTED
    } else {
        0
    };
    let mut out = Vec::with_capacity(26 + 8 + LLC_SNAP_LEN + payload.len());
    out.push((TYPE_DATA << 2) | (subtype << 4));
    out.push(flags | protected);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&a1);
    out.extend_from_slice(&a2);
    out.extend_from_slice(&a3);
    out.extend_from_slice(&((how.sequence & 0x0FFF) << 4).to_le_bytes());
    if let Some(tid) = how.tid {
        // Normal acknowledgement, no A-MSDU: only the TID is set.
        out.extend_from_slice(&u16::from(tid & 0x0F).to_le_bytes());
    }
    if let Some(security) = how.security {
        out.extend_from_slice(&security);
    }
    let llc = if ethertype == ETHERTYPE_AARP || ethertype == ETHERTYPE_IPX {
        BRIDGE_TUNNEL
    } else {
        RFC1042
    };
    out.extend_from_slice(&llc);
    out.extend_from_slice(&ethertype.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// One MSDU (LLC/SNAP header and payload) as an Ethernet II frame from `src`
/// to `dst`. An RFC 1042 header carrying a bridge-tunnel EtherType, or any
/// header that is neither RFC 1042 nor a bridge tunnel, is refused
/// ([`Error::BadLlc`]): a station has no use for 802.3 length frames.
pub fn msdu_to_ethernet(dst: &Mac, src: &Mac, msdu: &[u8]) -> Result<Vec<u8>, Error> {
    let llc = msdu.get(..6).ok_or(Error::Short)?;
    let ethertype = match msdu.get(6..8) {
        Some(&[a, b]) => u16::from_be_bytes([a, b]),
        _ => return Err(Error::Short),
    };
    let tunnelled = ethertype == ETHERTYPE_AARP || ethertype == ETHERTYPE_IPX;
    let ok = (llc == RFC1042 && !tunnelled) || llc == BRIDGE_TUNNEL;
    if !ok || ethertype < 0x0600 {
        return Err(Error::BadLlc);
    }
    let payload = &msdu[LLC_SNAP_LEN..];
    let mut out = Vec::with_capacity(ETH_HEADER + payload.len());
    out.extend_from_slice(dst);
    out.extend_from_slice(src);
    out.extend_from_slice(&ethertype.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// The Ethernet frames a received data frame's `body` carries: one MSDU, or
/// every subframe of an A-MSDU, appended to `out`. `body` is the frame after
/// its MAC header and security header and before any kept MIC. On an error
/// nothing is appended (a damaged A-MSDU is dropped whole).
pub fn decap(header: &DataHeader, body: &[u8], out: &mut Vec<Vec<u8>>) -> Result<(), Error> {
    if header.is_null() {
        return Ok(());
    }
    if header.fragmented() {
        return Err(Error::Fragmented);
    }
    if !header.amsdu() {
        if body.len() > MAX_MSDU {
            return Err(Error::BadEthernet);
        }
        let frame = msdu_to_ethernet(&header.destination(), &header.source(), body)?;
        out.push(frame);
        return Ok(());
    }
    let mut frames = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let sub = rest.get(..AMSDU_SUBHEADER).ok_or(Error::BadAmsdu)?;
        let dst: Mac = sub[0..6].try_into().map_err(|_| Error::BadAmsdu)?;
        let src: Mac = sub[6..12].try_into().map_err(|_| Error::BadAmsdu)?;
        // A first subframe whose DA is an LLC/SNAP header is what an attacker
        // gets by flipping the A-MSDU bit of a plain frame (CVE-2020-24588).
        if frames.is_empty() && dst == RFC1042 {
            return Err(Error::BadAmsdu);
        }
        let len = usize::from(u16::from_be_bytes([sub[12], sub[13]]));
        let msdu = rest
            .get(AMSDU_SUBHEADER..AMSDU_SUBHEADER + len)
            .ok_or(Error::BadAmsdu)?;
        if len > MAX_MSDU {
            return Err(Error::BadAmsdu);
        }
        frames.push(msdu_to_ethernet(&dst, &src, msdu)?);
        let used = AMSDU_SUBHEADER + len;
        rest = &rest[used..];
        if !rest.is_empty() {
            // Every subframe but the last is padded to a multiple of four.
            let pad = (4 - used % 4) % 4;
            rest = rest.get(pad..).ok_or(Error::BadAmsdu)?;
        }
    }
    out.extend(frames);
    Ok(())
}

/// Build an A-MSDU body from Ethernet II frames (the access point side, for
/// the simulator and the tests): each subframe is DA, SA, length, LLC/SNAP
/// and payload, padded to four octets except the last.
pub fn amsdu_body(frames: &[&[u8]]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    for (i, eth) in frames.iter().enumerate() {
        let ethertype = ethertype(eth).ok_or(Error::Short)?;
        if ethertype < 0x0600 {
            return Err(Error::BadEthernet);
        }
        let payload = &eth[ETH_HEADER..];
        let len = u16::try_from(LLC_SNAP_LEN + payload.len()).map_err(|_| Error::BadEthernet)?;
        out.extend_from_slice(&eth[0..12]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&RFC1042);
        out.extend_from_slice(&ethertype.to_be_bytes());
        out.extend_from_slice(payload);
        if i + 1 < frames.len() {
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }
    }
    Ok(out)
}
