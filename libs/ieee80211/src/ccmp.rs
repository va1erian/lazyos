//! CCMP framing (IEEE 802.11-2020 12.5.3): the 8-octet CCMP header with its
//! 48-bit packet number (PN), the CCM nonce and additional authentication
//! data (AAD) built from a frame's MAC header, and the receiver's replay
//! window.
//!
//! The cipher itself (AES-CCM, M = 8, L = 2) is not here: a chip runs it in
//! hardware, and the simulator calls `lazyos_crypto::wifi::ccm_seal`/`ccm_open`
//! with what this module builds. What the chip of this machine does *not* do
//! (the RTL8822CE, docs/wifi-rtw88-plan.md section 1) is choose the PN or
//! check it on receive, so both are software's job: [`header`] for transmit,
//! [`Replay`] for receive.

use alloc::vec::Vec;

use crate::data::{fc, DataHeader, TYPE_DATA, TYPE_MGMT};
use crate::{Error, Mac};

/// The CCMP header: PN0, PN1, reserved, key id octet, PN2..PN5.
pub const HEADER_LEN: usize = 8;
/// The CCMP-128 MIC.
pub const MIC_LEN: usize = 8;
/// The CCM nonce: flags, Address 2, PN.
pub const NONCE_LEN: usize = 13;
/// The largest packet number (48 bits).
pub const PN_MAX: u64 = (1 << 48) - 1;
/// The Extended IV bit of the key id octet: always set for CCMP.
pub const EXT_IV: u8 = 0x20;

/// The CCMP header for `pn` (masked to 48 bits) and key id `key_id` (0..=3).
pub fn header(pn: u64, key_id: u8) -> [u8; HEADER_LEN] {
    let pn = pn.to_le_bytes();
    [
        pn[0],
        pn[1],
        0,
        EXT_IV | ((key_id & 0x03) << 6),
        pn[2],
        pn[3],
        pn[4],
        pn[5],
    ]
}

/// The PN and key id of a CCMP header. Refused without the Extended IV bit
/// (a WEP or TKIP header) or under 8 octets ([`Error::BadCcmp`]).
pub fn parse_header(bytes: &[u8]) -> Result<(u64, u8), Error> {
    let h = bytes.get(..HEADER_LEN).ok_or(Error::BadCcmp)?;
    if h[3] & EXT_IV == 0 {
        return Err(Error::BadCcmp);
    }
    let pn = u64::from_le_bytes([h[0], h[1], h[4], h[5], h[6], h[7], 0, 0]);
    Ok((pn, h[3] >> 6))
}

/// What a frame's MAC header contributes to CCM: the AAD, the nonce's flags
/// octet and Address 2, and where the header ends (the CCMP header follows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Framing {
    pub aad: Vec<u8>,
    pub nonce_flags: u8,
    pub addr2: Mac,
    pub header_len: usize,
}

impl Framing {
    /// The CCM nonce for this frame and `pn`: flags, Address 2, then the PN
    /// most significant octet first (12.5.3.3.4).
    pub fn nonce(&self, pn: u64) -> [u8; NONCE_LEN] {
        let mut nonce = [0u8; NONCE_LEN];
        nonce[0] = self.nonce_flags;
        nonce[1..7].copy_from_slice(&self.addr2);
        nonce[7..].copy_from_slice(&pn.to_be_bytes()[2..]);
        nonce
    }
}

/// Build the AAD and nonce inputs of a protected data or management frame
/// (12.5.3.3.3): Frame Control with Subtype bits 4-6 of a data frame, Retry,
/// Power Management and More Data masked to 0, Protected set, and Order
/// masked on a QoS frame; Addresses 1-3; Sequence Control with only the
/// fragment number; Address 4 when present; QoS Control with only the TID.
/// The HT Control field never enters the AAD. The nonce's priority is the TID
/// (0 for a non-QoS frame), and bit 4 marks a management frame.
pub fn framing(frame: &[u8]) -> Result<Framing, Error> {
    let fc0 = *frame.first().ok_or(Error::Short)?;
    let ftype = (fc0 >> 2) & 0x03;
    let (header_len, addr4, qos) = match ftype {
        TYPE_DATA => {
            let h = DataHeader::parse(frame)?;
            (h.len, h.addr4, h.qos)
        }
        TYPE_MGMT if frame.len() >= 24 => (24, None, None),
        TYPE_MGMT => return Err(Error::Short),
        _ => return Err(Error::NotData),
    };
    let flags = frame[1];
    let mut fc0_masked = fc0;
    if ftype == TYPE_DATA {
        fc0_masked &= !0x70;
    }
    let mut flags_masked = (flags & !(fc::RETRY | fc::POWER_MGMT | fc::MORE_DATA)) | fc::PROTECTED;
    if qos.is_some() {
        flags_masked &= !fc::ORDER;
    }
    let mut aad = Vec::with_capacity(30);
    aad.push(fc0_masked);
    aad.push(flags_masked);
    aad.extend_from_slice(&frame[4..22]);
    // Sequence Control: the sequence number masked, the fragment number kept.
    aad.extend_from_slice(&[frame[22] & 0x0F, 0]);
    if let Some(addr4) = addr4 {
        aad.extend_from_slice(&addr4);
    }
    let tid = qos.map_or(0, |qos| (qos & 0x000F) as u8);
    if qos.is_some() {
        aad.extend_from_slice(&[tid, 0]);
    }
    let management = if ftype == TYPE_MGMT { 0x10 } else { 0 };
    Ok(Framing {
        aad,
        nonce_flags: tid | management,
        addr2: frame[10..16].try_into().map_err(|_| Error::Short)?,
        header_len,
    })
}

/// Replay counters slots: one per TID of QoS data, one for everything else.
pub const REPLAY_SLOTS: usize = 17;

/// A receiver's replay window for one key (12.5.3.4.4): a separate counter
/// per traffic identifier of QoS data and one for non-QoS data (and robust
/// management frames). A frame is accepted only when its PN is greater than
/// the last accepted PN of its slot; every slot starts at the receive
/// sequence counter the handshake delivered for the key, so the first frame
/// must exceed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Replay {
    last: [u64; REPLAY_SLOTS],
}

impl Replay {
    /// A window for a key installed with receive sequence counter `rsc`.
    pub fn new(rsc: u64) -> Replay {
        Replay {
            last: [rsc & PN_MAX; REPLAY_SLOTS],
        }
    }

    /// The slot of a frame with this TID (`None` for non-QoS).
    pub fn slot(tid: Option<u8>) -> usize {
        match tid {
            Some(tid) => usize::from(tid & 0x0F),
            None => REPLAY_SLOTS - 1,
        }
    }

    /// Whether `pn` would be accepted in `tid`'s slot (nothing is recorded).
    pub fn fresh(&self, tid: Option<u8>, pn: u64) -> bool {
        pn <= PN_MAX && pn > self.last[Self::slot(tid)]
    }

    /// Accept `pn` (record it and return true) or refuse it as a replay.
    /// Call it only for a frame whose MIC verified: an unauthenticated PN
    /// must never advance the window.
    pub fn accept(&mut self, tid: Option<u8>, pn: u64) -> bool {
        if !self.fresh(tid, pn) {
            return false;
        }
        self.last[Self::slot(tid)] = pn;
        true
    }

    /// The last accepted PN of a slot.
    pub fn last(&self, tid: Option<u8>) -> u64 {
        self.last[Self::slot(tid)]
    }
}

/// Little-endian receive sequence counter octets (`SetKey`'s `rsc`, up to 8
/// octets) as a PN; longer input is refused.
pub fn rsc_from_bytes(rsc: &[u8]) -> Result<u64, Error> {
    if rsc.len() > 8 {
        return Err(Error::BadCcmp);
    }
    let mut bytes = [0u8; 8];
    bytes[..rsc.len()].copy_from_slice(rsc);
    Ok(u64::from_le_bytes(bytes) & PN_MAX)
}
