//! The record a scan keeps for one access point.

use alloc::vec::Vec;

use crate::frame::{Body, Mgmt};
use crate::ie::{ssid_is_hidden, Elements};
use crate::rsn::Rsn;
use crate::{cap, is_group, Mac};

/// What a BSS requires of a station, summarised from its elements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Security {
    /// No privacy bit, no RSN or WPA element.
    Open,
    /// The privacy bit without an RSN or WPA element: WEP, which is refused.
    Wep,
    /// Only the WPA1 vendor element (TKIP era), which is refused.
    Wpa1,
    /// A well-formed RSN element.
    Rsn(Rsn),
    /// An RSN element that failed to parse: never selected.
    InvalidRsn,
}

/// One access point as heard in a beacon or probe response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bss {
    pub bssid: Mac,
    /// SSID octets as sent; empty or all NULs when hidden. Not necessarily
    /// UTF-8: use [`Bss::ssid_lossy`] to show it.
    pub ssid: Vec<u8>,
    /// True when the last frame carried a hidden SSID and no name is known.
    pub hidden: bool,
    /// The channel the frame was received on, as the chip reported it; when
    /// the chip gave none (0), the DS Parameter Set channel.
    pub channel: u8,
    /// The channel the AP claims in its DS Parameter Set, if any.
    pub ds_channel: Option<u8>,
    /// Received signal strength in dBm, from the caller.
    pub rssi: i8,
    pub capability: u16,
    /// Beacon interval in time units (1024 us).
    pub interval: u16,
    pub security: Security,
    /// The whole RSN element as received, for the 4-way handshake's byte-for-
    /// byte comparison. Taken from the same frame as `security`.
    pub rsn_ie: Option<Vec<u8>>,
    pub country: Option<[u8; 2]>,
    /// HT, VHT or HE capabilities were advertised.
    pub ht: bool,
    pub vht: bool,
    pub he: bool,
    /// Rates (basic bit included), Supported then Extended.
    pub rates: Vec<u8>,
    /// Caller's clock, milliseconds, at the last frame.
    pub last_seen: u64,
    /// The last frame was a probe response (a hidden SSID is named by those).
    pub from_probe_response: bool,
}

impl Bss {
    /// Summarise a beacon or probe response heard on `rx_channel` (0 if the
    /// chip did not say) at `rssi` dBm and time `now_ms`.
    ///
    /// Returns `None` for any other frame, for a group-addressed or all-zero
    /// BSSID, for an IBSS, for an SSID over 32 octets, and for a frame that
    /// is not from its own BSSID (transmitter and BSSID must agree).
    pub fn from_frame(frame: &Mgmt<'_>, rx_channel: u8, rssi: i8, now_ms: u64) -> Option<Bss> {
        let (fixed, probe) = match frame.body {
            Body::Beacon(b) => (b, false),
            Body::ProbeResponse(b) => (b, true),
            _ => return None,
        };
        let bssid = frame.header.addr3;
        if is_group(&bssid) || bssid == [0; 6] || frame.header.addr2 != bssid {
            return None;
        }
        if fixed.capability & cap::ESS == 0 || fixed.capability & cap::IBSS != 0 {
            return None;
        }
        let elements = Elements::parse(fixed.ies).ok()?;
        let ssid = elements.ssid.unwrap_or_default();
        let mut rates = Vec::new();
        rates.extend_from_slice(elements.rates.unwrap_or_default());
        rates.extend_from_slice(elements.ext_rates.unwrap_or_default());
        Some(Bss {
            bssid,
            hidden: ssid_is_hidden(ssid),
            ssid: ssid.to_vec(),
            channel: if rx_channel != 0 {
                rx_channel
            } else {
                elements.ds_channel.unwrap_or(0)
            },
            ds_channel: elements.ds_channel,
            rssi,
            capability: fixed.capability,
            interval: fixed.interval,
            security: security_of(&elements, fixed.capability),
            rsn_ie: elements.rsn_ie.map(<[u8]>::to_vec),
            country: elements.country_code(),
            ht: elements.ht_cap.is_some(),
            vht: elements.vht_cap.is_some(),
            he: elements.he_cap.is_some(),
            rates,
            last_seen: now_ms,
            from_probe_response: probe,
        })
    }

    /// The SSID for display: invalid UTF-8 replaced, a hidden name empty.
    pub fn ssid_lossy(&self) -> alloc::string::String {
        if self.hidden {
            return alloc::string::String::new();
        }
        alloc::string::String::from_utf8_lossy(&self.ssid).into_owned()
    }

    /// True for a BSS the station could try with a pre-shared key: an RSN
    /// element that parsed.
    pub fn is_rsn(&self) -> bool {
        matches!(self.security, Security::Rsn(_))
    }
}

fn security_of(elements: &Elements<'_>, capability: u16) -> Security {
    if let Some(body) = elements.rsn {
        return match Rsn::parse_body(body) {
            Ok(rsn) => Security::Rsn(rsn),
            Err(_) => Security::InvalidRsn,
        };
    }
    if elements.wpa_ie.is_some() {
        Security::Wpa1
    } else if capability & cap::PRIVACY != 0 {
        Security::Wep
    } else {
        Security::Open
    }
}
