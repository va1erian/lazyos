//! Fuzz entry point, shared by the seeded tests below and the cargo-fuzz
//! target (`fuzz/fuzz_targets/btsmp.rs`).
//!
//! [`run`] takes any bytes and uses them two ways:
//!
//! * as one SMP PDU: decoding must not panic, and a PDU that decodes must
//!   encode back to the same octets;
//! * as a script for the central: a flags octet (bit 0 requires an
//!   authenticated key, bit 1 offers our identity, bit 2 lets a short key
//!   size through), then frames, each a big-endian `u16` length and that many
//!   octets, delivered as inputs 100 ms apart (a frame starting `0xFF` is an
//!   Encryption Change, `0xFE` a user confirmation, `0xFD` a Tick much later).
//!   After every input: whatever the central sends decodes, an ended pairing
//!   stays ended and silent, the deadline only exists while active and never
//!   moves back, and at most one outcome is reported.

use alloc::vec::Vec;

use lazyos_crypto::rng::Entropy;

use crate::pairing::{Central, Config, DeviceAddr, Input, LocalIdentity};
use crate::pdu::{AddrType, Pdu};
use crate::secret::Secret;

const ME: DeviceAddr = DeviceAddr {
    kind: AddrType::Public,
    addr: [1, 2, 3, 4, 5, 6],
};
const PEER: DeviceAddr = DeviceAddr {
    kind: AddrType::Random,
    addr: [0xC6, 5, 4, 3, 2, 1],
};

/// Split a script body into frames; a truncated tail is dropped.
fn frames(mut body: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while body.len() >= 2 {
        let len = usize::from(u16::from_be_bytes([body[0], body[1]]));
        if body.len() < 2 + len {
            break;
        }
        out.push(&body[2..2 + len]);
        body = &body[2 + len..];
        if out.len() >= 64 {
            break;
        }
    }
    out
}

/// Arbitrary bytes in; no panic, invariants hold.
pub fn run(data: &[u8]) {
    if let Ok(pdu) = Pdu::decode(data) {
        assert_eq!(pdu.encode(), data, "a decoded PDU encodes back to itself");
    }
    script(data);
}

fn script(data: &[u8]) {
    let Some((&flags, body)) = data.split_first() else {
        return;
    };
    let mut cfg = Config::new(ME, PEER);
    cfg.require_authenticated = flags & 1 != 0;
    if flags & 2 != 0 {
        cfg.local_identity = Some(LocalIdentity {
            irk: Secret::new([0x33; 16]),
            address: ME,
        });
    }
    if flags & 4 != 0 {
        cfg.min_key_size = 7;
    }
    let mut rng = Entropy::new();
    rng.seed(b"btsmp fuzz");
    let mut central = Central::new(cfg, rng);
    let mut now = 0u64;
    let mut ended = false;
    let mut last_deadline = 0u64;
    let mut check = |central: &Central, step: &crate::pairing::Step, ended: &mut bool| {
        if *ended {
            assert!(
                step.send.is_empty() && step.outcome.is_none(),
                "silent after the end"
            );
        }
        for pdu in &step.send {
            assert!(Pdu::decode(pdu).is_ok(), "we only send what we can parse");
        }
        if step.outcome.is_some() {
            assert!(!*ended, "one outcome");
            assert!(!central.is_active());
            *ended = true;
        }
        assert_eq!(step.deadline_ms.is_some(), central.is_active());
        if let Some(deadline) = step.deadline_ms {
            assert!(deadline >= last_deadline, "the deadline never moves back");
            last_deadline = deadline;
        }
    };
    let step = central.step(now, Input::Start);
    check(&central, &step, &mut ended);
    for frame in frames(body) {
        now += 100;
        let input = match frame.first() {
            Some(0xFF) => Input::Encryption {
                ok: frame.get(1).is_none_or(|b| b & 1 == 0),
            },
            Some(0xFE) => Input::UserConfirm(frame.get(1).is_none_or(|b| b & 1 == 0)),
            Some(0xFD) => {
                now += 60_000;
                Input::Tick
            }
            _ => Input::Pdu(frame),
        };
        let step = central.step(now, input);
        check(&central, &step, &mut ended);
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::for_seeds;

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
        replay("btsmp", run);
    }

    #[test]
    fn random_bytes() {
        for_seeds("btsmp_random_bytes", |_, rng| {
            let len = rng.below(300) as usize;
            run(&rng.bytes(len));
        });
    }

    /// Random frames behind each flag combination, every code in the table.
    #[test]
    fn random_scripts() {
        for_seeds("btsmp_random_scripts", |_, rng| {
            let mut data = alloc::vec![rng.byte() & 7];
            for _ in 0..rng.below(12) {
                let len = rng.below(70) as usize;
                let mut frame = rng.bytes(len);
                if let Some(code) = frame.first_mut() {
                    *code = rng.below(16) as u8;
                }
                data.extend_from_slice(&(frame.len() as u16).to_be_bytes());
                data.extend_from_slice(&frame);
            }
            run(&data);
        });
    }
}
