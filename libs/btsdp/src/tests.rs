//! Golden bytes for the request, a server written here from the specification
//! (so the client is not checked against its own idea of the encoding),
//! continuations, and hostile answers.

use std::vec;
use std::vec::Vec;

use crate::elem::{self, Kind};
use crate::record::{parse_attribute_lists, subclass};
use crate::{Client, Error, Progress, MAX_ATTRIBUTE_BYTES, MAX_DESCRIPTOR, MAX_ROUNDS};

// ---- a tiny encoder ---------------------------------------------------------

fn des(body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x36];
    v.extend_from_slice(&(body.len() as u16).to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn u16e(v: u16) -> Vec<u8> {
    let b = v.to_be_bytes();
    vec![0x09, b[0], b[1]]
}

fn u8e(v: u8) -> Vec<u8> {
    vec![0x08, v]
}

fn boole(v: bool) -> Vec<u8> {
    vec![0x28, u8::from(v)]
}

fn text(t: &[u8]) -> Vec<u8> {
    let mut v = vec![0x26];
    v.extend_from_slice(&(t.len() as u16).to_be_bytes());
    v.extend_from_slice(t);
    v
}

fn pair(id: u16, value: Vec<u8>) -> Vec<u8> {
    let mut v = u16e(id);
    v.extend(value);
    v
}

const DESCRIPTOR: &[u8] = &[0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0xC0];

fn class_list(uuid: u16) -> Vec<u8> {
    des(&[0x19, (uuid >> 8) as u8, uuid as u8])
}

fn descriptor_list(descriptor: &[u8]) -> Vec<u8> {
    let mut entry = u8e(0x22);
    entry.extend(text(descriptor));
    des(&des(&entry))
}

fn record(class: u16, descriptor: &[u8], flags: (bool, bool)) -> Vec<u8> {
    let mut body = pair(0x0001, class_list(class));
    body.extend(pair(0x0100, text(b"Test Keyboard")));
    body.extend(pair(0x0202, u8e(0x40)));
    body.extend(pair(0x0205, boole(flags.0)));
    body.extend(pair(0x0206, descriptor_list(descriptor)));
    body.extend(pair(0x020D, boole(flags.1)));
    body.extend(pair(0x020E, boole(true)));
    des(&body)
}

fn lists(records: &[Vec<u8>]) -> Vec<u8> {
    des(&records.concat())
}

fn response(tid: u16, data: &[u8], continuation: &[u8]) -> Vec<u8> {
    let mut params = (data.len() as u16).to_be_bytes().to_vec();
    params.extend_from_slice(data);
    params.push(continuation.len() as u8);
    params.extend_from_slice(continuation);
    let mut pdu = vec![0x07];
    pdu.extend_from_slice(&tid.to_be_bytes());
    pdu.extend_from_slice(&(params.len() as u16).to_be_bytes());
    pdu.extend_from_slice(&params);
    pdu
}

fn tid_of(request: &[u8]) -> u16 {
    u16::from_be_bytes([request[1], request[2]])
}

// ---- tests ------------------------------------------------------------------

#[test]
fn the_first_request_is_the_hid_search_with_no_continuation() {
    let mut client = Client::new();
    let request = client.start();
    assert_eq!(
        request,
        [
            0x06, 0x00, 0x01, 0x00, 0x15, // ServiceSearchAttributeRequest, tid 1, 21 octets
            0x35, 0x03, 0x19, 0x11, 0x24, // pattern: UUID 0x1124
            0x02, 0x00, // at most 512 octets
            0x35, 0x0B, 0x09, 0x00, 0x01, 0x09, 0x01, 0x00, 0x0A, 0x02, 0x00, 0x02, 0x0E,
            0x00, // no continuation
        ]
    );
    assert!(client.awaiting());
}

#[test]
fn one_response_gives_the_record() {
    let mut client = Client::new();
    let tid = tid_of(&client.start());
    let data = lists(&[record(0x1124, DESCRIPTOR, (false, true))]);
    let Progress::Done(rec) = client.on_pdu(&response(tid, &data, &[])).unwrap() else {
        panic!("done")
    };
    assert_eq!(rec.descriptor, DESCRIPTOR);
    assert_eq!(rec.subclass, subclass::KEYBOARD);
    assert!(!rec.reconnect_initiate && rec.normally_connectable && rec.boot_device);
    assert_eq!(rec.name, "Test Keyboard");
    assert!(!client.awaiting());
}

#[test]
fn a_record_split_in_pieces_is_followed_by_continuations() {
    let data = lists(&[record(0x1124, DESCRIPTOR, (true, false))]);
    let mut client = Client::new();
    let mut request = client.start();
    let mut sent = 0;
    let mut round = 0u8;
    loop {
        let tid = tid_of(&request);
        let end = (sent + 17).min(data.len());
        let state: &[u8] = if end < data.len() {
            &[end as u8, 9]
        } else {
            &[]
        };
        let answer = response(tid, &data[sent..end], state);
        sent = end;
        round += 1;
        match client.on_pdu(&answer).unwrap() {
            Progress::Request(next) => {
                // The state goes back verbatim as the request's continuation.
                assert!(next.ends_with(&[2, sent as u8, 9]), "{next:02x?}");
                request = next;
            }
            Progress::Done(rec) => {
                assert_eq!(rec.descriptor, DESCRIPTOR);
                assert!(round > 3, "it really was split: {round}");
                break;
            }
        }
    }
}

#[test]
fn an_error_response_ends_the_search() {
    let mut client = Client::new();
    let tid = tid_of(&client.start());
    let mut pdu = vec![0x01];
    pdu.extend_from_slice(&tid.to_be_bytes());
    pdu.extend_from_slice(&[0, 2, 0, 3]);
    assert_eq!(client.on_pdu(&pdu), Err(Error::Remote(3)));
    assert!(!client.awaiting());
}

#[test]
fn stale_unsolicited_and_mismatched_pdus_are_refused() {
    let mut client = Client::new();
    let data = lists(&[record(0x1124, DESCRIPTOR, (true, true))]);
    // Nothing asked yet.
    assert_eq!(
        client.on_pdu(&response(1, &data, &[])),
        Err(Error::BadTransaction)
    );
    let tid = tid_of(&client.start());
    assert_eq!(
        client.on_pdu(&response(tid.wrapping_add(1), &data, &[])),
        Err(Error::BadTransaction)
    );
    // Still waiting for the right one after a wrong id.
    let mut client = Client::new();
    let tid = tid_of(&client.start());
    let mut bad_len = response(tid, &data, &[]);
    bad_len[4] = bad_len[4].wrapping_add(1);
    assert_eq!(client.on_pdu(&bad_len), Err(Error::BadPdu));
    assert_eq!(client.on_pdu(&[]), Err(Error::Truncated));
    assert_eq!(client.on_pdu(&[7, 0, 1]), Err(Error::Truncated));
}

#[test]
fn counts_that_disagree_with_the_pdu_are_refused() {
    for tail in [
        // Attribute byte count past the PDU.
        vec![0x00, 0x20, 0x00],
        // No room for the continuation octet.
        vec![0x00, 0x00],
        // Continuation length longer than what is present.
        vec![0x00, 0x00, 0x05, 0x01],
        // Continuation state past sixteen octets.
        [vec![0x00, 0x00, 0x11], vec![1; 17]].concat(),
    ] {
        let mut client = Client::new();
        let tid = tid_of(&client.start());
        let mut pdu = vec![0x07];
        pdu.extend_from_slice(&tid.to_be_bytes());
        pdu.extend_from_slice(&(tail.len() as u16).to_be_bytes());
        pdu.extend_from_slice(&tail);
        assert!(client.on_pdu(&pdu).is_err(), "{tail:02x?}");
    }
}

#[test]
fn a_continuation_without_data_makes_no_progress() {
    let mut client = Client::new();
    let tid = tid_of(&client.start());
    assert_eq!(
        client.on_pdu(&response(tid, &[], &[1])),
        Err(Error::NoProgress)
    );
}

#[test]
fn a_server_that_never_finishes_is_cut_off() {
    let mut client = Client::new();
    let mut request = client.start();
    let mut rounds = 0u32;
    let result = loop {
        rounds += 1;
        match client.on_pdu(&response(tid_of(&request), &[0x36], &[1])) {
            Ok(Progress::Request(next)) => request = next,
            other => break other,
        }
    };
    assert_eq!(result, Err(Error::TooManyRounds));
    assert_eq!(rounds, u32::from(MAX_ROUNDS));
}

#[test]
fn too_many_bytes_are_refused() {
    let mut client = Client::new();
    let mut request = client.start();
    let chunk = vec![0x36u8; 512];
    let result = loop {
        match client.on_pdu(&response(tid_of(&request), &chunk, &[1])) {
            Ok(Progress::Request(next)) => request = next,
            other => break other,
        }
    };
    assert_eq!(result, Err(Error::TooLarge));
    assert!(MAX_ATTRIBUTE_BYTES / 512 < usize::from(MAX_ROUNDS));
}

#[test]
fn a_record_of_the_wrong_class_or_without_a_descriptor_is_not_hid() {
    let wrong = lists(&[record(0x110A, DESCRIPTOR, (true, true))]);
    assert_eq!(parse_attribute_lists(&wrong), Err(Error::NoHidRecord));
    let mut body = pair(0x0001, class_list(0x1124));
    body.extend(pair(0x0202, u8e(0x40)));
    assert_eq!(
        parse_attribute_lists(&lists(&[des(&body)])),
        Err(Error::NoHidRecord)
    );
    // The usable record may come second.
    let two = lists(&[
        record(0x110A, DESCRIPTOR, (true, true)),
        record(0x1124, DESCRIPTOR, (true, true)),
    ]);
    assert!(parse_attribute_lists(&two).is_ok());
}

#[test]
fn an_oversized_descriptor_makes_the_record_unusable() {
    let big = vec![0x05; MAX_DESCRIPTOR + 1];
    let data = lists(&[record(0x1124, &big, (true, true))]);
    assert_eq!(parse_attribute_lists(&data), Err(Error::NoHidRecord));
    let edge = vec![0x05; MAX_DESCRIPTOR];
    let data = lists(&[record(0x1124, &edge, (true, true))]);
    assert_eq!(
        parse_attribute_lists(&data).unwrap().descriptor.len(),
        MAX_DESCRIPTOR
    );
}

#[test]
fn defaults_and_wrongly_typed_attributes_do_not_decide_anything() {
    // Reconnect initiate defaults to true; a wrongly typed flag keeps it.
    let mut body = pair(0x0001, class_list(0x1124));
    body.extend(pair(0x0206, descriptor_list(DESCRIPTOR)));
    let rec = parse_attribute_lists(&lists(&[des(&body)])).unwrap();
    assert!(rec.reconnect_initiate && !rec.normally_connectable && !rec.boot_device);
    body.extend(pair(0x0205, u8e(0)));
    let rec = parse_attribute_lists(&lists(&[des(&body)])).unwrap();
    assert!(rec.reconnect_initiate, "an integer is not a boolean");
}

#[test]
fn the_service_name_is_printable_ascii_and_short() {
    let mut name = b"ok\x1b[31m\xFF name".to_vec();
    name.extend(vec![b'x'; 200]);
    let mut body = pair(0x0001, class_list(0x1124));
    body.extend(pair(0x0100, text(&name)));
    body.extend(pair(0x0206, descriptor_list(DESCRIPTOR)));
    let rec = parse_attribute_lists(&lists(&[des(&body)])).unwrap();
    assert!(rec.name.starts_with("ok[31m name"));
    assert_eq!(rec.name.len(), crate::record::MAX_NAME);
    assert!(rec.name.bytes().all(|b| b.is_ascii_graphic() || b == b' '));
}

#[test]
fn malformed_elements_are_errors_not_panics() {
    let good = lists(&[record(0x1124, DESCRIPTOR, (true, true))]);
    // Every truncation either parses (never, here) or is refused.
    for cut in 0..good.len() {
        assert!(parse_attribute_lists(&good[..cut]).is_err(), "cut {cut}");
    }
    // Trailing bytes after the outer element.
    let mut extra = good.clone();
    extra.push(0);
    assert!(parse_attribute_lists(&extra).is_err());
    // A length that claims more than there is, in each size form.
    for bytes in [
        vec![0x35, 0x05, 0x00],
        vec![0x36, 0xFF, 0xFF, 0x00],
        vec![0x37, 0xFF, 0xFF, 0xFF, 0xFF, 0x00],
        vec![0x3D, 0x05, 0x00],
        vec![0x01],
        vec![0x07],
        vec![0xF8],
    ] {
        assert!(elem::parse(&bytes).is_err(), "{bytes:02x?}");
    }
    assert_eq!(elem::parse(&[0x00]).unwrap().0.kind, Kind::Nil);
}

#[test]
fn a_flood_of_tiny_elements_hits_the_budget() {
    // 4000 two-octet records are well within the size bound and far past the
    // element budget.
    let junk = [0x08u8, 0x01].repeat(4000);
    let flood = des(&junk);
    assert_eq!(
        parse_attribute_lists(&des(&flood)).err(),
        Some(Error::BadElement)
    );
}

#[test]
fn the_uuid_forms_are_read() {
    assert_eq!(
        elem::parse(&[0x19, 0x11, 0x24]).unwrap().0.uuid16(),
        Some(0x1124)
    );
    assert_eq!(
        elem::parse(&[0x1A, 0, 0, 0x11, 0x24]).unwrap().0.uuid16(),
        Some(0x1124)
    );
    assert_eq!(
        elem::parse(&[0x1A, 1, 0, 0x11, 0x24]).unwrap().0.uuid16(),
        None
    );
}

#[test]
fn seeded_fuzz_runs() {
    let valid = lists(&[record(0x1124, DESCRIPTOR, (true, true))]);
    let mut client = Client::new();
    let valid_response = response(tid_of(&client.start()), &valid, &[]);
    fuzzkit::for_seeds("btsdp_fuzz_run", |_seed, rng| {
        let mut data = match rng.below(3) {
            0 => valid.clone(),
            1 => valid_response.clone(),
            _ => {
                let n = rng.below(300) as usize;
                rng.bytes(n)
            }
        };
        let flips = rng.below(6) as usize;
        rng.flip_bits(&mut data, flips);
        if rng.one_in(3) {
            let n = rng.below(data.len() as u64 + 1) as usize;
            data.truncate(n);
        }
        if rng.one_in(2) {
            // A conversation script: the response cut into frames.
            let mut script = Vec::new();
            for chunk in data.chunks(1 + rng.below(40) as usize) {
                script.push(chunk.len() as u8);
                script.extend_from_slice(chunk);
            }
            data = script;
        }
        crate::fuzz::run(&data);
    });
}

fn seed(name: &str) -> Vec<u8> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/btsdp");
    std::fs::read(dir.join(name)).unwrap()
}

/// The checked-in seeds are well-formed answers: the record seeds parse, the
/// response seed completes a conversation.
#[test]
fn the_checked_in_seeds_mean_what_they_say() {
    let rec = parse_attribute_lists(&seed("lists_keyboard")).unwrap();
    assert_eq!(rec.subclass, subclass::KEYBOARD);
    let mut client = Client::new();
    let request = client.start();
    let mut pdu = seed("response_mouse");
    pdu[1..3].copy_from_slice(&request[1..3]);
    let Progress::Done(rec) = client.on_pdu(&pdu).unwrap() else {
        panic!("one response is the whole record")
    };
    assert_eq!(rec.subclass, subclass::POINTING);
    for name in ["empty", "script_continuation", "lists_truncated"] {
        crate::fuzz::run(&seed(name));
    }
}
