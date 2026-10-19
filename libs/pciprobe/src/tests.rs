use alloc::vec;
use alloc::vec::Vec;

use crate::builtin::RANGES;
use crate::{format_bdf, parse_bdf, parse_range, Denied, Policy, Range, MAX_EXTRA, MAX_MMIO_BYTES};

const HD620: (u16, u16) = (0x8086, 0x5916);
const VGA: (u16, u16) = (0x1234, 0x1111);
const ADLP: (u16, u16) = (0x8086, 0x46b3);

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
        Err(Denied::Disabled)
    );
    assert_eq!(
        Policy::config_only().plan(VGA.0, VGA.1, 2, 0x500, 4),
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
    // `diag.dbg=1` alone is the config header, and allow lines are ignored.
    let header = Policy::from_cfg("diag.dbg=1\ndiag.dbg.mmio.allow=1af4:1050:0:0-4\n");
    assert!(header.enabled && !header.mmio && header.extra().is_empty());
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
    // The ELD data ports (HSW_AUD_EDID_DATA_A, _B, _C): both dwords.
    for port in [0x65050u64, 0x65150, 0x65250] {
        for dword in [port, port + 4] {
            assert_eq!(
                p.plan(HD620.0, HD620.1, 0, dword, 4),
                Err(Denied::NotAllowed),
                "{dword:#x}"
            );
        }
        assert!(p.plan(HD620.0, HD620.1, 0, port - 4, 4).is_ok());
        assert!(p.plan(HD620.0, HD620.1, 0, port + 8, 4).is_ok());
    }
    assert!(p.plan(HD620.0, HD620.1, 0, 0x65000, 0x50).is_ok());
    // A display register and a pipe register are reachable.
    assert!(p.plan(HD620.0, HD620.1, 0, 0x70008, 4).is_ok());
    assert!(p.plan(HD620.0, HD620.1, 0, 0x60000, 4).is_ok());
}

#[test]
fn alder_lake_holes_are_refused_and_its_display_is_reachable() {
    let p = on();
    let reads = |offset: u64| p.plan(ADLP.0, ADLP.1, 0, offset, 4);
    // GMBUS, the four ELD ports, the palette data ports, the GT, HIP_INDEX.
    let mut holes = vec![0xC5100u64, 0xC510C, 0xC512C, 0x4A000, 0x49000, 0x4AFFC];
    for port in [0x65050u64, 0x65150, 0x65250, 0x65350] {
        holes.extend([port, port + 4]);
    }
    holes.extend([0x2000, 0xA188, 0xD84, 0x1010A0, 0x168000]);
    // Every scaler's and every plane's auto-incrementing index/data pairs,
    // pipes A to D.
    for pipe in 0..4u64 {
        for scaler in 0..2 {
            let base = 0x68000 + pipe * 0x800 + scaler * 0x100;
            holes.extend((0x198..0x1A8).step_by(4).map(|r| base + r));
        }
        for plane in 0..5 {
            let base = 0x70180 + pipe * 0x1000 + plane * 0x100;
            holes.extend((0x50..0x60).step_by(4).map(|r| base + r));
            holes.extend([base - 0x20, base - 0x1C]);
        }
    }
    for reg in holes {
        assert_eq!(reads(reg), Err(Denied::NotAllowed), "{reg:#x}");
    }
    // Power wells, CDCLK, transcoder A's DDI function, DDI B, pipe A's frame
    // counter, plane 1A's surface and its neighbours of the gamma pairs,
    // scaler 1A's control and window size, DPLL0, the first and last GGTT
    // entries.
    for reg in [
        0x45400u64, 0x46000, 0x60400, 0x64100, 0x70040, 0x7019C, 0x701CC, 0x701E0, 0x68180,
        0x68174, 0x681A8, 0x73580, 0x164284, 0x800000, 0xFFFFFC,
    ] {
        assert!(reads(reg).is_ok(), "{reg:#x}");
    }
}

#[test]
fn a_request_spans_adjacent_ranges() {
    let p = on();
    // 0x60000..0x65050 is one range and 0x65058..0x65150 the next: the ELD
    // port between them is a hole, not adjacent.
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
    assert!(p
        .plan(HD620.0, HD620.1, 0, 0x80_0000, MAX_MMIO_BYTES)
        .is_ok());
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
fn config_reads_need_diag_dbg_and_the_header_is_the_default() {
    assert_eq!(Policy::off().check_config(0, 4), Err(Denied::Disabled));
    let header = Policy::config_only();
    assert!(header.check_config(0, 64).is_ok());
    assert!(header.check_config(60, 4).is_ok());
    assert!(header.check_config(3, 5).is_ok());
    assert_eq!(header.check_config(0, 65), Err(Denied::ExtendedConfig));
    assert_eq!(header.check_config(0x40, 4), Err(Denied::ExtendedConfig));
    assert_eq!(header.check_config(0xFC, 4), Err(Denied::ExtendedConfig));
    let full = on();
    assert!(full.check_config(0, 256).is_ok());
    assert!(full.check_config(255, 1).is_ok());
    for policy in [&header, &full] {
        assert_eq!(policy.check_config(0, 0), Err(Denied::Length));
        assert_eq!(policy.check_config(0, 257), Err(Denied::Length));
        assert_eq!(policy.check_config(256, 1), Err(Denied::Length));
        assert_eq!(policy.check_config(u64::MAX, 2), Err(Denied::Length));
    }
    // `diag.dbg.mmio=1` without `diag.dbg=1` enables nothing, config too.
    assert_eq!(
        Policy::from_cfg("diag.dbg.mmio=1\n").check_config(0, 4),
        Err(Denied::Disabled)
    );
    assert!(Policy::from_cfg("diag.dbg=1\n").check_config(0, 64).is_ok());
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

#[test]
fn replay_checked_in_seeds() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
    for dir in ["seeds/pciprobe", "regressions/pciprobe"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            crate::fuzz::run(&std::fs::read(entry.path()).unwrap());
        }
    }
}
