//! A controller and peers that send garbage: nothing panics, the tables and
//! queues stay bounded, and no garbage makes a device type.

use btbond::BondStore;
use bthci::acl;
use btsim::HciIn;

use super::*;
use crate::host::MAX_ACL_QUEUE;
use crate::link::MAX_LINKS;

/// Every packet the simulator sends a host during a pairing, in order.
fn recorded_session() -> Vec<HciIn> {
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(3_000);
    assert!(
        rig.host.is_typing(addr(KBD)),
        "the recording is a whole pairing"
    );
    rig.recorded
}

fn drain(host: &mut crate::Host, now: u64) {
    // A hostile controller never answers, so credits never come back: take
    // what the credit allows, plus all the ACL and effects.
    let _ = host.poll_command(now);
    while host.poll_acl().is_some() {}
    while host.poll_effect().is_some() {}
}

fn check_bounds(host: &crate::Host) {
    assert!(host.link_count() <= MAX_LINKS);
    assert!(host.acl_tx.len() <= MAX_ACL_QUEUE);
}

#[test]
fn replayed_sessions_with_flipped_bits_never_panic() {
    let session = recorded_session();
    fuzzkit::for_seeds("bthost_replay", |_seed, rng| {
        let mut host = crate::Host::new(pair_with(&[KBD]), BondStore::new(), 0);
        let mut now = 0;
        for packet in &session {
            now += 1 + rng.below(3);
            let mut bytes = match packet {
                HciIn::Event(b) | HciIn::Acl(b) => b.clone(),
            };
            if rng.one_in(3) {
                let flips = 1 + rng.below(4) as usize;
                rng.flip_bits(&mut bytes, flips);
            }
            if rng.one_in(8) {
                let n = rng.below(bytes.len() as u64 + 1) as usize;
                bytes.truncate(n);
            }
            if rng.one_in(10) {
                continue;
            }
            match packet {
                HciIn::Event(_) => host.on_event(now, &bytes),
                HciIn::Acl(_) => host.on_acl(now, &bytes),
            }
            if rng.one_in(5) {
                host.on_tick(now);
            }
            drain(&mut host, now);
            check_bounds(&host);
        }
        host.on_tick(now + 100_000);
        drain(&mut host, now + 100_000);
    });
}

#[test]
fn random_bytes_never_panic_and_never_type() {
    fuzzkit::for_seeds("bthost_random", |_seed, rng| {
        let mut host = crate::Host::new(pair_with(&[KBD]), BondStore::new(), 0);
        let mut now = 0;
        let mut typed = false;
        for _ in 0..200 {
            now += rng.below(50);
            let n = rng.below(40) as usize;
            let mut bytes = rng.bytes(n);
            // Make events look like events and ACL like ACL often enough to
            // get past the first check.
            if rng.one_in(2) && !bytes.is_empty() {
                bytes[0] = *rng.pick(&[
                    0x03, 0x04, 0x05, 0x06, 0x08, 0x0E, 0x0F, 0x13, 0x17, 0x18, 0x31, 0x32, 0x33,
                    0x34, 0x36, 0x3B,
                ]);
                if bytes.len() > 1 {
                    bytes[1] = (bytes.len() - 2) as u8;
                }
            }
            if rng.one_in(2) {
                host.on_event(now, &bytes);
            } else {
                if bytes.len() >= 4 && rng.one_in(2) {
                    let len = (bytes.len() - 4) as u16;
                    bytes[2..4].copy_from_slice(&len.to_le_bytes());
                    bytes[1] &= 0x0F;
                }
                host.on_acl(now, &bytes);
            }
            if rng.one_in(10) {
                host.on_tick(now);
            }
            while let Some(effect) = host.poll_effect() {
                typed |= matches!(effect, Effect::OpenSource { .. } | Effect::Input { .. });
            }
            let _ = host.poll_command(now);
            while host.poll_acl().is_some() {}
            check_bounds(&host);
        }
        assert!(!typed, "garbage opened a source");
        let _ = acl::parse(&[]);
    });
}

#[test]
fn an_oversized_report_flood_is_bounded_by_the_decoder() {
    // A paired keyboard sending reports as fast as it can: every report is
    // decoded to edges that are released again; nothing accumulates.
    let (mut sim, kbd, _) = world();
    sim.br_mut(kbd).set_pairing_mode(true);
    let mut rig = Rig::fresh(sim, pair_with(&[KBD]));
    rig.run_for(3_000);
    assert!(rig.host.is_typing(addr(KBD)));
    for n in 0..200u32 {
        rig.sim.press_key(kbd, rig.now, 4 + (n % 20) as u8);
        rig.run_for(2);
    }
    rig.assert_clean();
    let (down, up) = rig.keys.iter().fold(
        (0, 0),
        |(d, u), (_, p)| if *p { (d + 1, u) } else { (d, u + 1) },
    );
    assert_eq!(down, up);
    assert!(rig.host.acl_tx.len() <= MAX_ACL_QUEUE);
}
