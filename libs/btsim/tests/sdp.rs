//! The simulator's SDP server, checked against hand-built request bytes.

use btsim::sdp::{self, hid_record, respond, Elem, HID_RECORD_HANDLE};
use btsim::HidKind;

fn record() -> Vec<(u16, Elem)> {
    hid_record(HidKind::Keyboard, true, false, "Kbd")
}

fn request(id: u8, tid: u16, params: &[u8]) -> Vec<u8> {
    let mut v = vec![id];
    v.extend_from_slice(&tid.to_be_bytes());
    v.extend_from_slice(&(params.len() as u16).to_be_bytes());
    v.extend_from_slice(params);
    v
}

const HID_PATTERN: [u8; 5] = [0x35, 0x03, 0x19, 0x11, 0x24];
const ALL_ATTRS: [u8; 7] = [0x35, 0x05, 0x0A, 0x00, 0x00, 0xFF, 0xFF];

fn search_attr(max: u16, cont: &[u8]) -> Vec<u8> {
    let mut p = HID_PATTERN.to_vec();
    p.extend_from_slice(&max.to_be_bytes());
    p.extend_from_slice(&ALL_ATTRS);
    p.push(cont.len() as u8);
    p.extend_from_slice(cont);
    request(0x06, 7, &p)
}

#[test]
fn service_search_returns_the_record_handle() {
    let mut p = HID_PATTERN.to_vec();
    p.extend_from_slice(&[0x00, 0x10, 0x00]);
    let r = respond(&request(0x02, 3, &p), &record(), 512);
    assert_eq!(r[0], 0x03);
    assert_eq!(&r[1..3], [0, 3], "the transaction id is echoed");
    assert_eq!(&r[5..9], [0, 1, 0, 1], "one match of one");
    assert_eq!(&r[9..13], HID_RECORD_HANDLE.to_be_bytes());
    // A pattern naming a different class finds nothing.
    let none = respond(
        &request(0x02, 3, &[0x35, 0x03, 0x19, 0x11, 0x05, 0x00, 0x10, 0x00]),
        &record(),
        512,
    );
    assert_eq!(&none[5..9], [0, 0, 0, 0]);
}

#[test]
fn service_attribute_returns_only_the_attributes_asked_for() {
    // Handle, max bytes, attribute list { 0x0205 (HIDReconnectInitiate) }, no continuation.
    let mut p = HID_RECORD_HANDLE.to_be_bytes().to_vec();
    p.extend_from_slice(&[0x01, 0x00, 0x35, 0x03, 0x09, 0x02, 0x05, 0x00]);
    let r = respond(&request(0x04, 9, &p), &record(), 512);
    assert_eq!(r[0], 0x05);
    assert_eq!(
        &r[7..],
        [0x36, 0x00, 0x05, 0x09, 0x02, 0x05, 0x28, 0x01, 0x00],
        "{r:02x?}"
    );
    // The wrong handle is an error with code 2.
    let mut bad = 0x1234_5678u32.to_be_bytes().to_vec();
    bad.extend_from_slice(&p[4..]);
    let r = respond(&request(0x04, 9, &bad), &record(), 512);
    assert_eq!((r[0], &r[5..7]), (0x01, &[0u8, 2][..]));
}

#[test]
fn search_attribute_response_holds_the_descriptor() {
    let r = respond(&search_attr(0xFFFF, &[]), &record(), 4096);
    assert_eq!(r[0], 0x07);
    let count = usize::from(u16::from_be_bytes([r[5], r[6]]));
    assert_eq!(r.len(), 7 + count + 1, "no continuation");
    let body = &r[7..7 + count];
    let desc = btsim::hid::KEYBOARD_DESCRIPTOR;
    assert!(body.windows(desc.len()).any(|w| w == desc));
    assert_eq!(&body[..1], [0x36], "a sequence of attribute lists");
}

#[test]
fn small_chunks_are_followed_by_continuations_that_reassemble_exactly() {
    let whole = respond(&search_attr(0xFFFF, &[]), &record(), 4096);
    let count = usize::from(u16::from_be_bytes([whole[5], whole[6]]));
    let expected = whole[7..7 + count].to_vec();
    for chunk in [1usize, 7, 64, 100] {
        let mut got = Vec::new();
        let mut cont: Vec<u8> = Vec::new();
        for _ in 0..expected.len() + 2 {
            let r = respond(&search_attr(0xFFFF, &cont), &record(), chunk);
            assert_eq!(r[0], 0x07, "chunk {chunk}: {r:02x?}");
            let n = usize::from(u16::from_be_bytes([r[5], r[6]]));
            assert!(n <= chunk);
            got.extend_from_slice(&r[7..7 + n]);
            cont = match &r[7 + n..] {
                [0] => break,
                [2, a, b] => vec![*a, *b],
                other => panic!("bad continuation {other:?}"),
            };
        }
        assert_eq!(got, expected, "chunk {chunk}");
    }
}

#[test]
fn the_requested_byte_count_caps_a_response() {
    let r = respond(&search_attr(10, &[]), &record(), 4096);
    assert_eq!(u16::from_be_bytes([r[5], r[6]]), 10);
    assert_eq!(&r[17..], [2, 0, 10]);
}

#[test]
fn bad_requests_get_an_error_pdu_never_a_panic() {
    // A continuation beyond the end.
    let r = respond(&search_attr(100, &[0xFF, 0xFF]), &record(), 512);
    assert_eq!((r[0], &r[5..7]), (0x01, &[0u8, 5][..]));
    // Unknown PDU id, empty, length mismatches.
    assert_eq!(respond(&request(0x55, 1, &[]), &record(), 512)[0], 0x01);
    assert_eq!(respond(&[], &record(), 512)[0], 0x01);
    let mut long = search_attr(100, &[]);
    long.push(0);
    assert_eq!(respond(&long, &record(), 512)[0], 0x01);
    // Every truncation of a valid request, and every single-byte corruption.
    let good = search_attr(0xFFFF, &[2, 0, 5]);
    for n in 0..good.len() {
        let r = respond(&good[..n], &record(), 512);
        assert_eq!(r[0], 0x01, "cut {n}");
    }
    for i in 0..good.len() {
        for v in [0x00, 0x01, 0x35, 0x36, 0x37, 0x19, 0x1C, 0x0A, 0xFF] {
            let mut b = good.clone();
            b[i] = v;
            let _ = respond(&b, &record(), 512);
        }
    }
}

#[test]
fn data_elements_encode_as_specified() {
    assert_eq!(Elem::U16(0x0100).to_bytes(), [0x09, 0x01, 0x00]);
    assert_eq!(
        Elem::Uuid(sdp::uuid16(0x1124)).to_bytes(),
        [0x19, 0x11, 0x24]
    );
    assert_eq!(Elem::Bool(true).to_bytes(), [0x28, 0x01]);
    assert_eq!(
        Elem::Text(b"ab".to_vec()).to_bytes(),
        [0x26, 0, 2, b'a', b'b']
    );
    assert_eq!(
        Elem::Seq(vec![Elem::U8(1)]).to_bytes(),
        [0x36, 0, 2, 0x08, 0x01]
    );
    let wide = Elem::Uuid(0x1234_5678_9ABC_DEF0_1234_5678_9ABC_DEF0);
    assert_eq!(wide.to_bytes().len(), 17);
}

#[test]
fn the_record_carries_the_reconnect_flags() {
    let find = |rec: &[(u16, Elem)], id| rec.iter().find(|(i, _)| *i == id).map(|(_, e)| e.clone());
    let a = hid_record(HidKind::Mouse, false, true, "m");
    assert_eq!(find(&a, 0x0205), Some(Elem::Bool(false)));
    assert_eq!(find(&a, 0x020D), Some(Elem::Bool(true)));
    assert_eq!(find(&a, 0x0202), Some(Elem::U8(0x80)));
    let ids: Vec<u16> = a.iter().map(|(i, _)| *i).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "attribute ids ascend");
}
