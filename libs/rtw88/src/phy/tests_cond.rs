//! Table conditions, delay pseudo-addresses, RF access and the load order.

use std::vec::Vec;

use super::cond::*;
use super::rf::*;
use crate::dev::{Dev, RF_PATH_A, RF_PATH_B};
use crate::fake::{Access, FakeHci};
use crate::hci::Hci;
use crate::rtw8822c::tables;

const IF: u32 = 0x8000_0000;
const ELIF: u32 = 0x9000_0000;
const ELSE: u32 = 0xa000_0000;
const ENDIF: u32 = 0xb000_0000;
const NEG: u32 = 0x4000_0000;

fn header(branch: u32, cut: u32, rfe: u32) -> [u32; 4] {
    [branch | (cut << 24) | rfe, 0, NEG, 0]
}

fn run(drv: &PhyCond, words: &[u32]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    parse_phy_cond(drv, words, |a, d| out.push((a, d)));
    out
}

/// if (cut 3, rfe 1) {0x10} elif (rfe 2) {0x20} else {0x30} endif, then 0x40.
fn chain() -> Vec<u32> {
    let mut t = std::vec![0x8, 0x1];
    t.extend(header(IF, 3, 1));
    t.extend([0x10, 0xa]);
    t.extend(header(ELIF, 0, 2));
    t.extend([0x20, 0xb]);
    t.extend([ELSE, 0]);
    t.extend([0x30, 0xc]);
    t.extend([ENDIF, 0]);
    t.extend([0x40, 0xd]);
    t
}

#[test]
fn setup_maps_cut_a_and_no_package_to_15() {
    let c = PhyCond::setup(0, 0, 5);
    assert_eq!(
        (c.cut, c.pkg, c.plat, c.rfe, c.intf),
        (15, 15, 4, 5, INTF_PCIE)
    );
    let c = PhyCond::setup(3, 2, 1);
    assert_eq!((c.cut, c.pkg), (3, 2));
}

#[test]
fn check_positive_zero_means_any_but_rfe_is_exact() {
    let drv = PhyCond::setup(3, 0, 1);
    let cond = |cut, pkg, intf, rfe| PhyCond {
        cut,
        pkg,
        intf,
        rfe,
        plat: 0,
    };
    assert!(check_positive(&drv, &cond(0, 0, 0, 1)));
    assert!(check_positive(&drv, &cond(3, 15, 1, 1)));
    assert!(!check_positive(&drv, &cond(4, 0, 0, 1)));
    assert!(!check_positive(&drv, &cond(0, 2, 0, 1)));
    assert!(!check_positive(&drv, &cond(0, 0, 2, 1)));
    assert!(
        !check_positive(&drv, &cond(0, 0, 0, 0)),
        "rfe 0 in a header is not a wildcard"
    );
}

#[test]
fn positive_negative_and_else_branches() {
    let t = chain();
    let first = run(&PhyCond::setup(3, 0, 1), &t);
    assert_eq!(first, [(0x8, 0x1), (0x10, 0xa), (0x40, 0xd)]);
    // Cut 4 fails the IF; the ELIF has no cut, so rfe 2 matches on any cut.
    let second = run(&PhyCond::setup(4, 0, 2), &t);
    assert_eq!(second, [(0x8, 0x1), (0x20, 0xb), (0x40, 0xd)]);
    let neither = run(&PhyCond::setup(3, 0, 5), &t);
    assert_eq!(neither, [(0x8, 0x1), (0x30, 0xc), (0x40, 0xd)]);
    // Rfe 1 on cut A: the IF names cut 3, so the ELSE applies.
    let cut_a = run(&PhyCond::setup(0, 0, 1), &t);
    assert_eq!(cut_a, [(0x8, 0x1), (0x30, 0xc), (0x40, 0xd)]);
}

#[test]
fn first_match_wins_and_a_trailing_odd_word_is_ignored() {
    let mut t = std::vec![];
    t.extend(header(IF, 0, 1));
    t.extend([0x10, 1]);
    t.extend(header(ELIF, 3, 1)); // would match too
    t.extend([0x20, 2]);
    t.extend([ENDIF, 0, 0x50]);
    assert_eq!(run(&PhyCond::setup(3, 0, 1), &t), [(0x10, 1)]);
}

fn digest(pairs: &[(u32, u32)]) -> u32 {
    pairs.iter().fold(0u32, |h, &(a, d)| {
        h.wrapping_mul(31)
            .wrapping_add(a)
            .wrapping_mul(31)
            .wrapping_add(d)
    })
}

/// Counts and digests from an independent Python model of upstream's
/// parser run over the C source (cut 0 is cut A, i.e. 15).
/// (table, words, cut, rfe, writes, digest)
type Case = (&'static str, &'static [u32], u8, u8, usize, u32);

#[test]
fn real_tables_match_the_reference_model() {
    let cases: &[Case] = &[
        ("agc", tables::AGC, 3, 1, 450, 0x4a32_7f00),
        ("agc", tables::AGC, 3, 0x15, 450, 0x0c65_0ade),
        ("bb", tables::BB, 4, 5, 1289, 0x6730_b030),
        ("bb", tables::BB, 4, 0x15, 1289, 0x00c6_76f2),
        ("rf_a", tables::RF_A, 0, 0, 789, 0x7a9c_84ab),
        ("rf_a", tables::RF_A, 1, 1, 789, 0x81c0_c6e4),
        ("rf_a", tables::RF_A, 3, 2, 789, 0xf63f_8b43),
        ("rf_a", tables::RF_A, 5, 0x15, 789, 0x14a3_38e4),
        ("rf_a", tables::RF_A, 4, 0, 789, 0x7a9c_84ab),
        ("rf_b", tables::RF_B, 1, 2, 697, 0xb4d1_b00a),
        ("rf_b", tables::RF_B, 4, 6, 697, 0xd945_8a91),
        ("rf_b", tables::RF_B, 3, 0x15, 697, 0x321e_fb3e),
        ("rf_b", tables::RF_B, 0, 5, 697, 0xecbc_5bd3),
    ];
    for &(name, words, cut, rfe, n, d) in cases {
        let got = run(&PhyCond::setup(cut, 0, rfe), words);
        assert_eq!(
            (got.len(), digest(&got)),
            (n, d),
            "{name} cut {cut} rfe {rfe}"
        );
    }
}

fn dev() -> Dev<FakeHci> {
    let mut d = Dev::new(FakeHci::default());
    d.hal.rf_path_num = 2;
    d.hal.rf_phy_num = 2;
    d
}

#[test]
fn bb_and_rf_delay_addresses_only_wait() {
    let mut d = dev();
    for (a, us) in [
        (0xfe, 50_000),
        (0xfd, 5_000),
        (0xfc, 1_000),
        (0xfb, 50),
        (0xfa, 5),
        (0xf9, 1),
    ] {
        d.hci.log.clear();
        cfg_bb(&mut d.hci, a, 0x1234);
        assert_eq!(d.hci.log, [Access::Delay { us }], "bb {a:#x}");
    }
    d.hci.log.clear();
    cfg_bb(&mut d.hci, 0x800, 0x1234);
    assert_eq!(d.hci.writes(), [(4, 0x800, 0x1234)]);
    for (a, us) in [(0xffe, 50_000), (0xfe, 100)] {
        d.hci.log.clear();
        cfg_rf(&mut d, RF_PATH_A, a, 0x1234);
        assert_eq!(d.hci.log, [Access::Delay { us }], "rf {a:#x}");
    }
}

#[test]
fn rf_direct_write_and_read() {
    let mut d = dev();
    d.hci.write32(0x3c00 + 0x56 * 4, 0xfff0_0000 | 0x1_2345);
    d.hci.log.clear();
    assert!(write_rf(&mut d, RF_PATH_A, 0x56, 0x1f, 0x7));
    assert_eq!(
        d.hci.log,
        [
            Access::Read {
                width: 4,
                addr: 0x3d58,
                val: 0xfff1_2345
            },
            Access::Write {
                width: 4,
                addr: 0x3d58,
                val: 0xfff1_2347
            },
            Access::Delay { us: 1 },
        ]
    );
    assert_eq!(read_rf(&mut d, RF_PATH_A, 0x56, RFREG_MASK), 0x1_2347);
    assert_eq!(read_rf(&mut d, RF_PATH_A, 0x56, 0xf00), 0x3);
    // Path B goes to its own window; bits past the address byte are dropped.
    write_rf(&mut d, RF_PATH_B, 0x118, RFREG_MASK, 0xabcde);
    assert_eq!(d.hci.peek32(0x4c00 + 0x18 * 4) & RFREG_MASK, 0xabcde);
}

#[test]
fn rf_register_0_goes_over_sipi() {
    let mut d = dev();
    d.hci.log.clear();
    write_rf(&mut d, RF_PATH_A, 0, RFREG_MASK, 0x3_0000);
    assert_eq!(
        d.hci.log,
        [
            Access::Write {
                width: 4,
                addr: 0x1808,
                val: 0x3_0000
            },
            Access::Delay { us: 13 }
        ]
    );
    // A partial write reads the register through the direct window first.
    d.hci.write32(0x4c00, 0x1_0005);
    d.hci.log.clear();
    write_rf(&mut d, RF_PATH_B, 0, 0xf_0000, 0x2);
    assert_eq!(
        d.hci.log,
        [
            Access::Read {
                width: 4,
                addr: 0x4c00,
                val: 0x1_0005
            },
            Access::Write {
                width: 4,
                addr: 0x4108,
                val: 0x2_0005
            },
            Access::Delay { us: 13 },
        ]
    );
}

#[test]
fn a_missing_path_reads_invalid_and_writes_nothing() {
    let mut d = dev();
    d.hal.rf_phy_num = 1;
    d.hci.log.clear();
    assert_eq!(read_rf(&mut d, RF_PATH_B, 0x18, RFREG_MASK), INV_RF_DATA);
    assert!(!write_rf(&mut d, RF_PATH_B, 0x18, RFREG_MASK, 1));
    assert!(!write_rf(&mut d, RF_PATH_B, 0, RFREG_MASK, 1));
    assert!(!write_rf(&mut d, 7, 0x18, RFREG_MASK, 1));
    assert!(d.hci.log.is_empty());
}

#[test]
fn load_tables_order_and_rf_paths() {
    let mut d = dev();
    d.efuse.rfe_option = 1;
    d.phy.cond = PhyCond::setup(3, 0, 1);
    load_tables(&mut d);
    assert!(d.dm.dpk.is_dpk_pwr_on);
    let w = d.hci.writes();
    let first_bb = w.iter().position(|&(_, a, _)| a == 0x800).unwrap();
    let first_agc = w.iter().position(|&(_, a, _)| a == 0x1d90).unwrap();
    let first_cal = w.iter().position(|&(_, a, _)| a == 0x1b00).unwrap();
    let rf_b = |a: u32| (0x4c00..0x5000).contains(&a);
    let rf_a = |a: u32| (0x3c00..0x4000).contains(&a);
    let first_rf_b = w.iter().position(|&(_, a, _)| rf_b(a)).unwrap();
    let last_rf_b = w.iter().rposition(|&(_, a, _)| rf_b(a)).unwrap();
    let first_rf_a = w.iter().position(|&(_, a, _)| rf_a(a)).unwrap();
    assert!(first_bb < first_agc && first_agc < first_cal && first_cal < first_rf_b);
    assert!(
        last_rf_b < first_rf_a,
        "path B's table loads first (upstream rf_tbl order)"
    );
    // One write per RF table entry on each path (register 0 over SIPI).
    let reference = |words: &[u32]| {
        run(&d.phy.cond, words)
            .iter()
            .filter(|&&(a, _)| a != 0xfe && a != 0xffe)
            .count()
    };
    let sipi = |reg: u32| w.iter().filter(|&&(_, a, _)| a == reg).count();
    let count_b = w.iter().filter(|&&(_, a, _)| rf_b(a)).count() + sipi(0x4108);
    let count_a = w.iter().filter(|&&(_, a, _)| rf_a(a)).count() + sipi(0x1808);
    assert_eq!(count_b, reference(tables::RF_B));
    assert_eq!(count_a, reference(tables::RF_A));
}
