//! The `hw.v1` control surface: ownership, the country gate, join,
//! management frames both ways, association state, keys, leave, beacon
//! loss, deauthentication, release.

use std::vec;
use std::vec::Vec;

use ieee80211::frame::SUB_DEAUTH;

use super::*;
use crate::{Error, Event, FrameKind, ScanRequest};

fn join_request(bssid: &Mac, channel: u32) -> JoinRequest {
    JoinRequest {
        bssid: bssid.to_vec(),
        channel,
        band: u32::MAX,
        width: 0,
    }
}

fn auth_from_sta() -> Vec<u8> {
    build::auth(&AP, &STA, &AP, 0, (0, 1, 0), &[])
}

#[test]
fn facts_and_mtu() {
    let m = softmac();
    let info = m.info();
    assert_eq!(info.mac, STA);
    assert_eq!(m.radio().address, Some(STA));
    assert_eq!((info.max_mgmt, info.mtu, info.ciphers), (1484, 1442, 1));
    assert!(!info.scan_offload);
}

#[test]
fn one_owner_at_a_time() {
    let mut m = softmac();
    assert_eq!(
        m.set_country(OWNER, "FR"),
        Err(Error::Denied),
        "nobody attached"
    );
    m.attach(OWNER).unwrap();
    assert_eq!(m.attach(OTHER), Err(Error::Busy));
    assert_eq!(m.attach(OWNER), Err(Error::Busy));
    assert_eq!(m.set_country(OTHER, "FR"), Err(Error::Denied));
    assert_eq!(m.detach(OTHER), Err(Error::Denied));
    assert_eq!(m.leave(OTHER, 3), Err(Error::Denied));
    m.detach(OWNER).unwrap();
    assert_eq!(m.owner(), None);
    m.attach(OTHER).unwrap();
    assert_eq!(m.owner(), Some(OTHER));
}

#[test]
fn nothing_transmits_before_a_country() {
    let mut m = softmac();
    m.attach(OWNER).unwrap();
    assert_eq!(m.join(OWNER, &join_request(&AP, 6), 0), Err(Error::Again));
    assert_eq!(m.set_country(OWNER, "fr"), Err(Error::Invalid));
    assert_eq!(m.set_country(OWNER, "FRA"), Err(Error::Invalid));
    m.set_country(OWNER, "00").unwrap();
    m.join(OWNER, &join_request(&AP, 6), 0).unwrap();
    // The country is forgotten with the owner.
    m.release();
    m.attach(OWNER).unwrap();
    assert_eq!(m.tx_mgmt(OWNER, 0, &auth_from_sta()), Err(Error::Again));
}

#[test]
fn join_validates_and_tunes() {
    let mut m = attached();
    for bad in [
        JoinRequest {
            bssid: vec![1; 6],
            ..join_request(&AP, 6)
        },
        JoinRequest {
            bssid: vec![2; 5],
            ..join_request(&AP, 6)
        },
        join_request(&[0; 6], 6),
        join_request(&AP, 14),
        join_request(&AP, 0),
        JoinRequest {
            width: 4,
            ..join_request(&AP, 6)
        },
    ] {
        assert_eq!(m.join(OWNER, &bad, 0), Err(Error::Invalid), "{bad:?}");
    }
    assert_eq!(
        m.tx_mgmt(OWNER, 0, &auth_from_sta()),
        Err(Error::NotConnected)
    );
    m.join(OWNER, &join_request(&AP, 6), 0).unwrap();
    assert_eq!(m.radio().tuned.last(), Some(&Channel::new(Band::Ghz2, 6)));
    assert_eq!(m.radio().bssid, Some((AP, 0)));
    assert!(!m.link_up());
}

#[test]
fn management_frames_out_are_checked_and_sequenced() {
    let mut m = joined(false);
    m.tx_mgmt(OWNER, FrameKind::Mgmt as u32, &auth_from_sta())
        .unwrap();
    m.tx_mgmt(OWNER, FrameKind::Mgmt as u32, &auth_from_sta())
        .unwrap();
    let sent = &m.radio().sent;
    assert_eq!(sent.len(), 2);
    let seq = |f: &Vec<u8>| u16::from_le_bytes([f[22], f[23]]) >> 4;
    assert_eq!(seq(&sent[1].0), seq(&sent[0].0) + 1);
    assert!(sent[0].1.basic_rate && !sent[0].1.data);

    let mut spoofed = auth_from_sta();
    spoofed[10] ^= 1;
    let mut flagged = auth_from_sta();
    flagged[1] = 0x40;
    let probe_resp = probe_response(&STA, b"x", 6);
    for bad in [
        spoofed,
        flagged,
        probe_resp,
        vec![0xB0, 0, 0],
        vec![0x08; 30],
    ] {
        assert_eq!(m.tx_mgmt(OWNER, 0, &bad), Err(Error::Invalid));
    }
    assert_eq!(m.tx_mgmt(OWNER, 2, &auth_from_sta()), Err(Error::Invalid));
    assert_eq!(m.tx_mgmt(OWNER, 0, &vec![0; 2000]), Err(Error::Invalid));
    assert_eq!(m.tx_mgmt(OTHER, 0, &auth_from_sta()), Err(Error::Denied));
    m.radio_mut().queue_full = true;
    assert_eq!(m.tx_mgmt(OWNER, 0, &auth_from_sta()), Err(Error::Busy));
}

#[test]
fn management_frames_in_only_from_the_bss_to_us() {
    let mut m = joined(false);
    let ok = build::auth(&STA, &AP, &AP, 1, (0, 2, 0), &[]);
    hear(&mut m, ok.clone(), 6);
    hear(&mut m, build::auth(&GW, &AP, &AP, 1, (0, 2, 0), &[]), 6);
    hear(&mut m, build::auth(&STA, &AP2, &AP2, 1, (0, 2, 0), &[]), 6);
    let mut protected = ok.clone();
    protected[1] = 0x40;
    hear(&mut m, protected, 6);
    m.poll(30);
    assert_eq!(
        m.next_event(),
        Some(Event::RxMgmt {
            kind: FrameKind::Mgmt,
            rssi_dbm: -40,
            frame: ok
        })
    );
    assert_eq!(m.next_event(), None);
    assert_eq!(m.stats().mgmt_rx, 1);
}

#[test]
fn association_state_drives_the_link() {
    let mut m = attached();
    assert_eq!(m.set_state(OWNER, 3, 1), Err(Error::NotConnected));
    m.set_state(OWNER, 0, 0).unwrap();
    let mut m = joined(false);
    assert_eq!(m.set_state(OWNER, 2, 0), Err(Error::Invalid), "AID 0");
    assert_eq!(m.set_state(OWNER, 2, 2008), Err(Error::Invalid));
    assert_eq!(m.set_state(OWNER, 4, 1), Err(Error::Invalid));
    m.set_state(OWNER, 1, 0).unwrap();
    m.set_state(OWNER, 2, 5).unwrap();
    assert_eq!(m.radio().bssid, Some((AP, 5)));
    assert!(!m.link_up());
    m.set_state(OWNER, 3, 5).unwrap();
    assert!(m.link_up());
    assert_eq!(m.radio().state, Some(StaState::Authorized));
    m.set_state(OWNER, 2, 5).unwrap();
    assert!(!m.link_up(), "a lower state takes the link down");
}

#[test]
fn keys_are_validated_and_installed() {
    let mut m = joined(true);
    assert_eq!(m.set_key(OWNER, &key(KeyKind::Pairwise, 0, &[])), Ok(()));
    let short = [0u8; 15];
    let bad = [
        KeyRequest {
            key: &short,
            ..key(KeyKind::Pairwise, 0, &[])
        },
        KeyRequest {
            cipher: 1,
            ..key(KeyKind::Pairwise, 0, &[])
        },
        key(KeyKind::Pairwise, 2, &[]),
        key(KeyKind::Group, 0, &[]),
        key(KeyKind::Group, 4, &[]),
        key(KeyKind::Igtk, 4, &[]),
        key(KeyKind::Group, 1, &[0; 9]),
        KeyRequest {
            addr: &AP2,
            ..key(KeyKind::Group, 1, &[])
        },
        KeyRequest {
            kind: 3,
            ..key(KeyKind::Group, 1, &[])
        },
    ];
    for request in bad {
        assert_eq!(
            m.set_key(OWNER, &request),
            Err(Error::Invalid),
            "{request:?}"
        );
    }
    m.radio_mut().slots_full = true;
    assert_eq!(
        m.set_key(OWNER, &key(KeyKind::Group, 2, &[])),
        Err(Error::NoSpace)
    );
    assert_eq!(m.radio().keys.len(), 1);
    assert_eq!(m.del_key(OWNER, 0, 0), Ok(()));
    assert_eq!(m.del_key(OWNER, 1, 3), Ok(()), "an empty slot");
    assert_eq!(m.del_key(OWNER, 1, 6), Err(Error::Invalid));
    assert!(m.radio().keys.is_empty());
    let mut idle = attached();
    assert_eq!(
        idle.set_key(OWNER, &key(KeyKind::Pairwise, 0, &[])),
        Err(Error::NotConnected)
    );
}

#[test]
fn authorized_needs_the_pairwise_key_once_any_key_exists() {
    let mut m = joined(true);
    m.set_state(OWNER, 2, 1).unwrap();
    m.set_key(OWNER, &key(KeyKind::Group, 1, &[])).unwrap();
    assert_eq!(m.set_state(OWNER, 3, 1), Err(Error::Invalid));
    m.set_key(OWNER, &key(KeyKind::Pairwise, 0, &[])).unwrap();
    m.set_state(OWNER, 3, 1).unwrap();
    assert!(m.link_up());
}

#[test]
fn leave_deauthenticates_and_forgets_everything() {
    let mut m = wpa2_authorized();
    m.radio_mut().sent.clear();
    m.leave(OWNER, 8).unwrap();
    let (deauth, options) = &m.radio().sent[0];
    assert_eq!(deauth[0] >> 4, SUB_DEAUTH);
    assert_eq!(&deauth[4..10], &AP);
    assert_eq!(&deauth[24..26], &[8, 0]);
    assert!(!options.protected);
    assert!(m.radio().keys.is_empty());
    assert_eq!(m.radio().bssid, None);
    assert!(!m.link_up());
    assert_eq!(
        m.tx_mgmt(OWNER, 0, &auth_from_sta()),
        Err(Error::NotConnected)
    );
    m.leave(OWNER, 3).unwrap();
    // Not authenticated: leaving sends nothing.
    let mut m = joined(false);
    m.leave(OWNER, 3).unwrap();
    assert!(m.radio().sent.is_empty());
}

#[test]
fn the_ap_ending_the_association_is_reported_and_closes_the_port() {
    let mut m = wpa2_authorized();
    let deauth = build::deauth(&STA, &AP, &AP, 3, 15);
    hear(&mut m, deauth.clone(), 6);
    m.poll(100);
    assert_eq!(
        m.next_event(),
        Some(Event::RxMgmt {
            kind: FrameKind::Mgmt,
            rssi_dbm: -40,
            frame: deauth
        })
    );
    assert_eq!(m.next_event(), Some(Event::Deauthenticated { reason: 15 }));
    assert!(!m.link_up());
    assert!(m.radio().keys.is_empty());
    assert_eq!(m.stats().deauths, 1);
    // A broadcast disassociation from the AP counts too; a spoofed one from
    // another BSS does not.
    hear(&mut m, build::disassoc(&[0xFF; 6], &AP2, &AP2, 3, 1), 6);
    hear(&mut m, build::disassoc(&[0xFF; 6], &AP, &AP, 3, 7), 6);
    m.poll(110);
    assert!(matches!(m.next_event(), Some(Event::RxMgmt { .. })));
    assert_eq!(m.next_event(), Some(Event::Deauthenticated { reason: 7 }));
    assert_eq!(m.next_event(), None);
}

#[test]
fn beacon_loss_is_reported_once_per_silence() {
    let mut m = joined(true);
    // Joined at 20 with the 100 TU interval: the deadline is 1040.
    hear(&mut m, beacon(&AP, b"LazyNet", 6, true), 6);
    m.poll(200);
    assert_eq!(m.next_deadline(), Some(1220));
    m.poll(1219);
    assert_eq!(m.next_event(), None);
    m.poll(1220);
    assert_eq!(m.next_event(), Some(Event::BeaconLoss));
    m.poll(5000);
    assert_eq!(m.next_event(), None, "reported once");
    assert_eq!(m.next_deadline(), None);
    // A beacon on another channel or from another BSS does not count.
    hear(&mut m, beacon(&AP, b"LazyNet", 1, true), 1);
    hear(&mut m, beacon(&AP2, b"LazyNet", 6, true), 6);
    m.poll(5001);
    assert_eq!(m.next_deadline(), None);
    hear(&mut m, beacon(&AP, b"LazyNet", 6, true), 6);
    m.poll(6000);
    m.poll(7100);
    assert_eq!(m.next_event(), Some(Event::BeaconLoss));
    assert_eq!(m.stats().beacon_losses, 2);
}

#[test]
fn release_leaves_the_radio_idle() {
    let mut m = wpa2_authorized();
    m.scan(
        OWNER,
        &ScanRequest {
            channels: vec![1],
            band: 0,
            ..Default::default()
        },
        50,
    )
    .unwrap();
    m.radio_mut().sent.clear();
    m.release();
    assert!(!m.radio().scanning);
    assert!(m.radio().keys.is_empty());
    assert_eq!(m.radio().bssid, None);
    assert_eq!(m.radio().sent.len(), 1, "a deauthentication");
    assert_eq!(m.owner(), None);
    assert_eq!(m.next_event(), None);
    assert!(!m.link_up());
}

#[test]
fn events_are_bounded() {
    let mut m = joined(false);
    let frame = build::auth(&STA, &AP, &AP, 1, (0, 2, 0), &[]);
    for _ in 0..100 {
        hear(&mut m, frame.clone(), 6);
    }
    m.poll(30);
    m.poll(31);
    let mut count = 0;
    while m.next_event().is_some() {
        count += 1;
    }
    assert_eq!(count, crate::events::EVENT_QUEUE);
    assert_eq!(m.stats().mgmt_dropped, 100 - count as u64);
    assert_eq!(m.stats().mgmt_rx, 100);
}
