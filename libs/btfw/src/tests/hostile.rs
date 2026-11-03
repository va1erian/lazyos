//! Hostile input: every container parser and answer parser refuses what is
//! malformed, and none of them panics on any prefix or single-byte mutation.

use std::vec::Vec;

use super::intel::{body, sfi};
use super::realtek_epatch::{sub, v1, v2};
use crate::intel::ddc;
use crate::intel::sfi::{Layout, Sfi};
use crate::intel::version::{parse_boot_params, parse_legacy, parse_tlv};
use crate::realtek::epatch;

fn rsa(data: Vec<u8>) -> Result<Sfi, &'static str> {
    Sfi::parse(data, Layout::Rsa)
}

fn both(data: Vec<u8>, sbe_type: u8) -> Result<Sfi, &'static str> {
    Sfi::parse(data, Layout::Both { sbe_type })
}

fn patched(mut data: Vec<u8>, at: usize, bytes: &[u8]) -> Vec<u8> {
    data[at..at + bytes.len()].copy_from_slice(bytes);
    data
}

#[test]
fn a_good_image_parses_and_its_groups_tile_the_command_stream() {
    let b = body((1, 2, 3));
    let parsed = rsa(sfi(false, &b)).unwrap();
    assert_eq!(parsed.boot_addr, 0x0010_0800);
    assert_eq!(parsed.version, Some((1, 2, 3)));
    let mut pos = parsed.body_start();
    let mut joined = Vec::new();
    while let Some(end) = parsed.next_group(pos) {
        assert_eq!((end - pos) % 4, 0);
        joined.extend_from_slice(parsed.bytes(pos..end));
        pos = end;
        if pos == parsed.body_start() + b.len() {
            break;
        }
    }
    assert_eq!(joined, b);
    assert!(both(sfi(true, &b), 0).is_ok());
    assert!(both(sfi(true, &b), 1).is_ok());
}

#[test]
fn a_malformed_image_is_refused_whole() {
    let b = body((1, 2, 3));
    let good = sfi(false, &b);
    let good_both = sfi(true, &b);
    let cases: Vec<(&str, Result<Sfi, &'static str>)> = std::vec![
        ("shorter than the header", rsa(good[..643].to_vec())),
        ("empty", rsa(Vec::new())),
        (
            "wrong CSS version",
            rsa(patched(good.clone(), 8, &[1, 2, 3, 4]))
        ),
        ("no commands", rsa(good[..644].to_vec())),
        ("command header cut", rsa(good[..good.len() - 1].to_vec())),
        (
            "command runs past the end",
            rsa(patched(good.clone(), good.len() - 2, &[250]))
        ),
        (
            "stream ends mid-group",
            rsa({
                let mut f = good.clone();
                f.extend_from_slice(&[0xfc, 0x8e, 0x00]);
                f
            })
        ),
        ("ecdsa block missing", both(good.clone(), 0)),
        (
            "ecdsa magic wrong",
            both(patched(good_both.clone(), 644, &[0x07]), 0)
        ),
        (
            "ecdsa CSS version wrong",
            both(patched(good_both.clone(), 652, &[9, 9, 9, 9]), 0)
        ),
        ("unknown engine type", both(good_both.clone(), 2)),
        ("ecdsa block cut", both(good_both[..900].to_vec(), 1)),
    ];
    for (why, result) in cases {
        assert!(result.is_err(), "{why}");
    }
}

#[test]
fn no_prefix_or_single_byte_change_of_an_image_panics() {
    let b = body((1, 2, 3));
    for (file, layout) in [
        (sfi(false, &b), Layout::Rsa),
        (sfi(true, &b), Layout::Both { sbe_type: 0 }),
        (sfi(true, &b), Layout::Both { sbe_type: 1 }),
    ] {
        // Inside the signed blocks a prefix never parses; inside the command
        // stream one that ends on a group boundary is a shorter valid image.
        let blocks = if matches!(layout, Layout::Rsa) {
            644
        } else {
            964
        };
        for cut in 0..file.len() {
            let r = Sfi::parse(file[..cut].to_vec(), layout);
            if cut <= blocks {
                assert!(r.is_err(), "a prefix of {cut} octets must not parse");
            }
        }
        for at in (0..file.len()).step_by(3) {
            let mut f = file.clone();
            f[at] ^= 0xff;
            if let Ok(parsed) = Sfi::parse(f, layout) {
                // Whatever parsed can be walked to the end without panicking.
                let mut pos = parsed.body_start();
                while let Some(end) = parsed.next_group(pos) {
                    pos = end;
                }
            }
        }
    }
}

fn good_tlv() -> Vec<u8> {
    super::intel::tlv_version(0x19, 1, 1, (0x410, 0x410))[1..].to_vec()
}

#[test]
fn the_tlv_version_list_is_parsed_strictly() {
    let tlv = parse_tlv(&good_tlv()).unwrap();
    assert_eq!(tlv.hw_variant(), 0x19);
    assert_eq!(tlv.hw_platform(), 0x37);
    assert_eq!((tlv.img_type, tlv.sbe_type), (1, 1));
    let all = good_tlv();
    for cut in 0..all.len() {
        // Cuts between the optional trailing entries stay valid; the rest
        // must be refused. Either way, no panic.
        let _ = parse_tlv(&all[..cut]);
    }
    assert!(parse_tlv(&all[..3]).is_err());
    assert!(parse_tlv(&[]).is_err(), "required entries absent");
    assert!(parse_tlv(&[0x10]).is_err(), "header cut");
    assert!(
        parse_tlv(&[0x10, 4, 1, 2]).is_err(),
        "value runs past the end"
    );
    assert!(
        parse_tlv(&[0x10, 2, 1, 2]).is_err(),
        "32-bit entry with 2 octets"
    );
    assert!(parse_tlv(&[0x12, 0]).is_err(), "empty entry");
    let mut short_min = all.clone();
    short_min.extend_from_slice(&[0x2d, 2, 1, 1]);
    assert!(
        parse_tlv(&short_min).is_err(),
        "minimum-firmware entry of 2"
    );
}

#[test]
fn the_fixed_records_are_length_checked() {
    let legacy = super::intel::legacy_version(0x13, 6, (1, 2, 3));
    assert!(parse_legacy(&legacy).is_some());
    for len in 0..legacy.len() {
        assert!(parse_legacy(&legacy[..len]).is_none(), "{len} octets");
    }
    let mut longer = legacy.clone();
    longer.push(0);
    assert!(
        parse_legacy(&longer).is_none(),
        "a longer answer is a TLV one"
    );
    let mut other_platform = legacy;
    other_platform[1] = 0x38;
    assert!(parse_legacy(&other_platform).is_none());

    let params = super::intel::boot_params(0x1234, 0);
    assert_eq!(
        parse_boot_params(&params).map(|p| p.dev_revid),
        Some(0x1234)
    );
    for len in 0..params.len() {
        assert!(parse_boot_params(&params[..len]).is_none());
    }
}

#[test]
fn ddc_records_are_validated_before_anything_is_written() {
    assert_eq!(ddc::validate(&super::intel::DDC), Ok(2));
    assert_eq!(ddc::validate(&[]), Ok(0));
    for bad in [
        &[0u8][..],             // a record with no id
        &[1, 9],                // too short for an id
        &[3, 1, 2],             // runs past the end
        &[255, 0],              // longer than one command carries
        &[3, 1, 2, 3, 4, 3, 1], // second record cut
    ] {
        assert!(ddc::validate(bad).is_err(), "{bad:?}");
    }
    let mut big = std::vec![254u8];
    big.extend(std::iter::repeat_n(0, 254));
    assert_eq!(
        ddc::validate(&big),
        Ok(1),
        "the longest record that fits a command"
    );
}

#[test]
fn no_prefix_or_single_byte_change_of_an_epatch_file_panics() {
    let files = [
        v1(&[(1, std::vec![1; 40]), (2, std::vec![2; 30])], 5, 3),
        v2(
            &[
                (1, std::vec![sub(1, 1, 0, b"abc"), sub(2, 2, 0, b"de")]),
                (3, std::vec![sub(1, 3, 9, b"signed")]),
            ],
            13,
        ),
    ];
    let lmps = [0x8761, 0x8822];
    for (file, lmp) in files.iter().zip(lmps) {
        for cut in 0..file.len() {
            for rom in [0u8, 1, 255] {
                let _ = epatch::build(&file[..cut], lmp, rom, 0);
            }
        }
        for at in 0..file.len() {
            let mut f = file.clone();
            f[at] ^= 0xff;
            for key in [0u8, 9] {
                let _ = epatch::build(&f, lmp, 0, key);
                let _ = epatch::build(&f, lmp, 1, key);
            }
        }
        assert!(epatch::build(file, lmp, 0, 0).is_ok());
    }
}

#[test]
fn a_loader_survives_arbitrary_event_bytes_in_every_state() {
    // The same entry point the fuzz target uses, on a spread of scripted
    // inputs including the seeds-shaped ones.
    let mut seed = 0x1234_5678_9abc_def0u64;
    for round in 0..300 {
        let mut data = Vec::new();
        let len = 20 + round % 300;
        for _ in 0..len {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            data.push((seed >> 24) as u8);
        }
        crate::fuzz::run(&data);
    }
}
