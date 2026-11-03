//! LE under a hostile controller and hostile peers, and under sustained use:
//! garbage never panics or types, the tables stay bounded, and thousands of
//! connections leave nothing behind.

use btbond::BondStore;
use bthci::acl;
use btsim::HciIn;

use super::*;
use crate::host::MAX_ACL_QUEUE;
use crate::link::MAX_LINKS;

/// Every packet the simulator sends a host during an LE keyboard pairing.
fn recorded_session() -> Vec<HciIn> {
    let (sim, _, _) = le_world();
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
    rig.run_for(4_000);
    assert!(rig.host.is_typing(addr(LE_KBD)), "a whole pairing");
    rig.recorded
}

fn seeded_host(cfg: Config) -> crate::Host {
    let mut host = crate::Host::new(cfg, BondStore::new(), 0);
    host.seed_entropy(b"hostile test");
    host
}

fn drain(host: &mut crate::Host, now: u64) {
    let _ = host.poll_command(now);
    while host.poll_acl().is_some() {}
    while host.poll_effect().is_some() {}
}

fn check_bounds(host: &crate::Host) {
    assert!(host.link_count() <= MAX_LINKS);
    assert!(host.acl_tx.len() <= MAX_ACL_QUEUE);
}

#[test]
fn replayed_le_sessions_with_flipped_bits_never_panic() {
    let session = recorded_session();
    fuzzkit::for_seeds("bthost_le_replay", |_seed, rng| {
        let mut host = seeded_host(le_pair_with(&[LE_KBD]));
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
fn random_le_events_never_panic_and_never_type() {
    fuzzkit::for_seeds("bthost_le_random", |_seed, rng| {
        let mut host = seeded_host(le_pair_with(&[LE_KBD]));
        let mut now = 0;
        let mut typed = false;
        for _ in 0..200 {
            now += rng.below(50);
            let n = rng.below(40) as usize;
            let mut bytes = rng.bytes(n);
            if rng.one_in(2) && bytes.len() >= 3 {
                // LE Meta (or Encryption Change / Disconnection) shaped.
                bytes[0] = *rng.pick(&[0x3E, 0x3E, 0x3E, 0x08, 0x05, 0x0E, 0x0F]);
                bytes[1] = (bytes.len() - 2) as u8;
                bytes[2] = *rng.pick(&[0x01, 0x02, 0x05, 0x0A]);
            }
            if rng.one_in(3) {
                host.on_event(now, &bytes);
            } else if bytes.len() >= 4 {
                let len = (bytes.len() - 4) as u16;
                bytes[2..4].copy_from_slice(&len.to_le_bytes());
                bytes[1] &= 0x0F;
                // L2CAP on the fixed channels ATT and SMP.
                if bytes.len() >= 8 {
                    bytes[6] = *rng.pick(&[4, 6, 5]);
                    bytes[7] = 0;
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

/// Sustained use: pair, then drop and reconnect many times, with the
/// address changing each time. Nothing may accumulate: one link at most,
/// one source per half of the device, one bond, bounded queues.
#[test]
fn many_reconnects_leave_nothing_behind() {
    let (sim, kbd, _) = le_world();
    let mut rig = Rig::fresh(sim, le_pair_with(&[LE_KBD]));
    rig.run_for(4_000);
    assert!(rig.host.is_typing(addr(LE_KBD)));
    rig.sim.le_mut(kbd).use_rpa = true;
    let rounds = 60u64;
    for round in 0..rounds {
        rig.sim.drop_link(kbd, rig.now, 0x13);
        rig.run_for(200);
        assert_eq!(rig.host.link_count(), 0, "round {round}");
        assert!(rig.sources.is_empty(), "round {round}");
        rig.sim.press_key(kbd, rig.now, KEY_A + (round % 20) as u8);
        rig.run_for(1_500);
        assert!(
            rig.host.is_typing(addr(LE_KBD)),
            "round {round}: {:#?}",
            rig.traces
        );
        assert_eq!(rig.sources.len(), 1, "round {round}");
        assert!(rig.host.acl_tx.len() <= MAX_ACL_QUEUE);
    }
    rig.assert_clean();
    assert_eq!(rig.host.store().len(), 1);
    assert_eq!(rig.typed().len() as u64, rounds);
    // Every key that went down came up before its source closed.
    assert!(rig.kernel_released.is_empty());
    assert_eq!(rig.opened.len() as u64, rounds + 1);
}

#[test]
fn a_pairing_cut_short_leaves_no_bond_and_no_link() {
    let (sim, kbd, _) = le_world();
    let mut rig = Rig::boot(sim, BondStore::new(), le_pair_with(&[LE_KBD]), 0);
    // Run until the link is up and SMP has started, then pull the plug.
    for _ in 0..400 {
        rig.run_for(2);
        if rig.traced("PAIR:START") {
            break;
        }
    }
    assert!(rig.traced("PAIR:START"), "{:#?}", rig.traces);
    rig.sim.drop_link(kbd, rig.now, 0x08);
    rig.run_for(100);
    assert_eq!(rig.host.link_count(), 0);
    assert!(rig.host.store().is_empty());
    assert!(rig.opened.is_empty());
}
