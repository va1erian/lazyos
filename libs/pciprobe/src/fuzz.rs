//! Fuzz entry point: untrusted bytes in, no panic and no escape out.

use crate::{check_config, format_bdf, parse_bdf, parse_range, Policy, MAX_MMIO_BYTES};

/// Feed `data` to the config parser, the BDF and range parsers and the
/// planner. Invariants: nothing panics; every segment of a plan lies inside
/// the range it names, the segments tile the request exactly, and every
/// range of an accepted plan is valid; a parsed BDF formats back to itself.
pub fn run(data: &[u8]) {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let policy = Policy::from_cfg(text);
    if let Some(bdf) = parse_bdf(text) {
        assert_eq!(parse_bdf(&format_bdf(bdf)), Some(bdf));
    }
    let _ = parse_range(text);
    // Derive a request from the bytes themselves.
    let word = |at: usize| {
        let mut raw = [0u8; 8];
        for (i, b) in data.iter().skip(at).take(8).enumerate() {
            raw[i] = *b;
        }
        u64::from_le_bytes(raw)
    };
    let (vendor, device) = (word(0) as u16, (word(0) >> 16) as u16);
    let (offset, len) = (word(8), word(16) % (2 * MAX_MMIO_BYTES));
    let _ = check_config(word(24), word(32));
    let candidates = [policy.clone(), Policy::with_ranges(false, policy.extra())];
    // Aim at a real built-in device too, so the allow path is exercised.
    for built in crate::builtin::RANGES.iter().take(1) {
        let _ = candidates[1].plan(built.vendor, built.device, built.bar, offset, len);
    }
    for p in &candidates {
        if let Ok(plan) = p.plan(vendor, device, (word(40) % 8) as u8, offset, len) {
            let mut at = offset;
            for seg in &plan {
                assert!(seg.range.valid());
                assert_eq!(seg.offset, at);
                assert!(seg.range.start <= seg.offset);
                assert!(seg.offset + seg.len <= seg.range.end);
                at += seg.len;
            }
            assert_eq!(at, offset + len);
        }
    }
}
