use std::vec;
use std::vec::Vec;

use super::{long_map, server};
use crate::hogp::{Hogp, HogpEvent};

fn seed(name: &str) -> Vec<u8> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/btgatt");
    std::fs::read(dir.join(name)).unwrap()
}

#[test]
fn seeded_fuzz_runs() {
    let mut server = server::keyboard(&long_map());
    let mut hogp = Hogp::new(247);
    let mut script = vec![0u8];
    let mut step = hogp.discover(0);
    // Record a real conversation as a script, then mutate it.
    while let Some(pdu) = step.send.pop() {
        let Some(answer) = server.handle(&pdu) else {
            break;
        };
        script.push(answer.len() as u8);
        script.extend_from_slice(&answer);
        step = hogp.on_pdu(1, &answer);
    }
    fuzzkit::for_seeds("btgatt_fuzz_run", |_seed, rng| {
        let mut data = match rng.below(3) {
            0 => script.clone(),
            1 => {
                let n = rng.below(120) as usize;
                rng.bytes(n)
            }
            _ => {
                let mut d = vec![rng.below(2) as u8];
                for _ in 0..rng.below(12) {
                    let n = rng.below(40) as usize;
                    d.push(n as u8);
                    d.extend(rng.bytes(n));
                }
                d
            }
        };
        let flips = rng.below(6) as usize;
        rng.flip_bits(&mut data, flips);
        if rng.one_in(3) {
            let n = rng.below(data.len() as u64 + 1) as usize;
            data.truncate(n);
        }
        crate::fuzz::run(&data);
    });
}

/// The checked-in seeds mean what their names say.
#[test]
fn the_checked_in_seeds_mean_what_they_say() {
    let events = crate::fuzz::drive(&seed("discovery_complete"));
    assert!(
        matches!(events.last(), Some(HogpEvent::Discovered(s)) if s.report_map.len() == 6),
        "{events:?}"
    );
    let events = crate::fuzz::drive(&seed("resume"));
    assert_eq!(events.last(), Some(&HogpEvent::Ready));
    let events = crate::fuzz::drive(&seed("no_hid_service"));
    assert!(matches!(events.last(), Some(HogpEvent::Failed(_))));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/btgatt");
    for entry in std::fs::read_dir(dir).unwrap() {
        crate::fuzz::run(&std::fs::read(entry.unwrap().path()).unwrap());
    }
}
