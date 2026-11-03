//! Synthetic epatch containers, and the selection rules over them.

use std::vec::Vec;

use crate::realtek::epatch;

pub(crate) const TAIL: [u8; 4] = [0x51, 0x04, 0xfd, 0x77];

/// A v1 file: one patch per `(chip id, content)`; `project` names the chip
/// family. Each stored patch ends in four placeholder octets that the loader
/// replaces with the file's version.
pub(crate) fn v1(patches: &[(u16, Vec<u8>)], fw_version: u32, project: u8) -> Vec<u8> {
    let n = patches.len();
    let mut out = b"Realtech".to_vec();
    out.extend_from_slice(&fw_version.to_le_bytes());
    out.extend_from_slice(&(n as u16).to_le_bytes());
    for (id, _) in patches {
        out.extend_from_slice(&id.to_le_bytes());
    }
    for (_, content) in patches {
        out.extend_from_slice(&((content.len() + 4) as u16).to_le_bytes());
    }
    let mut offset = 14 + 8 * n;
    for (_, content) in patches {
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += content.len() + 4;
    }
    for (_, content) in patches {
        out.extend_from_slice(content);
        out.extend_from_slice(&[0xEE; 4]);
    }
    out.extend_from_slice(&[project, 1, 0]);
    out.extend_from_slice(&TAIL);
    out
}

/// One v2 sub-section: ECO (ROM + 1), priority, `cb[0]` (the key id of a
/// signed one), data.
pub(crate) type Sub = (u8, u8, u8, Vec<u8>);

/// A v2 file from `(section opcode, sub-sections)`.
pub(crate) fn v2(sections: &[(u32, Vec<Sub>)], project: u8) -> Vec<u8> {
    let mut out = b"RTBTCore".to_vec();
    out.extend_from_slice(&[0x11; 8]);
    out.extend_from_slice(&(sections.len() as u32).to_le_bytes());
    for (opcode, subs) in sections {
        let mut body = (subs.len() as u16).to_le_bytes().to_vec();
        body.extend_from_slice(&[0, 0]);
        for (eco, prio, cb0, data) in subs {
            body.extend_from_slice(&[*eco, *prio, *cb0, 0]);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
        }
        out.extend_from_slice(&opcode.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
    }
    out.extend_from_slice(&[project, 1, 0]);
    out.extend_from_slice(&TAIL);
    out
}

pub(crate) fn sub(eco: u8, prio: u8, cb0: u8, data: &[u8]) -> Sub {
    (eco, prio, cb0, data.to_vec())
}

const LMP_8761: u16 = 0x8761;
const LMP_8822: u16 = 0x8822;

#[test]
fn v1_picks_the_patch_for_the_rom_and_appends_the_file_version() {
    let file = v1(
        &[
            (1, b"rom-zero-patch".to_vec()),
            (2, b"rom-one-patch!".to_vec()),
        ],
        0xDEADBEEF,
        3,
    );
    let mut want = b"rom-zero-patch".to_vec();
    want.extend_from_slice(&0xDEADBEEFu32.to_le_bytes());
    assert_eq!(epatch::build(&file, LMP_8761, 0, 0).unwrap(), want);
    let mut want = b"rom-one-patch!".to_vec();
    want.extend_from_slice(&0xDEADBEEFu32.to_le_bytes());
    assert_eq!(epatch::build(&file, LMP_8761, 1, 0).unwrap(), want);
    assert!(
        epatch::build(&file, LMP_8761, 2, 0).is_err(),
        "no patch for ROM 2"
    );
}

#[test]
fn a_file_for_another_chip_is_refused() {
    let file = v1(&[(1, b"x".to_vec())], 1, 3);
    assert!(
        epatch::build(&file, LMP_8822, 0, 0).is_err(),
        "project 3 is an RTL8761"
    );
    let unknown = v1(&[(1, b"x".to_vec())], 1, 200);
    assert!(epatch::build(&unknown, LMP_8761, 0, 0).is_err());
}

#[test]
fn v1_refuses_every_inconsistent_table() {
    let good = v1(&[(1, b"abcdefgh".to_vec())], 7, 3);
    assert!(epatch::build(&good, LMP_8761, 0, 0).is_ok());
    let patch = |f: &mut Vec<u8>, off: usize, bytes: &[u8]| {
        f[off..off + bytes.len()].copy_from_slice(bytes)
    };
    // tables: ids at 14, lengths at 16, offsets at 18.
    for (why, at, bytes) in [
        ("offset inside the header", 18, 0u32.to_le_bytes().to_vec()),
        (
            "offset past the end",
            18,
            0x00FF_FFFFu32.to_le_bytes().to_vec(),
        ),
        (
            "length shorter than the version",
            16,
            3u16.to_le_bytes().to_vec(),
        ),
        ("length past the end", 16, 0xFFFFu16.to_le_bytes().to_vec()),
        ("absurd patch count", 12, 0xFFFFu16.to_le_bytes().to_vec()),
    ] {
        let mut f = good.clone();
        patch(&mut f, at, &bytes);
        assert!(epatch::build(&f, LMP_8761, 0, 0).is_err(), "{why}");
    }
}

#[test]
fn the_tail_and_instructions_are_checked() {
    let good = v1(&[(1, b"abcdefgh".to_vec())], 7, 3);
    let mut bad_tail = good.clone();
    *bad_tail.last_mut().unwrap() ^= 1;
    assert!(epatch::build(&bad_tail, LMP_8761, 0, 0).is_err());
    let mut zero_len = good.clone();
    let n = zero_len.len();
    zero_len[n - 6] = 0; // the instruction's length octet
    assert!(epatch::build(&zero_len, LMP_8761, 0, 0).is_err());
    let mut eof = good.clone();
    eof[n - 5] = 0xff; // an EOF opcode before any project id
    assert!(epatch::build(&eof, LMP_8761, 0, 0).is_err());
    assert!(epatch::build(b"Realtech", LMP_8761, 0, 0).is_err());
    assert!(epatch::build(b"NotRealtekAtAll....................", LMP_8761, 0, 0).is_err());
    assert!(epatch::build(&[], LMP_8761, 0, 0).is_err());
}

#[test]
fn v2_joins_the_matching_subsections_in_priority_order() {
    let file = v2(
        &[
            (
                1,
                std::vec![
                    sub(1, 5, 0, b"[five]"),
                    sub(2, 1, 0, b"[other-rom]"),
                    sub(1, 2, 0, b"[two]"),
                ],
            ),
            (2, std::vec![sub(1, 9, 0, b"[nine]")]),
            (9, std::vec![sub(1, 0, 0, b"[unknown-section]")]),
        ],
        13,
    );
    assert_eq!(
        epatch::build(&file, LMP_8822, 0, 0).unwrap(),
        b"[two][five][nine]"
    );
    assert_eq!(
        epatch::build(&file, LMP_8822, 1, 0).unwrap(),
        b"[other-rom]"
    );
    assert!(
        epatch::build(&file, LMP_8822, 5, 0).is_err(),
        "nothing for ROM 5"
    );
    // The same priority: the later one goes first (the documented order).
    let tie = v2(
        &[(1, std::vec![sub(1, 4, 0, b"A"), sub(1, 4, 0, b"B")])],
        13,
    );
    assert_eq!(epatch::build(&tie, LMP_8822, 0, 0).unwrap(), b"BA");
}

#[test]
fn v2_signed_sections_apply_only_to_a_matching_key() {
    let file = v2(
        &[
            (1, std::vec![sub(1, 1, 0, b"plain")]),
            (3, std::vec![sub(1, 2, 7, b"+key7"), sub(1, 3, 8, b"+key8")]),
        ],
        13,
    );
    assert_eq!(
        epatch::build(&file, LMP_8822, 0, 0).unwrap(),
        b"plain",
        "no key: signed ignored"
    );
    assert_eq!(epatch::build(&file, LMP_8822, 0, 7).unwrap(), b"plain+key7");
    assert_eq!(epatch::build(&file, LMP_8822, 0, 9).unwrap(), b"plain");
}

#[test]
fn v2_survives_truncated_sections() {
    let good = v2(&[(1, std::vec![sub(1, 1, 0, b"abcdef")])], 13);
    assert_eq!(epatch::build(&good, LMP_8822, 0, 0).unwrap(), b"abcdef");
    // Cutting inside the sections leaves a valid tail but a short body: the
    // walk stops, it never reads past the end.
    for cut in 20..good.len() - 7 {
        let mut f = good[..cut].to_vec();
        f.extend_from_slice(&[13, 1, 0]);
        f.extend_from_slice(&TAIL);
        let _ = epatch::build(&f, LMP_8822, 0, 0);
    }
}
