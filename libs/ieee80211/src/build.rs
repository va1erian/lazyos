//! Management frame builders: the frames a station sends (probe request,
//! authentication, association request, deauthentication, disassociation) and
//! the ones an access point sends (beacon, probe response, authentication,
//! association response), which the simulator and the tests use.
//!
//! Each returns the frame from the MAC header, without FCS. `ies` is an
//! element list already built with [`crate::ie::IeBuilder`]; it is not
//! validated here (a test may want a hostile one).

use alloc::vec::Vec;

use crate::frame::*;
use crate::Mac;

/// The 24-octet management header: version 0, type management, no flags.
pub fn header(subtype: u8, da: &Mac, sa: &Mac, bssid: &Mac, sequence: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + 32);
    out.push(subtype << 4);
    out.push(0);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(da);
    out.extend_from_slice(sa);
    out.extend_from_slice(bssid);
    // The sequence number occupies bits 4..16; the fragment number is 0.
    out.extend_from_slice(&(sequence << 4).to_le_bytes());
    out
}

fn with(mut out: Vec<u8>, fixed: &[u16], ies: &[u8]) -> Vec<u8> {
    for field in fixed {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out.extend_from_slice(ies);
    out
}

/// Fields common to beacons and probe responses.
#[derive(Clone, Copy, Debug)]
pub struct BeaconFields {
    pub timestamp: u64,
    pub interval: u16,
    pub capability: u16,
}

fn beacon_like(
    subtype: u8,
    da: &Mac,
    bssid: &Mac,
    seq: u16,
    f: BeaconFields,
    ies: &[u8],
) -> Vec<u8> {
    let mut out = header(subtype, da, bssid, bssid, seq);
    out.extend_from_slice(&f.timestamp.to_le_bytes());
    with(out, &[f.interval, f.capability], ies)
}

/// A beacon from `bssid` (broadcast destination).
pub fn beacon(bssid: &Mac, seq: u16, fields: BeaconFields, ies: &[u8]) -> Vec<u8> {
    beacon_like(SUB_BEACON, &crate::BROADCAST, bssid, seq, fields, ies)
}

/// A probe response from `bssid` to `da`.
pub fn probe_response(
    bssid: &Mac,
    da: &Mac,
    seq: u16,
    fields: BeaconFields,
    ies: &[u8],
) -> Vec<u8> {
    beacon_like(SUB_PROBE_RESP, da, bssid, seq, fields, ies)
}

/// A probe request from `sa`, to the wildcard BSSID.
pub fn probe_request(sa: &Mac, seq: u16, ies: &[u8]) -> Vec<u8> {
    let mut out = header(SUB_PROBE_REQ, &crate::BROADCAST, sa, &crate::BROADCAST, seq);
    out.extend_from_slice(ies);
    out
}

/// An authentication frame (`from` to `to`, in `bssid`).
pub fn auth(
    to: &Mac,
    from: &Mac,
    bssid: &Mac,
    seq: u16,
    body: (u16, u16, u16),
    ies: &[u8],
) -> Vec<u8> {
    let (algorithm, transaction, status) = body;
    with(
        header(SUB_AUTH, to, from, bssid, seq),
        &[algorithm, transaction, status],
        ies,
    )
}

/// An association request from `sa` to the AP `bssid`.
pub fn assoc_request(
    sa: &Mac,
    bssid: &Mac,
    seq: u16,
    capability: u16,
    listen: u16,
    ies: &[u8],
) -> Vec<u8> {
    with(
        header(SUB_ASSOC_REQ, bssid, sa, bssid, seq),
        &[capability, listen],
        ies,
    )
}

/// An association response from `bssid` to `da`. `aid` gets the two top bits
/// set, as the standard requires of the AID field.
pub fn assoc_response(
    bssid: &Mac,
    da: &Mac,
    seq: u16,
    capability: u16,
    status: u16,
    aid: u16,
    ies: &[u8],
) -> Vec<u8> {
    with(
        header(SUB_ASSOC_RESP, da, bssid, bssid, seq),
        &[capability, status, (aid & 0x3FFF) | 0xC000],
        ies,
    )
}

/// A deauthentication frame carrying `reason`.
pub fn deauth(to: &Mac, from: &Mac, bssid: &Mac, seq: u16, reason: u16) -> Vec<u8> {
    with(header(SUB_DEAUTH, to, from, bssid, seq), &[reason], &[])
}

/// A disassociation frame carrying `reason`.
pub fn disassoc(to: &Mac, from: &Mac, bssid: &Mac, seq: u16, reason: u16) -> Vec<u8> {
    with(header(SUB_DISASSOC, to, from, bssid, seq), &[reason], &[])
}

/// An action frame with `category` and an opaque `payload`.
pub fn action(
    to: &Mac,
    from: &Mac,
    bssid: &Mac,
    seq: u16,
    category: u8,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = header(SUB_ACTION, to, from, bssid, seq);
    out.push(category);
    out.extend_from_slice(payload);
    out
}
