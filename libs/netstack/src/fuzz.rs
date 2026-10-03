//! Byte-script fuzzing of the stack against hostile frames.
//!
//! [`run`] reads its input as a script: inject a raw frame, advance the clock
//! while a scripted gateway answers (and what it answers may be truncated,
//! bit-flipped, dropped or duplicated on the way), start pings, renew the
//! lease. It is the libFuzzer target (`fuzz/fuzz_targets/netstack.rs`) and the
//! body of the seeded tests.
//!
//! **Invariants**, checked after every step: nothing panics and every `poll`
//! returns; everything the stack transmits is a legal frame (14..=1514 bytes);
//! memory is bounded (pending pings never exceed the cap, at most three
//! resolvers, the ring guard pages are never written); an address the stack
//! holds is a sane unicast address with a /1../30 prefix; the ping counters
//! add up (answered + timed out + outstanding never exceeds sent, and a
//! result appears once).

use std::vec::Vec;

use framering::MAX_FRAME;

use crate::config::{is_usable_unicast, Mode, StaticConfig};
use crate::stack::{MAX_DNS, MAX_PINGS};
use crate::testnet::*;

struct Script<'a> {
    data: &'a [u8],
    at: usize,
}

impl Script<'_> {
    fn done(&self) -> bool {
        self.at >= self.data.len()
    }
    fn u8(&mut self) -> u8 {
        let b = self.data.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        b
    }
    fn u16(&mut self) -> u16 {
        u16::from(self.u8()) << 8 | u16::from(self.u8())
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.u8()).collect()
    }
}

/// Damage `frame` the way a hostile or broken path might.
fn mutate(frame: &mut Vec<u8>, s: &mut Script) -> bool {
    match s.u8() % 10 {
        0..=4 => {}
        5 => {
            let keep = usize::from(s.u16()) % (frame.len() + 1);
            frame.truncate(keep);
        }
        6 | 7 => {
            for _ in 0..=(s.u8() % 8) {
                if frame.is_empty() {
                    break;
                }
                let at = usize::from(s.u16()) % frame.len();
                frame[at] ^= 1 << (s.u8() % 8);
            }
        }
        8 => {
            let extra = usize::from(s.u16() % 600);
            frame.extend(core::iter::repeat_n(0xAB, extra));
            frame.truncate(MAX_FRAME);
        }
        _ => return false,
    }
    true
}

fn checksum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for pair in bytes.chunks(2) {
        sum += u32::from(pair[0]) << 8 | u32::from(*pair.get(1).unwrap_or(&0));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Make a damaged frame pass the checksum layers again, so the damage reaches
/// the code behind them (a flipped bit otherwise dies at the IP or UDP
/// checksum, which is right, but is not what this is meant to reach).
fn fix_checksums(frame: &mut [u8]) {
    if frame.len() < 34 || frame[12..14] != [0x08, 0x00] || frame[14] >> 4 != 4 {
        return;
    }
    let ihl = usize::from(frame[14] & 0xF) * 4;
    if ihl < 20 || frame.len() < 14 + ihl {
        return;
    }
    frame[24] = 0;
    frame[25] = 0;
    let sum = checksum(&frame[14..14 + ihl]);
    frame[24..26].copy_from_slice(&sum.to_be_bytes());
    let payload = 14 + ihl;
    match frame[23] {
        // UDP over IPv4: a zero checksum means "none".
        17 if frame.len() >= payload + 8 => {
            frame[payload + 6..payload + 8].copy_from_slice(&[0, 0])
        }
        1 if frame.len() >= payload + 4 => {
            frame[payload + 2] = 0;
            frame[payload + 3] = 0;
            let sum = checksum(&frame[payload..]);
            frame[payload + 2..payload + 4].copy_from_slice(&sum.to_be_bytes());
        }
        _ => {}
    }
}

fn check(lan: &mut Lan, sent_seen: &mut usize) {
    for frame in &lan.sent[*sent_seen..] {
        assert!(
            (14..=1514).contains(&frame.len()),
            "the stack sent a {}-byte frame",
            frame.len()
        );
    }
    *sent_seen = lan.sent.len();
    assert!(lan.stack.pings_outstanding() <= MAX_PINGS);
    let state = lan.stack.state();
    assert!(state.dns.len() <= MAX_DNS);
    if let Some(text) = crate::resolvconf::render(&state.dns) {
        assert!(
            text.lines()
                .filter(|l| l.starts_with("nameserver "))
                .count()
                <= MAX_DNS
        );
    }
    if let Some(addr) = state.addr {
        assert!(is_usable_unicast(addr), "holding the address {addr:?}");
        assert!(
            (1..=30).contains(&state.prefix_len),
            "prefix {}",
            state.prefix_len
        );
    } else {
        assert!(
            state.gateway.is_none() && state.dns.is_empty(),
            "configuration without an address"
        );
    }
    if let Some(gateway) = state.gateway {
        assert!(is_usable_unicast(gateway));
    }
    let c = lan.stack.counters();
    assert!(
        c.pings_answered + c.pings_timed_out + lan.stack.pings_outstanding() as u64 <= c.pings_sent
    );
    lan.mem.assert_guards();
}

/// Interpret `data` as a script; see the module docs for the invariants.
pub fn run(data: &[u8]) {
    let mut s = Script { data, at: 0 };
    let mode = match s.u8() % 4 {
        0 => Mode::Static(StaticConfig {
            addr: [10, 0, 2, 15],
            prefix_len: 24,
            gateway: Some(GW_IP),
            dns: None,
        }),
        _ => Mode::Dhcp,
    };
    let seed = u64::from(s.u16()) << 48
        | u64::from(s.u16()) << 32
        | u64::from(s.u16()) << 16
        | u64::from(s.u16());
    let mut lan = Lan::with_seed(&mode, seed);
    let mut sent_seen = 0;
    let mut steps = 0u32;
    while !s.done() && steps < 20_000 {
        steps += 1;
        match s.u8() {
            // A raw frame of arbitrary bytes.
            0..=59 => {
                let len = usize::from(s.u16()) % 1700;
                let frame = s.bytes(len.min(s.data.len().saturating_sub(s.at) + 64));
                let _ = lan.deliver(&frame);
            }
            // Advance the clock; the gateway answers, and the answers are damaged.
            60..=139 => {
                lan.now += i64::from(s.u8() % 100 + 1) * 10;
                lan.stack.poll(lan.now);
                let mut buf = [0u8; MAX_FRAME];
                while let Ok(Some(n)) = lan.from_stack.pop(&mut buf) {
                    let frame = buf[..n].to_vec();
                    for answer in lan.gateway.react(&frame) {
                        let mut answer = answer;
                        if mutate(&mut answer, &mut s) && !answer.is_empty() {
                            if s.u8().is_multiple_of(2) {
                                fix_checksums(&mut answer);
                            }
                            let _ = lan.to_stack.push(&answer);
                        }
                    }
                    lan.sent.push(frame);
                }
                lan.stack.poll(lan.now);
            }
            140..=159 => {
                let dst = match s.u8() % 4 {
                    0 => GW_IP,
                    1 => [s.u8(), s.u8(), s.u8(), s.u8()],
                    2 => [10, 0, 2, s.u8()],
                    _ => [192, 168, s.u8(), s.u8()],
                };
                let len = usize::from(s.u16()) % 1500;
                let timeout = u64::from(s.u8()) * 20;
                let _ = lan.stack.ping(dst, len, timeout, lan.now);
            }
            160..=169 => lan.stack.renew(),
            170..=179 => lan.gateway.answer_echo = s.u8().is_multiple_of(2),
            180..=184 => lan.stack.cancel_ping(s.u16()),
            // The gateway sends a mangled frame of its own accord.
            185..=199 => {
                let mut frame = match s.u8() % 3 {
                    0 => echo_frame(
                        STACK_MAC,
                        GW_MAC,
                        GW_IP,
                        LEASE_IP,
                        s.u8().is_multiple_of(2),
                        s.u16(),
                        s.u16(),
                        &[1, 2, 3],
                    ),
                    1 => arp_reply(STACK_MAC, GW_MAC, GW_IP, LEASE_IP),
                    _ => echo_frame(
                        [0xFF; 6],
                        GW_MAC,
                        [s.u8(), 1, 2, 3],
                        [255; 4],
                        false,
                        1,
                        1,
                        &[],
                    ),
                };
                if mutate(&mut frame, &mut s) {
                    if s.u8().is_multiple_of(2) {
                        fix_checksums(&mut frame);
                    }
                    let _ = lan.deliver(&frame);
                }
            }
            // The gateway changes what it offers: any address, mask, router,
            // resolvers and lease it likes.
            200..=214 => {
                let g = &mut lan.gateway;
                g.offer_ip = [s.u8(), s.u8(), s.u8(), s.u8()];
                g.mask = match s.u8() % 4 {
                    0 => [255, 255, 255, 0],
                    1 => [s.u8(), s.u8(), s.u8(), s.u8()],
                    2 => [255, 255, 0, 0],
                    _ => [255, 255, 255, 252],
                };
                g.router = (!s.u8().is_multiple_of(4)).then(|| [s.u8(), s.u8(), s.u8(), s.u8()]);
                g.dns = (0..s.u8() % 5)
                    .map(|_| [s.u8(), s.u8(), s.u8(), s.u8()])
                    .collect();
                g.lease_secs = u32::from(s.u16()) * u32::from(s.u8());
            }
            _ => {
                lan.now += 10;
                lan.stack.poll(lan.now);
            }
        }
        let mut seqs: Vec<u16> = lan
            .stack
            .take_ping_results()
            .iter()
            .map(|r| r.seq)
            .collect();
        let reported = seqs.len();
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(seqs.len(), reported, "a ping was reported twice");
        check(&mut lan, &mut sent_seen);
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::for_seeds;

    #[test]
    fn hostile_scripts_never_break_the_stack() {
        for_seeds(
            "netstack::hostile_scripts_never_break_the_stack",
            |_, rng| {
                let len = rng.range(40, 2500) as usize;
                run(&rng.bytes(len));
            },
        );
    }

    #[test]
    fn structured_scripts_reach_the_protocol_paths() {
        for_seeds(
            "netstack::structured_scripts_reach_the_protocol_paths",
            |_, rng| {
                // Mostly clock steps and pings, so the DHCP and echo exchanges run,
                // with the occasional raw frame.
                let mut script =
                    std::vec![rng.byte(), rng.byte(), rng.byte(), rng.byte(), rng.byte()];
                for _ in 0..rng.range(30, 400) {
                    match rng.below(10) {
                        0..=4 => script.extend_from_slice(&[100, rng.byte(), rng.byte()]),
                        5 | 6 => {
                            script.extend_from_slice(&[150, 0, rng.byte(), rng.byte(), rng.byte()])
                        }
                        7 => script.extend_from_slice(&[
                            190,
                            rng.byte(),
                            rng.byte(),
                            rng.byte(),
                            rng.byte(),
                            rng.byte(),
                            rng.byte(),
                        ]),
                        8 => script.extend_from_slice(&[
                            30,
                            rng.byte(),
                            rng.byte(),
                            rng.byte(),
                            rng.byte(),
                        ]),
                        _ => script.extend_from_slice(&[165]),
                    }
                }
                run(&script);
            },
        );
    }

    #[test]
    fn checked_in_seeds_replay() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join("netstack")) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for netstack");
        }
    }
}
