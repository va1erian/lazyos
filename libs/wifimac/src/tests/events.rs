//! Events under pressure: a radio in range forging frames that look like the
//! BSS's must not push out the control events, nor reach the owner faster
//! than the rate limit, nor before the owner can be waiting for them.

use std::vec::Vec;

use super::*;
use crate::{Event, FrameKind};

fn auth_from_sta() -> Vec<u8> {
    build::auth(&AP, &STA, &AP, 0, (0, 1, 0), &[])
}

/// Joined, with an authentication request sent so responses are expected.
fn authenticating() -> SoftMac<FakeRadio> {
    let mut m = joined(false);
    m.tx_mgmt(OWNER, 0, &auth_from_sta()).unwrap();
    m
}

#[test]
fn rxmgmt_is_rate_limited_and_bounded() {
    let mut m = authenticating();
    let frame = build::auth(&STA, &AP, &AP, 1, (0, 2, 0), &[]);
    for _ in 0..100 {
        hear(&mut m, frame.clone(), 6);
    }
    m.poll(30);
    m.poll(31);
    let per_sec = crate::mac::RXMGMT_PER_SEC as usize;
    let mut count = 0;
    while m.next_event().is_some() {
        count += 1;
    }
    assert_eq!(count, per_sec, "one second's budget");
    assert_eq!(m.stats().mgmt_dropped, 100 - count as u64);
    assert_eq!(m.stats().mgmt_rx, 100);
    // Over several seconds of flood, the queue holds at most its frame share.
    for t in 1..=4u64 {
        for _ in 0..100 {
            hear(&mut m, frame.clone(), 6);
        }
        m.poll(t * 1000 + 31);
        m.poll(t * 1000 + 32);
    }
    let events: Vec<Event> = core::iter::from_fn(|| m.next_event()).collect();
    let frames = events
        .iter()
        .filter(|e| matches!(e, Event::RxMgmt { .. }))
        .count();
    assert_eq!(frames, crate::events::RXMGMT_QUEUE);
    // The AP has been silent all along: that is reported too.
    assert!(events.contains(&Event::BeaconLoss));
}

#[test]
fn a_flood_never_pushes_out_the_end_of_the_association() {
    let mut m = authenticating();
    let frame = build::auth(&STA, &AP, &AP, 1, (0, 2, 0), &[]);
    // Fill the queue's frame share over a few seconds, then the AP's
    // deauthentication arrives with the budget spent.
    for t in 0..3u64 {
        for _ in 0..40 {
            hear(&mut m, frame.clone(), 6);
        }
        m.poll(t * 1000 + 40);
        m.poll(t * 1000 + 41);
    }
    for _ in 0..40 {
        hear(&mut m, frame.clone(), 6);
    }
    hear(&mut m, build::deauth(&STA, &AP, &AP, 9, 6), 6);
    hear(&mut m, build::deauth(&STA, &AP, &AP, 10, 7), 6);
    m.poll(2500);
    let events: Vec<Event> = core::iter::from_fn(|| m.next_event()).collect();
    let ends: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::Deauthenticated { .. }))
        .collect();
    assert_eq!(ends, [&Event::Deauthenticated { reason: 6 }], "one, folded");
    assert!(events.len() <= crate::events::EVENT_QUEUE);
    assert!(!m.link_up());
}

#[test]
fn control_events_evict_frames_when_the_queue_is_full() {
    use crate::events::{Events, EVENT_QUEUE, RXMGMT_QUEUE};
    let frame = || Event::RxMgmt {
        kind: FrameKind::Mgmt,
        rssi_dbm: 0,
        frame: Vec::new(),
    };
    let mut events = Events::default();
    for i in 0..EVENT_QUEUE {
        let pushed = events.push(frame());
        assert_eq!(pushed.queued, i < RXMGMT_QUEUE);
    }
    let room = (EVENT_QUEUE - RXMGMT_QUEUE) as u32;
    for id in 0..room + 3 {
        let pushed = events.push(Event::ScanDone {
            scan_id: id,
            aborted: false,
        });
        assert!(pushed.queued);
        assert_eq!(pushed.dropped, u64::from(id >= room), "a frame evicted");
    }
    assert_eq!(events.len(), EVENT_QUEUE);
    // A queue of control events alone drops the newest.
    let mut full = Events::default();
    for id in 0..EVENT_QUEUE as u32 {
        full.push(Event::ScanDone {
            scan_id: id,
            aborted: true,
        });
    }
    assert!(!full.push(Event::BeaconLoss).queued);
    // What could not be delivered goes back first.
    let first = events.pop().unwrap();
    events.unpop(first.clone());
    assert_eq!(events.pop(), Some(first));
}

#[test]
fn responses_need_their_request_and_eapol_an_association() {
    let mut m = joined(false);
    let assoc_resp = build::assoc_response(&AP, &STA, 2, 1, 0, 1, &[]);
    let eapol = ethernet(&STA, &AP, 0x888E, &[2, 3, 0, 95]);
    hear(&mut m, assoc_resp.clone(), 6);
    receive(&mut m, from_ap(&eapol, None, None, 0), 30);
    assert_eq!(m.next_event(), None, "no request, no association");
    let request = build::assoc_request(&STA, &AP, 0, 1, 10, &[]);
    m.tx_mgmt(OWNER, 0, &request).unwrap();
    hear(&mut m, assoc_resp.clone(), 6);
    receive(&mut m, from_ap(&eapol, None, None, 0), 40);
    assert!(matches!(
        m.next_event(),
        Some(Event::RxMgmt {
            kind: FrameKind::Mgmt,
            ..
        })
    ));
    assert!(matches!(
        m.next_event(),
        Some(Event::RxMgmt {
            kind: FrameKind::Eapol,
            ..
        })
    ));
}

#[test]
fn retransmissions_of_unprotected_unicast_frames_are_dropped() {
    let mut m = authenticating();
    let frame = build::auth(&STA, &AP, &AP, 1, (0, 2, 0), &[]);
    let mut retry = frame.clone();
    retry[1] |= 0x08;
    hear(&mut m, frame.clone(), 6);
    hear(&mut m, retry.clone(), 6);
    m.poll(30);
    assert!(m.next_event().is_some());
    assert_eq!(m.next_event(), None, "the retry repeats the last");
    // A retry that is not a repeat (another sequence number) is delivered.
    let mut other = build::auth(&STA, &AP, &AP, 2, (0, 2, 0), &[]);
    other[1] |= 0x08;
    hear(&mut m, other, 6);
    m.poll(40);
    assert!(m.next_event().is_some());
}
