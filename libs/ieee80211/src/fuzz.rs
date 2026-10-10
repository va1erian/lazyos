//! Fuzz entry point, shared by the seeded tests below and the cargo-fuzz
//! target (`fuzz/fuzz_targets/ieee80211.rs`), so a crash found by one replays
//! under the other.
//!
//! [`run`] takes any bytes and uses them three ways:
//!
//! * as a management frame (header included): parse it, read its elements,
//!   summarise it as a [`Bss`], feed that to a table, and rebuild a frame of
//!   the same shape that must parse back to the same fields;
//! * as an RSN element body and as a whole RSN element: whatever parses must
//!   build and parse back to an equal value;
//! * as a script against a [`ScanTable`]: the capacity bound and BSSID
//!   uniqueness hold after every step.
//!
//! Nothing may panic, and every slice a parse hands back lies inside the input.

use std::vec::Vec;

use crate::bss::{Bss, Security};
use crate::build::{self, BeaconFields};
use crate::frame::{Body, Mgmt};
use crate::ie::{ElementIter, Elements, MAX_SSID, MAX_VENDOR};
use crate::rsn::Rsn;
use crate::table::ScanTable;

/// Run every check on `data`.
pub fn run(data: &[u8]) {
    frame_checks(data);
    rsn_checks(data);
    table_script(data);
}

fn inside(outer: &[u8], inner: &[u8]) -> bool {
    let range = outer.as_ptr_range();
    let sub = inner.as_ptr_range();
    sub.start >= range.start && sub.end <= range.end
}

fn frame_checks(data: &[u8]) {
    let Ok(frame) = Mgmt::parse(data) else {
        return;
    };
    if let Some(ies) = frame.body.ies() {
        assert!(inside(data, ies), "element list outside the frame");
        check_elements(ies);
    }
    if let Body::Beacon(b) | Body::ProbeResponse(b) = frame.body {
        let h = frame.header;
        let fields = BeaconFields {
            timestamp: b.timestamp,
            interval: b.interval,
            capability: b.capability,
        };
        let rebuilt = build::beacon(&h.addr3, h.seq_number(), fields, b.ies);
        if let Ok(again) = Mgmt::parse(&rebuilt) {
            assert!(matches!(again.body, Body::Beacon(x) if x == b));
        }
    }
    if let Some(bss) = Bss::from_frame(&frame, 6, -50, 1) {
        assert!(bss.ssid.len() <= MAX_SSID, "ssid over 32 octets");
        let mut table = ScanTable::new(4);
        table.update(bss);
        assert_eq!(table.len(), 1);
    }
}

fn check_elements(ies: &[u8]) {
    let mut walk = ElementIter::new(ies);
    let mut used = 0;
    for element in walk.by_ref() {
        assert!(inside(ies, element.raw) && element.raw.len() == element.body.len() + 2);
        used += element.raw.len();
    }
    assert!(used <= ies.len());
    assert_eq!(walk.truncated(), used < ies.len());
    match Elements::parse(ies) {
        Ok(e) => {
            assert!(e.ssid.is_none_or(|s| s.len() <= MAX_SSID && inside(ies, s)));
            assert!(e.vendor.len() <= MAX_VENDOR);
            assert!(e.rsn_ie.is_none_or(|r| inside(ies, r)));
            assert_eq!(e.truncated, used < ies.len());
        }
        Err(error) => assert_eq!(error, crate::Error::BadSsid),
    }
}

fn rsn_checks(data: &[u8]) {
    if let Ok(rsn) = Rsn::parse_body(data) {
        assert_eq!(Rsn::parse_body(&rsn.to_body()), Ok(rsn.clone()));
        if let Ok(ie) = rsn.to_ie() {
            assert_eq!(Rsn::parse_ie(&ie), Ok(rsn));
        }
    }
    let _ = Rsn::parse_ie(data);
}

fn synthetic(id: u8, rssi: i8, at: u64) -> Bss {
    Bss {
        bssid: [2, 0, 0, 0, 0, id],
        ssid: std::vec![id],
        hidden: false,
        channel: 1,
        ds_channel: None,
        rssi,
        capability: crate::cap::ESS,
        interval: 100,
        security: Security::Open,
        rsn_ie: None,
        country: None,
        ht: false,
        vht: false,
        he: false,
        rates: Vec::new(),
        last_seen: at,
        from_probe_response: false,
    }
}

fn table_script(data: &[u8]) {
    let Some((&cap, ops)) = data.split_first() else {
        return;
    };
    let capacity = usize::from(cap % 8);
    let mut table = ScanTable::new(capacity);
    let mut now = 0u64;
    let mut ops = ops;
    while let Some((op, rest)) = ops.split_first_chunk::<3>() {
        ops = rest;
        now += u64::from(op[2] & 0x0F);
        if op[0] % 4 == 3 {
            table.expire(now, u64::from(op[1]));
        } else {
            table.update(synthetic(op[1] % 16, op[2] as i8, now));
        }
        assert!(table.len() <= capacity, "capacity bound");
        let mut seen: Vec<_> = table.iter().map(|b| b.bssid).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), table.len(), "duplicate BSSID");
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::{for_seeds, Rng};

    fn replay(target: &str, run: fn(&[u8])) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for {target}");
        }
    }

    #[test]
    fn checked_in_seeds_replay() {
        replay("ieee80211", run);
    }

    #[test]
    fn random_bytes() {
        for_seeds("ieee80211_random_bytes", |_, rng| {
            let len = rng.below(200) as usize;
            run(&rng.bytes(len));
        });
    }

    /// Beacons built from random elements, then mutated: bit flips, cuts and
    /// stretched lengths.
    #[test]
    fn mutated_beacons() {
        for_seeds("ieee80211_mutated_beacons", |_, rng| {
            let mut ies = Vec::new();
            for _ in 0..rng.below(8) {
                let n = rng.below(40) as usize;
                let body = rng.bytes(n);
                let id = *rng.pick(&[0u8, 1, 3, 7, 45, 48, 50, 191, 221, 255, 9]);
                let _ = crate::ie::push_ie(&mut ies, id, &body);
            }
            let fields = BeaconFields {
                timestamp: rng.next_u64(),
                interval: 100,
                capability: crate::cap::ESS | (rng.next_u32() as u16 & 0x10),
            };
            let mut frame = build::beacon(&[2, 1, 1, 1, 1, 1], 7, fields, &ies);
            let flips = rng.below(6) as usize;
            rng.flip_bits(&mut frame, flips);
            let cut = rng.below(frame.len() as u64 / 2) as usize;
            frame.truncate(frame.len() - cut);
            run(&frame);
        });
    }

    #[test]
    fn rsn_from_valid_lists() {
        for_seeds("ieee80211_rsn", |_, rng: &mut Rng| {
            let mut body = std::vec![1, 0];
            body.extend_from_slice(&[0x00, 0x0F, 0xAC, rng.below(12) as u8]);
            for _ in 0..2 {
                let count = rng.below(4) as usize;
                body.extend_from_slice(&(count as u16).to_le_bytes());
                body.extend_from_slice(&rng.bytes(count * 4));
            }
            let n = rng.below(30) as usize;
            body.extend_from_slice(&rng.bytes(n));
            run(&body);
        });
    }
}
