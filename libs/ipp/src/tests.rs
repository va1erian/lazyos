use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::request::{self, Client, JobStatus, Marker, Ticket};
use crate::uri::PrinterUri;
use crate::{job_state, op, tag, Attribute, DecodeError, Group, Message, Value, MAX_DEPTH};

const CLIENT: Client = Client {
    printer_uri: "ipp://192.168.1.89:631/ipp/print",
    user: "alice",
};

/// The probe's request (tools/print/ipp_probe.py), byte for byte.
#[test]
fn get_printer_attributes_matches_the_probe() {
    let mut m = request::get_printer_attributes(&CLIENT, 1, &["all", "media-col-database"]);
    // The probe sends no user name; drop ours to compare.
    m.groups[0]
        .attributes
        .retain(|a| a.name != "requesting-user-name");
    let mut want = vec![2, 0, 0x00, 0x0B, 0, 0, 0, 1, tag::OPERATION];
    let mut attr = |t: u8, name: &str, value: &str| {
        want.push(t);
        want.extend_from_slice(&(name.len() as u16).to_be_bytes());
        want.extend_from_slice(name.as_bytes());
        want.extend_from_slice(&(value.len() as u16).to_be_bytes());
        want.extend_from_slice(value.as_bytes());
    };
    attr(tag::CHARSET, "attributes-charset", "utf-8");
    attr(tag::NATURAL_LANGUAGE, "attributes-natural-language", "en");
    attr(tag::URI, "printer-uri", CLIENT.printer_uri);
    attr(tag::KEYWORD, "requested-attributes", "all");
    attr(tag::KEYWORD, "", "media-col-database");
    want.push(tag::END);
    assert_eq!(m.encode().unwrap(), want);
}

/// A reply shaped like the DeskJet 3700's (docs/printing/deskjet3700-attributes.txt).
fn deskjet_reply() -> Message {
    let mut m = Message::new(0x0000, 1);
    let mut operation = Group::new(tag::OPERATION);
    operation.attributes.push(Attribute::new(
        "attributes-charset",
        Value::string(tag::CHARSET, "utf-8"),
    ));
    m.groups.push(operation);
    let mut printer = Group::new(tag::PRINTER);
    let p = &mut printer.attributes;
    p.push(Attribute::new(
        "printer-make-and-model",
        Value::string(tag::TEXT, "HP DeskJet 3700 series"),
    ));
    p.push(Attribute::new("printer-state", Value::Enum(3)));
    p.push(Attribute::new(
        "printer-state-message",
        Value::string(tag::TEXT, ""),
    ));
    p.push(Attribute::set(
        "pwg-raster-document-resolution-supported",
        vec![Value::Resolution {
            x: 300,
            y: 300,
            units: 3,
        }],
    ));
    p.push(Attribute::new(
        "copies-supported",
        Value::Range {
            lower: 1,
            upper: 99,
        },
    ));
    p.push(Attribute::new(
        "page-ranges-supported",
        Value::Boolean(true),
    ));
    p.push(Attribute::set(
        "marker-names",
        vec![
            Value::string(tag::NAME, "tri-color ink"),
            Value::string(tag::NAME, "black ink"),
        ],
    ));
    p.push(Attribute::set(
        "marker-levels",
        vec![Value::Integer(90), Value::Integer(50)],
    ));
    let size = |x: i32, y: i32| {
        Value::Collection(vec![
            Attribute::new("x-dimension", Value::Integer(x)),
            Attribute::new("y-dimension", Value::Integer(y)),
        ])
    };
    p.push(Attribute::new(
        "media-col-default",
        Value::Collection(vec![
            Attribute::new("media-size", size(21000, 29700)),
            Attribute::new("media-bottom-margin", Value::Integer(1270)),
            Attribute::new("media-source", Value::keyword("main")),
        ]),
    ));
    p.push(Attribute::new(
        "printer-current-time",
        Value::OutOfBand(tag::UNKNOWN),
    ));
    m.groups.push(printer);
    m
}

#[test]
fn a_printer_reply_round_trips_and_reads_back() {
    let m = deskjet_reply();
    let bytes = m.encode().unwrap();
    let (back, end) = Message::decode(&bytes).unwrap();
    assert_eq!(back, m);
    assert_eq!(end, bytes.len());
    assert!(back.is_success());
    assert_eq!(
        back.text(tag::PRINTER, "printer-make-and-model"),
        Some("HP DeskJet 3700 series")
    );
    assert_eq!(back.int(tag::PRINTER, "printer-state"), Some(3));
    let col = back
        .get(tag::PRINTER, "media-col-default")
        .unwrap()
        .first()
        .unwrap();
    let size = col.as_collection().unwrap()[0]
        .first()
        .unwrap()
        .as_collection()
        .unwrap();
    assert_eq!(size[1].first().unwrap().as_int(), Some(29700));
    assert_eq!(
        request::markers(&back),
        vec![
            Marker {
                name: String::from("tri-color ink"),
                level: Some(90)
            },
            Marker {
                name: String::from("black ink"),
                level: Some(50)
            },
        ]
    );
}

#[test]
fn a_print_job_carries_the_ticket_and_the_document_follows() {
    let ticket = Ticket {
        name: String::from("Letter to Bob"),
        format: String::from("image/pwg-raster"),
        copies: Some(2),
        media: Some(String::from("iso_a4_210x297mm")),
        color_mode: Some(String::from("monochrome")),
        quality: Some(request::quality::HIGH),
    };
    let m = request::print_job(&CLIENT, 7, &ticket);
    assert_eq!(m.code, op::PRINT_JOB);
    let names: Vec<&str> = m.groups[0]
        .attributes
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "attributes-charset",
            "attributes-natural-language",
            "printer-uri",
            "requesting-user-name",
            "job-name",
            "document-format",
        ]
    );
    assert_eq!(m.int(tag::JOB, "copies"), Some(2));
    assert_eq!(m.int(tag::JOB, "print-quality"), Some(5));
    assert_eq!(m.text(tag::JOB, "print-color-mode"), Some("monochrome"));
    let mut wire = m.encode().unwrap();
    let head = wire.len();
    wire.extend_from_slice(b"RaS2");
    let (back, end) = Message::decode(&wire).unwrap();
    assert_eq!(back, m);
    assert_eq!(&wire[end..], b"RaS2");
    assert_eq!(end, head);
}

#[test]
fn create_job_then_send_document_split_the_print_job() {
    let ticket = Ticket {
        name: String::from("Letter to Bob"),
        format: String::from("image/pwg-raster"),
        copies: Some(1),
        media: Some(String::from("iso_a4_210x297mm")),
        ..Ticket::default()
    };
    let create = request::create_job(&CLIENT, 2, &ticket);
    assert_eq!(create.code, op::CREATE_JOB);
    let names: Vec<&str> = create.groups[0]
        .attributes
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    // No document attributes: the format travels with the document.
    assert_eq!(
        names,
        [
            "attributes-charset",
            "attributes-natural-language",
            "printer-uri",
            "requesting-user-name",
            "job-name",
        ]
    );
    assert_eq!(create.text(tag::JOB, "media"), Some("iso_a4_210x297mm"));

    let send = request::send_document(&CLIENT, 3, 12, "image/pwg-raster", true);
    assert_eq!(send.code, op::SEND_DOCUMENT);
    let names: Vec<&str> = send.groups[0]
        .attributes
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "attributes-charset",
            "attributes-natural-language",
            "printer-uri",
            "job-id",
            "requesting-user-name",
            "document-format",
            "last-document",
        ]
    );
    assert_eq!(send.int(tag::OPERATION, "job-id"), Some(12));
    assert_eq!(
        send.get(tag::OPERATION, "last-document")
            .map(|a| a.values.clone()),
        Some(vec![Value::Boolean(true)])
    );
    assert_eq!(send.groups.len(), 1);
    let wire = send.encode().unwrap();
    assert_eq!(Message::decode(&wire).unwrap().0, send);
}

#[test]
fn a_bare_ticket_sends_no_job_group() {
    let ticket = Ticket {
        name: String::from("x"),
        format: String::from("image/pwg-raster"),
        ..Ticket::default()
    };
    let m = request::validate_job(&CLIENT, 1, &ticket);
    assert_eq!(m.code, op::VALIDATE_JOB);
    assert!(m.group(tag::JOB).is_none());
}

#[test]
fn job_requests_name_the_job_after_the_uri() {
    let m = request::get_job_attributes(&CLIENT, 3, 42);
    assert_eq!(m.groups[0].attributes[3].name, "job-id");
    assert_eq!(m.int(tag::OPERATION, "job-id"), Some(42));
    assert!(m.get(tag::OPERATION, "requested-attributes").is_some());
    let c = request::cancel_job(&CLIENT, 4, 42);
    assert_eq!(c.code, op::CANCEL_JOB);
    assert!(c.get(tag::OPERATION, "requested-attributes").is_none());
}

#[test]
fn a_job_reply_reads_as_a_status() {
    let mut m = Message::new(0, 7);
    let mut job = Group::new(tag::JOB);
    job.attributes
        .push(Attribute::new("job-id", Value::Integer(12)));
    job.attributes.push(Attribute::new(
        "job-state",
        Value::Enum(job_state::PROCESSING),
    ));
    job.attributes.push(Attribute::set(
        "job-state-reasons",
        vec![Value::keyword("none"), Value::keyword("media-empty")],
    ));
    job.attributes.push(Attribute::new(
        "job-state-message",
        Value::string(tag::TEXT, "Out of paper"),
    ));
    m.groups.push(job);
    let status = JobStatus::of(&m);
    assert_eq!(status.id, Some(12));
    assert_eq!(status.state, Some(job_state::PROCESSING));
    assert_eq!(status.reasons, vec![String::from("media-empty")]);
    assert_eq!(status.message.as_deref(), Some("Out of paper"));
    assert!(!job_state::is_final(5));
    assert!(job_state::is_final(job_state::COMPLETED));
}

#[test]
fn malformed_messages_are_refused() {
    let good = deskjet_reply().encode().unwrap();
    // Every proper prefix is truncated.
    for cut in 0..good.len() {
        assert!(Message::decode(&good[..cut]).is_err(), "prefix {cut}");
    }
    let head = [2u8, 0, 0, 0, 0, 0, 0, 1];
    let with = |body: &[u8]| {
        let mut v = head.to_vec();
        v.extend_from_slice(body);
        v
    };
    // A value before any group.
    assert_eq!(
        Message::decode(&with(&[tag::INTEGER, 0, 1, b'a', 0, 4, 0, 0, 0, 1, 3])),
        Err(DecodeError::Orphan)
    );
    // An additional value with nothing to add to.
    assert_eq!(
        Message::decode(&with(&[1, tag::INTEGER, 0, 0, 0, 4, 0, 0, 0, 1, 3])),
        Err(DecodeError::Orphan)
    );
    // Reserved and extension tags.
    assert_eq!(Message::decode(&with(&[0])), Err(DecodeError::BadTag(0)));
    assert_eq!(
        Message::decode(&with(&[1, 0x7F])),
        Err(DecodeError::BadTag(0x7F))
    );
    // An integer of three bytes, a boolean of 2.
    assert_eq!(
        Message::decode(&with(&[1, tag::INTEGER, 0, 1, b'a', 0, 3, 0, 0, 1, 3])),
        Err(DecodeError::BadLength(tag::INTEGER))
    );
    assert_eq!(
        Message::decode(&with(&[1, tag::BOOLEAN, 0, 1, b'a', 0, 1, 2, 3])),
        Err(DecodeError::BadLength(tag::BOOLEAN))
    );
    // endCollection outside a collection.
    assert_eq!(
        Message::decode(&with(&[1, tag::END_COLLECTION, 0, 1, b'a', 0, 0, 3])),
        Err(DecodeError::BadCollection)
    );
}

#[test]
fn nesting_and_size_are_bounded() {
    let mut value = Value::Integer(1);
    for _ in 0..=MAX_DEPTH {
        value = Value::Collection(vec![Attribute::new("m", value)]);
    }
    let mut m = Message::new(0, 1);
    let mut g = Group::new(tag::PRINTER);
    g.attributes.push(Attribute::new("deep", value));
    m.groups.push(g);
    assert_eq!(
        Message::decode(&m.encode().unwrap()),
        Err(DecodeError::TooDeep)
    );

    let mut m = Message::new(0, 1);
    let mut g = Group::new(tag::PRINTER);
    for i in 0..=crate::MAX_ATTRIBUTES {
        g.attributes.push(Attribute::new(
            &alloc::format!("a{i}"),
            Value::Boolean(true),
        ));
    }
    m.groups.push(g);
    assert_eq!(
        Message::decode(&m.encode().unwrap()),
        Err(DecodeError::TooMany)
    );
}

#[test]
fn invalid_utf8_is_shown_not_refused() {
    let body = [
        2u8,
        0,
        0,
        0,
        0,
        0,
        0,
        1,
        4,
        tag::TEXT,
        0,
        1,
        b'a',
        0,
        2,
        0xFF,
        b'x',
        3,
    ];
    let (m, _) = Message::decode(&body).unwrap();
    assert_eq!(m.text(tag::PRINTER, "a"), Some("\u{FFFD}x"));
}

#[test]
fn printer_addresses_are_completed_and_checked() {
    let p = PrinterUri::parse(" 192.168.1.89 ").unwrap();
    assert_eq!(p.to_ipp(), "ipp://192.168.1.89:631/ipp/print");
    assert_eq!(p.host_header(), "192.168.1.89:631");
    let p = PrinterUri::parse("IPP://printer.lan:8631/printers/q").unwrap();
    assert_eq!(
        (p.host.as_str(), p.port, p.path.as_str()),
        ("printer.lan", 8631, "/printers/q")
    );
    let p = PrinterUri::parse("http://10.0.2.2:8000/").unwrap();
    assert_eq!(p.to_ipp(), "ipp://10.0.2.2:8000/ipp/print");
    let p = PrinterUri::parse("ipp://[fe80::1]:631").unwrap();
    assert_eq!(p.connect_host(), "fe80::1");
    for bad in [
        "",
        "ipps://192.168.1.89",
        "ftp://x",
        "host:0",
        "host:99999",
        "a b",
        "host\r\nX: y",
        "host/pa th",
        "[zz]",
        "[::1]x",
        "user@host",
    ] {
        assert!(PrinterUri::parse(bad).is_err(), "{bad:?}");
    }
}

#[cfg(feature = "std")]
mod http;
