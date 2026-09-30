//! Byte-script fuzzing of the virtio-net parsers.
//!
//! [`run`] is the libFuzzer target (`fuzz/fuzz_targets/virtio_net.rs`) and the
//! body of the seeded tests below. The first byte picks a parser and the rest
//! is its hostile input; each arm checks the invariants the driver relies on:
//! nothing panics, a returned frame lies inside the buffer and within the
//! length policy, every clamped setting is in range.

use std::string::String;

use crate::config::NetConfig;
use crate::frame::{classify, rx_frame, FrameClass, RxError};
use crate::hdr::{NetHdr, HDR_LEN};
use crate::settings::{parse_mac_override, IrqMode, Raw, Settings, MAX_ENTRIES, MIN_ENTRIES};
use crate::{features, ETH_HEADER, MAX_MTU, MIN_MTU};

/// Interpret `data`; the first byte picks the parser.
pub fn run(data: &[u8]) {
    let Some((&which, rest)) = data.split_first() else {
        return;
    };
    match which % 4 {
        0 => config(rest),
        1 => receive(rest),
        2 => header(rest),
        _ => settings(rest),
    }
}

fn take<'a>(data: &mut &'a [u8], n: usize) -> &'a [u8] {
    let n = n.min(data.len());
    let (head, tail) = data.split_at(n);
    *data = tail;
    head
}

fn take_u64(data: &mut &[u8]) -> u64 {
    let mut bytes = [0u8; 8];
    let got = take(data, 8);
    bytes[..got.len()].copy_from_slice(got);
    u64::from_le_bytes(bytes)
}

fn config(mut data: &[u8]) {
    let offered = take_u64(&mut data);
    // The driver only ever negotiates a subset, but the parser is fed whatever.
    let config = NetConfig::from_bytes(offered, data);
    assert_eq!(config.mac.is_some(), offered & features::MAC != 0);
    assert_eq!(config.mtu.is_some(), offered & features::MTU != 0);
    if offered & features::MQ == 0 {
        assert_eq!(config.max_pairs, 1);
    }
    if offered & features::STATUS == 0 {
        assert!(config.link_up(), "without STATUS the link is always up");
    }
    if let Some(mac) = config.usable_mac() {
        assert_eq!(mac[0] & 1, 0);
        assert_ne!(mac, [0; 6]);
    }
}

fn receive(mut data: &[u8]) {
    let written = u32::from_le_bytes(take_u64(&mut data).to_le_bytes()[..4].try_into().unwrap());
    let max_frame = usize::from(u16::from_le_bytes([data.first().copied().unwrap_or(0), 5]));
    let buf = data.get(1..).unwrap_or(&[]);
    let result = rx_frame(buf, written, max_frame);
    match result {
        Ok(frame) => {
            let start = buf.as_ptr() as usize;
            let at = frame.as_ptr() as usize;
            assert!(
                at >= start + HDR_LEN && at + frame.len() <= start + buf.len(),
                "frame lies inside the buffer"
            );
            assert_eq!(frame.len(), written as usize - HDR_LEN);
            assert_eq!(classify(frame.len(), max_frame), FrameClass::Ok);
            assert!(frame.len() >= ETH_HEADER);
            assert!(NetHdr::decode(buf).unwrap().is_plain());
        }
        Err(RxError::Overrun) => assert!(written as usize > buf.len()),
        Err(RxError::NoHeader) => assert!((written as usize) < HDR_LEN),
        Err(RxError::NotPlain) => assert!(!NetHdr::decode(buf).unwrap().is_plain()),
        Err(RxError::Runt) => assert!((written as usize - HDR_LEN) < ETH_HEADER),
        Err(RxError::Oversize) => assert!(written as usize - HDR_LEN > max_frame),
    }
}

fn header(data: &[u8]) {
    match NetHdr::decode(data) {
        Some(hdr) => {
            assert!(data.len() >= HDR_LEN);
            assert_eq!(&hdr.encode()[..], &data[..HDR_LEN], "encode inverts decode");
            assert_eq!(hdr.is_plain(), hdr.flags == 0 && hdr.gso_type == 0);
        }
        None => assert!(data.len() < HDR_LEN),
    }
}

fn settings(mut data: &[u8]) {
    let number = |data: &mut &[u8]| match take(data, 1).first() {
        Some(0) => None,
        Some(1) => Some(u64::MAX),
        _ => Some(take_u64(data)),
    };
    let irq = number(&mut data);
    let poll = number(&mut data);
    let rx = number(&mut data);
    let tx = number(&mut data);
    let mtu = number(&mut data);
    let text = String::from_utf8_lossy(data);
    let mid = text
        .char_indices()
        .nth(text.chars().count() / 2)
        .map_or(text.len(), |(i, _)| i);
    let (mode, mac) = text.split_at(mid);
    let raw = Raw {
        irq_mode: irq.map(|_| mode),
        poll_interval_ms: poll,
        rx_ring_entries: rx,
        tx_ring_entries: tx,
        mtu,
        mac_override: Some(mac),
    };
    let s = Settings::from_raw(&raw);
    for entries in [s.rx_entries, s.tx_entries] {
        assert!((MIN_ENTRIES..=MAX_ENTRIES).contains(&entries) && entries.is_power_of_two());
    }
    assert!((MIN_MTU..=MAX_MTU).contains(&s.mtu));
    assert!((1..=1000).contains(&s.poll_interval_ms));
    if let Some(mac) = s.mac_override {
        assert_eq!(mac[0] & 3, 2, "unicast and locally administered");
    }
    assert_eq!(s.mac_override, parse_mac_override(mac));
    if irq.is_none() {
        assert_eq!(s.irq_mode, IrqMode::Auto);
    }
    assert!(
        !s.needs_restart(&s),
        "a setting never asks to restart into itself"
    );
    assert_eq!(Settings::from_raw(&raw), s, "clamping is deterministic");
}

#[cfg(test)]
mod seeded {
    use super::*;
    use fuzzkit::for_seeds;

    /// Replay every checked-in seed (`fuzz/seeds/<target>`) and every saved
    /// crash (`fuzz/regressions/<target>`) through the entry point, so the
    /// corpus stays valid and a fixed bug stays fixed under plain `cargo test`.
    fn replay(target: &str, run: fn(&[u8])) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
                continue;
            };
            for entry in entries.flatten() {
                let data = std::fs::read(entry.path()).unwrap();
                run(&data);
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for {target}");
        }
    }

    fn many(name: &str, which: u8, max_len: u64) {
        for_seeds(name, |_, rng| {
            for _ in 0..200 {
                let len = rng.range(0, max_len) as usize;
                let mut data = rng.bytes(len);
                data.insert(0, which);
                run(&data);
            }
        });
    }

    #[test]
    fn config_parser_survives_random_bytes() {
        many("virtio_net::config", 0, 40);
    }

    #[test]
    fn receive_parser_survives_random_buffers() {
        many("virtio_net::receive", 1, 2200);
    }

    #[test]
    fn receive_parser_survives_structured_completions() {
        for_seeds("virtio_net::receive_structured", |_, rng| {
            for _ in 0..200 {
                // A buffer that starts like a real completion, with a length
                // report that is sometimes right and sometimes hostile.
                let frame_len = rng.range(0, 1700) as usize;
                let mut data = alloc_completion(frame_len);
                if rng.one_in(3) {
                    rng.flip_bits(&mut data[13..], 3);
                }
                let written = match rng.below(5) {
                    0 => rng.next_u32(),
                    1 => (HDR_LEN + frame_len) as u32 + 1,
                    _ => (HDR_LEN + frame_len) as u32,
                };
                let mut input = std::vec![1u8];
                input.extend_from_slice(&u64::from(written).to_le_bytes());
                input.push(0xEA); // max_frame low byte, high byte is fixed at 5: 0x05EA = 1514
                input.extend_from_slice(&data);
                run(&input);
            }
        });
    }

    fn alloc_completion(frame_len: usize) -> std::vec::Vec<u8> {
        let mut buf = std::vec![0u8; 2048];
        buf[..HDR_LEN].copy_from_slice(&NetHdr::PLAIN.encode());
        for (i, b) in buf[HDR_LEN..].iter_mut().take(frame_len).enumerate() {
            *b = i as u8;
        }
        buf
    }

    #[test]
    fn header_codec_round_trips() {
        many("virtio_net::header", 2, 30);
    }

    #[test]
    fn settings_are_always_clamped() {
        many("virtio_net::settings", 3, 120);
    }

    #[test]
    fn checked_in_corpus_replays() {
        replay("virtio_net", run);
    }
}
