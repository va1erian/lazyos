use alloc::vec::Vec;

use crate::builtin::RANGES;
use crate::{
    check_config, format_bdf, parse_bdf, parse_range, Denied, Policy, Range, MAX_EXTRA,
    MAX_MMIO_BYTES,
};

const HD620: (u16, u16) = (0x8086, 0x5916);
const VGA: (u16, u16) = (0x1234, 0x1111);

fn on() -> Policy {
    Policy::from_cfg("diag.dbg=1\ndiag.dbg.mmio=1\n")
}

#[test]
fn the_builtin_table_is_sorted_valid_and_disjoint() {
    for r in RANGES {
        assert!(r.valid(), "{r:?}");
    }
    for pair in RANGES.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        assert!(
            (a.vendor, a.device, a.bar, a.start) < (b.vendor, b.device, b.bar, b.start),
            "unsorted: {a:?} {b:?}"
        );
        if (a.vendor, a.device, a.bar) == (b.vendor, b.device, b.bar) {
            assert!(a.end <= b.start, "overlap: {a:?} {b:?}");
        }
    }
}

#[test]
fn config_is_off_by_default_and_needs_diag_dbg() {
    assert_eq!(
        Policy::off().plan(VGA.0, VGA.1, 2, 0x500, 4),
        Err(Denied::Off)
    );
    for cfg in [
        "",
        "diag.dbg.mmio=1\n",
        "diag.dbg=1\n",
        "diag.dbg=1\ndiag.dbg.mmio=0\n",
        "diag.dbg=1\ndiag.dbg.mmio=1\ndiag.dbg=0\n",
    ] {
        assert!(!Policy::from_cfg(cfg).mmio, "{cfg:?}");
    }
    assert!(on().mmio);
    assert!(!on().claimed);
    assert!(Policy::from_cfg("diag.dbg=1\ndiag.dbg.mmio=1\ndiag.dbg.mmio.claimed=1\n").claimed);
}

#[test]
fn a_registered_read_is_allowed_and_everything_else_is_not() {
    let p = on();
    let ok = p.plan(VGA.0, VGA.1, 2, 0x500, 4).unwrap();
    assert_eq!(ok.len(), 1);
    assert_eq!((ok[0].offset, ok[0].len), (0x500, 4));
    // The VGA input-status register: a hole between the ranges.
    assert_eq!(
        p.plan(VGA.0, VGA.1, 2, 0x41a & !3, 4),
        Err(Denied::NotAllowed)
    );
    // A span that starts inside and runs out.
    assert_eq!(p.plan(VGA.0, VGA.1, 2, 0x3fc, 8), Err(Denied::NotAllowed));
    // Unknown device, wrong BAR.
    assert_eq!(p.plan(0x1af4, 0x1000, 0, 0, 4), Err(Denied::NotAllowed));
    assert_eq!(p.plan(VGA.0, VGA.1, 1, 0, 4), Err(Denied::NotAllowed));
}

#[test]
fn gmbus_data_and_the_eld_port_are_refused() {
    let p = on();
    // PCH_GMBUS0..5 and the span around the data register.
    for reg in [
        0xC5100u64, 0xC5104, 0xC5108, 0xC510C, 0xC5110, 0xC5120, 0xC512C,
    ] {
        assert_eq!(
            p.plan(HD620.0, HD620.1, 0, reg, 4),
            Err(Denied::NotAllowed),
            "{reg:#x}"
        );
    }
    assert!(p.plan(HD620.0, HD620.1, 0, 0xC50FC, 4).is_ok());
    assert!(p.plan(HD620.0, HD620.1, 0, 0xC5130, 4).is_ok());
    // A request that would cross GMBUS is refused whole.
    assert_eq!(
        p.plan(HD620.0, HD620.1, 0, 0xC50F0, 0x40),
        Err(Denied::NotAllowed)
    );
    // The ELD data port (HSW_AUD_EDID_DATA_A).
    assert_eq!(
        p.plan(HD620.0, HD620.1, 0, 0x65050, 4),
        Err(Denied::NotAllowed)
    );
    assert!(p.plan(HD620.0, HD620.1, 0, 0x65000, 0x50).is_ok());
    // A display register and a pipe register are reachable.
    assert!(p.plan(HD620.0, HD620.1, 0, 0x70008, 4).is_ok());
    assert!(p.plan(HD620.0, HD620.1, 0, 0x60000, 4).is_ok());
}

#[test]
fn a_request_spans_adjacent_ranges() {
    let p = on();
    // 0x64ff0..0x65050 is one range; 0x65058.. the next: not adjacent.
    let across = p.plan(HD620.0, HD620.1, 0, 0x65048, 0x20);
    assert_eq!(across, Err(Denied::NotAllowed));
    let cfg = "diag.dbg=1\ndiag.dbg.mmio=1\ndiag.dbg.mmio.allow=8086:5916:0:0x65050-0x65058\n";
    let joined = Policy::from_cfg(cfg);
    let plan = joined.plan(HD620.0, HD620.1, 0, 0x65048, 0x20).unwrap();
    assert_eq!(plan.len(), 3);
    assert_eq!(plan.iter().map(|s| s.len).sum::<u64>(), 0x20);
}

#[test]
fn alignment_length_and_bar_bounds() {
    let p = on();
    assert_eq!(p.plan(VGA.0, VGA.1, 2, 0x501, 4), Err(Denied::Alignment));
    assert_eq!(p.plan(VGA.0, VGA.1, 2, 0x500, 3), Err(Denied::Alignment));
    assert_eq!(p.plan(VGA.0, VGA.1, 2, 0x500, 0), Err(Denied::Length));
    assert_eq!(
        p.plan(VGA.0, VGA.1, 0, 0, MAX_MMIO_BYTES + 4),
        Err(Denied::Length)
    );
    assert!(p.plan(VGA.0, VGA.1, 0, 0, MAX_MMIO_BYTES).is_ok());
    assert_eq!(p.plan(VGA.0, VGA.1, 6, 0, 4), Err(Denied::Bar));
    assert_eq!(
        p.plan(VGA.0, VGA.1, 0, u64::MAX - 3, 8),
        Err(Denied::Length)
    );
    assert_eq!(
        p.plan(VGA.0, VGA.1, 0, u64::MAX & !3, 4),
        Err(Denied::Length)
    );
}

#[test]
fn config_reads_stay_in_the_first_256_bytes() {
    assert!(check_config(0, 256).is_ok());
    assert!(check_config(255, 1).is_ok());
    assert!(check_config(3, 5).is_ok());
    assert_eq!(check_config(0, 0), Err(Denied::Length));
    assert_eq!(check_config(0, 257), Err(Denied::Length));
    assert_eq!(check_config(256, 1), Err(Denied::Length));
    assert_eq!(check_config(u64::MAX, 2), Err(Denied::Length));
}

#[test]
fn bdf_spellings() {
    assert_eq!(parse_bdf("00:02.0"), Some(0x0010));
    assert_eq!(parse_bdf("0000:00:02.0"), Some(0x0010));
    assert_eq!(parse_bdf("ff:1f.7"), Some(0xFFFF));
    assert_eq!(parse_bdf("0:2.0"), Some(0x0010));
    assert_eq!(format_bdf(0x0010), "00:02.0");
    for bad in [
        "",
        "00:02",
        "00:20.0",
        "00:02.8",
        "000:02.0",
        "0001:00:02.0",
        "gg:02.0",
        "00:02.00",
        "00.02:0",
        "1:2:3:4.0",
        "00:-1.0",
    ] {
        // "00:20.0" is device 0x20 = 32: out of range.
        assert_eq!(parse_bdf(bad), None, "{bad:?}");
    }
}

#[test]
fn allow_lines_are_parsed_and_bounded() {
    let r = parse_range("1af4:1050:0:0x0-0x1000").unwrap();
    assert_eq!(
        (r.vendor, r.device, r.bar, r.start, r.end),
        (0x1af4, 0x1050, 0, 0, 0x1000)
    );
    for bad in [
        "1af4:1050:0:0x1000-0x0",
        "1af4:1050:0:1-4",
        "1af4:1050:0:0-5",
        "1af4:1050:6:0-4",
        "1af4:1050:0:0-0x8000000",
        "1af4:1050:0",
        "1af4:1050:0:0-4:9",
        "zzzz:1050:0:0-4",
        "10000:1050:0:0-4",
    ] {
        assert_eq!(parse_range(bad), None, "{bad:?}");
    }
    let mut cfg = alloc::string::String::from("diag.dbg=1\ndiag.dbg.mmio=1\n");
    for i in 0..MAX_EXTRA + 3 {
        cfg.push_str(&alloc::format!(
            "diag.dbg.mmio.allow=1af4:1050:0:{:x}-{:x}\n",
            i * 8,
            i * 8 + 4
        ));
    }
    cfg.push_str("diag.dbg.mmio.allow=garbage\n");
    let p = Policy::from_cfg(&cfg);
    assert_eq!(p.extra().len(), MAX_EXTRA);
    assert_eq!(p.rejected, 4);
    // Extra ranges do not apply without the opt-in.
    let off = Policy::from_cfg("diag.dbg=1\ndiag.dbg.mmio.allow=1af4:1050:0:0-4\n");
    assert!(off.extra().is_empty() && !off.mmio);
}

#[test]
fn seeded_requests_never_escape_a_range() {
    fuzzkit::for_seeds("pciprobe_plan", |_, rng| {
        let mut data: Vec<u8> = (0..64).map(|_| rng.byte()).collect();
        // Half the seeds ask about a built-in device and a plausible BAR.
        if rng.next_u32() & 1 == 0 {
            let r = RANGES[(rng.next_u32() as usize) % RANGES.len()];
            data[0..2].copy_from_slice(&r.vendor.to_le_bytes());
            data[2..4].copy_from_slice(&r.device.to_le_bytes());
            data[8..16]
                .copy_from_slice(&(r.start + u64::from(rng.next_u32() % 64) * 4).to_le_bytes());
            data[16..24].copy_from_slice(&(u64::from(rng.next_u32() % 64) * 4).to_le_bytes());
            data[40] = r.bar;
        }
        crate::fuzz::run(&data);
    });
}

#[test]
fn a_range_value_checks() {
    let r = Range {
        vendor: 1,
        device: 2,
        bar: 0,
        start: 0,
        end: 4,
    };
    assert!(r.valid());
    assert!(!Range { end: 0, ..r }.valid());
}
