use alloc::vec::Vec;
use bthci::Boundary;

use crate::frag::{fragments, FragError, Reassembler};
use crate::pdu::encode_basic;

fn pdu(len: usize) -> Vec<u8> {
    let payload: Vec<u8> = (0..len).map(|i| i as u8).collect();
    encode_basic(0x0040, &payload).unwrap()
}

fn feed(
    r: &mut Reassembler,
    pieces: &[(Boundary, &[u8])],
) -> Vec<Result<Option<Vec<u8>>, FragError>> {
    pieces.iter().map(|(b, d)| r.push(*b, d)).collect()
}

/// The PDUs that came out, asserting nothing errored.
fn completed(out: &[Result<Option<Vec<u8>>, FragError>]) -> Vec<Vec<u8>> {
    out.iter()
        .filter_map(|o| o.clone().expect("no error"))
        .collect()
}

#[test]
fn a_whole_pdu_in_one_fragment() {
    let mut r = Reassembler::new(2048);
    let p = pdu(10);
    assert_eq!(r.push(Boundary::FirstFlushable, &p), Ok(Some(p)));
}

#[test]
fn every_two_way_split_reassembles() {
    let p = pdu(20);
    for cut in 0..=p.len() {
        let mut r = Reassembler::new(2048);
        let out = feed(
            &mut r,
            &[
                (Boundary::FirstNonFlushable, &p[..cut]),
                (Boundary::Continuing, &p[cut..]),
            ],
        );
        assert_eq!(completed(&out), std::slice::from_ref(&p), "cut {cut}");
    }
}

#[test]
fn every_three_way_split_reassembles() {
    let p = pdu(9);
    for a in 0..=p.len() {
        for b in a..=p.len() {
            let mut r = Reassembler::new(2048);
            let out = feed(
                &mut r,
                &[
                    (Boundary::FirstNonFlushable, &p[..a]),
                    (Boundary::Continuing, &p[a..b]),
                    (Boundary::Continuing, &p[b..]),
                ],
            );
            assert_eq!(completed(&out), std::slice::from_ref(&p), "cuts {a} {b}");
        }
    }
}

#[test]
fn byte_by_byte_and_with_the_fragmenter() {
    let p = pdu(100);
    let mut r = Reassembler::new(2048);
    let mut done = None;
    for (i, byte) in p.iter().enumerate() {
        let b = if i == 0 {
            Boundary::FirstFlushable
        } else {
            Boundary::Continuing
        };
        done = r.push(b, &[*byte]).unwrap();
    }
    assert_eq!(done, Some(p.clone()));
    for max in [1, 2, 3, 7, 27, 99, 100, 104, 200] {
        let mut r = Reassembler::new(2048);
        let mut got = None;
        for (b, piece) in fragments(&p, max) {
            assert!(piece.len() <= max);
            got = r.push(b, piece).unwrap();
        }
        assert_eq!(got, Some(p.clone()), "max {max}");
    }
}

#[test]
fn fragmenter_marks_only_the_first_as_a_start() {
    let p = pdu(20);
    let marks: Vec<_> = fragments(&p, 10).map(|(b, _)| b).collect();
    assert_eq!(
        marks,
        [
            Boundary::FirstNonFlushable,
            Boundary::Continuing,
            Boundary::Continuing
        ]
    );
    // A zero maximum cannot loop forever.
    assert_eq!(fragments(&p, 0).count(), p.len());
}

#[test]
fn an_oversize_pdu_is_refused_and_its_continuations_swallowed() {
    let mut r = Reassembler::new(64);
    let p = pdu(200);
    assert_eq!(
        r.push(Boundary::FirstFlushable, &p[..30]),
        Err(FragError::Oversize)
    );
    assert_eq!(r.push(Boundary::Continuing, &p[30..100]), Ok(None));
    assert_eq!(r.push(Boundary::Continuing, &p[100..]), Ok(None));
    // And the next PDU is fine.
    let ok = pdu(5);
    assert_eq!(r.push(Boundary::FirstFlushable, &ok), Ok(Some(ok)));
}

#[test]
fn the_oversize_header_split_across_fragments_is_still_caught() {
    let mut r = Reassembler::new(64);
    let p = pdu(500);
    assert_eq!(r.push(Boundary::FirstFlushable, &p[..2]), Ok(None));
    assert_eq!(
        r.push(Boundary::Continuing, &p[2..6]),
        Err(FragError::Oversize)
    );
    assert_eq!(r.push(Boundary::Continuing, &p[6..]), Ok(None));
    assert_eq!(
        r.push(Boundary::Continuing, &[1]),
        Err(FragError::UnexpectedContinuation)
    );
}

#[test]
fn a_fragment_beyond_the_announced_length_is_an_overrun() {
    let mut r = Reassembler::new(2048);
    let mut p = pdu(5);
    p.push(0xFF);
    assert_eq!(
        r.push(Boundary::FirstFlushable, &p),
        Err(FragError::Overrun)
    );
    let p = pdu(10);
    assert_eq!(r.push(Boundary::FirstFlushable, &p[..8]), Ok(None));
    assert_eq!(
        r.push(Boundary::Continuing, &[0; 20]),
        Err(FragError::Overrun)
    );
}

#[test]
fn continuation_without_start_and_reserved_boundary() {
    let mut r = Reassembler::new(2048);
    assert_eq!(
        r.push(Boundary::Continuing, &[1, 2]),
        Err(FragError::UnexpectedContinuation)
    );
    assert_eq!(
        r.push(Boundary::Reserved, &[1, 2]),
        Err(FragError::BadBoundary)
    );
}

#[test]
fn a_new_start_drops_the_unfinished_pdu() {
    let mut r = Reassembler::new(2048);
    let a = pdu(50);
    let b = pdu(6);
    assert_eq!(r.push(Boundary::FirstFlushable, &a[..20]), Ok(None));
    assert_eq!(r.push(Boundary::FirstFlushable, &b), Ok(Some(b)));
    assert_eq!(r.dropped_incomplete, 1);
}

#[test]
fn a_maximum_size_pdu_fits_exactly() {
    let mut r = Reassembler::new(100);
    let p = pdu(96);
    assert_eq!(r.push(Boundary::FirstFlushable, &p), Ok(Some(p)));
    let p = pdu(97);
    assert_eq!(
        r.push(Boundary::FirstFlushable, &p),
        Err(FragError::Oversize)
    );
}
