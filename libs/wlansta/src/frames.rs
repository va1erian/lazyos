//! The frames the station sends, and what it believes of the ones it gets.
//!
//! Everything received is hostile: a frame counts only when it parses, comes
//! from the joined BSS (transmitter and BSSID both equal to it) and is
//! addressed to this station (or, for a deauthentication or disassociation,
//! to everyone). Anything else is [`Rx::Ignored`] and changes nothing.

use alloc::vec::Vec;

use ieee80211::build;
use ieee80211::frame::{Body, Mgmt, AUTH_OPEN, STATUS_SUCCESS};
use ieee80211::ie::IeBuilder;
use ieee80211::{cap, Bss, BROADCAST};

use crate::select::{assoc_rsn, Join};
use crate::types::{Band, Mac};

/// EAPOL's ethertype.
pub const ETHERTYPE_EAPOL: u16 = 0x888E;
/// The PAE group address (IEEE 802.1X), which an AP may send EAPOL to.
pub const PAE_GROUP: Mac = [0x01, 0x80, 0xC2, 0x00, 0x00, 0x03];
/// Listen interval the station asks for, in beacon intervals.
const LISTEN_INTERVAL: u16 = 10;
/// Short preamble and short slot time: every 802.11g/n/ac radio has both.
const CAP_SHORT_PREAMBLE: u16 = 1 << 5;
const CAP_SHORT_SLOT: u16 = 1 << 10;

/// Rates (500 kb/s units, basic bit clear) when a BSS advertised none.
const RATES_2GHZ: &[u8] = &[2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108];
const RATES_5GHZ: &[u8] = &[12, 18, 24, 36, 48, 72, 96, 108];

/// An Open System authentication request (transaction 1).
pub fn auth_request(mac: &Mac, bssid: &Mac, seq: u16) -> Vec<u8> {
    build::auth(bssid, mac, bssid, seq, (AUTH_OPEN, 1, 0), &[])
}

/// An association request for `bss`: its SSID, the rates it advertised (the
/// chip runs every legacy rate), and for WPA2 the RSN element of
/// [`assoc_rsn`]. Returns the frame and that RSN element (whole), which the
/// handshake must see again in message 2.
pub fn assoc_request(
    mac: &Mac,
    bss: &Bss,
    band: Band,
    join: Join,
    seq: u16,
) -> (Vec<u8>, Option<Vec<u8>>) {
    let rates: &[u8] = if !bss.rates.is_empty() {
        &bss.rates
    } else if band == Band::Ghz2 {
        RATES_2GHZ
    } else {
        RATES_5GHZ
    };
    // The SSID is at most 32 octets (the scan refused longer ones) and the
    // rates at most 255 each way, so the builder cannot fail; an empty list
    // would only make the AP refuse us.
    let mut ies = IeBuilder::new();
    if let Ok(next) = ies.clone().ssid(&bss.ssid) {
        ies = next;
    }
    if let Ok(next) = ies.clone().rates(&rates[..rates.len().min(255)]) {
        ies = next;
    }
    let mut rsn_ie = None;
    if let Join::Psk(akm) = join {
        let rsn = assoc_rsn(akm);
        if let (Ok(next), Ok(ie)) = (ies.clone().rsn(&rsn), rsn.to_ie()) {
            ies = next;
            rsn_ie = Some(ie);
        }
    }
    let mut capability = cap::ESS | CAP_SHORT_PREAMBLE | (bss.capability & cap::PRIVACY);
    if band == Band::Ghz2 {
        capability |= CAP_SHORT_SLOT;
    }
    let frame = build::assoc_request(mac, &bss.bssid, seq, capability, LISTEN_INTERVAL, &ies.finish());
    (frame, rsn_ie)
}

/// An EAPOL PDU as the 802.3 frame `TxMgmt(Eapol)` carries.
pub fn eapol_frame(to: &Mac, from: &Mac, pdu: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + pdu.len());
    frame.extend_from_slice(to);
    frame.extend_from_slice(from);
    frame.extend_from_slice(&ETHERTYPE_EAPOL.to_be_bytes());
    frame.extend_from_slice(pdu);
    frame
}

/// The EAPOL PDU of an 802.3 frame from `bssid` to `mac` (or the PAE group
/// address), or `None`.
pub fn eapol_pdu<'a>(frame: &'a [u8], mac: &Mac, bssid: &Mac) -> Option<&'a [u8]> {
    let (header, pdu) = (frame.get(..14)?, frame.get(14..)?);
    let to_us = header[..6] == mac[..] || header[..6] == PAE_GROUP[..];
    let from_ap = header[6..12] == bssid[..];
    let eapol = header[12..14] == ETHERTYPE_EAPOL.to_be_bytes();
    (to_us && from_ap && eapol).then_some(pdu)
}

/// What a received management frame means to the join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rx {
    /// An Open System authentication response with this status.
    AuthResponse { status: u16 },
    /// An association response with this status and association id.
    AssocResponse { status: u16, aid: u16 },
    /// A deauthentication or disassociation with this reason.
    Ended { reason: u16 },
    /// Nothing the station acts on.
    Ignored,
}

/// Judge one management frame against the joined BSS.
pub fn classify(frame: &[u8], mac: &Mac, bssid: &Mac) -> Rx {
    let Ok(mgmt) = Mgmt::parse(frame) else {
        return Rx::Ignored;
    };
    let h = mgmt.header;
    if h.addr2 != *bssid || h.addr3 != *bssid {
        return Rx::Ignored;
    }
    let to_us = h.addr1 == *mac;
    match mgmt.body {
        Body::Auth(auth) if to_us && auth.algorithm == AUTH_OPEN && auth.transaction == 2 => {
            Rx::AuthResponse {
                status: auth.status,
            }
        }
        Body::AssocResponse(resp) if to_us => Rx::AssocResponse {
            status: resp.status,
            aid: resp.aid(),
        },
        Body::Deauth { reason } | Body::Disassoc { reason } if to_us || h.addr1 == BROADCAST => {
            Rx::Ended { reason }
        }
        _ => Rx::Ignored,
    }
}

/// Whether an authentication or association status is success.
pub fn success(status: u16) -> bool {
    status == STATUS_SUCCESS
}
