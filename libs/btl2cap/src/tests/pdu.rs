use crate::pdu::*;
use crate::Error;

fn cmd(ident: u8, signal: Signal) -> Command {
    Command { ident, signal }
}

#[test]
fn basic_header() {
    let p = encode_basic(0x0040, &[1, 2, 3]).unwrap();
    assert_eq!(p, [3, 0, 0x40, 0, 1, 2, 3]);
    assert_eq!(parse_basic(&p), Ok((0x40, &[1u8, 2, 3][..])));
    assert_eq!(parse_basic(&[3, 0, 0x40, 0, 1]), Err(Error::BadLength));
    assert_eq!(parse_basic(&[0, 0, 1]), Err(Error::Truncated));
    assert_eq!(
        encode_basic(1, &alloc::vec![0; 70_000]),
        Err(Error::TooLong)
    );
}

#[test]
fn golden_signalling() {
    let golden: [(&[u8], Command); 7] = [
        (
            &[0x02, 0x01, 0x04, 0x00, 0x11, 0x00, 0x40, 0x00],
            cmd(
                1,
                Signal::ConnectionRequest {
                    psm: 0x11,
                    scid: 0x40,
                },
            ),
        ),
        (
            &[
                0x03, 0x01, 0x08, 0x00, 0x41, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
            ],
            cmd(
                1,
                Signal::ConnectionResponse {
                    dcid: 0x41,
                    scid: 0x40,
                    result: 0,
                    status: 0,
                },
            ),
        ),
        (
            &[
                0x04, 0x02, 0x08, 0x00, 0x41, 0x00, 0x00, 0x00, 0x01, 0x02, 0xA0, 0x02,
            ],
            cmd(
                2,
                Signal::ConfigRequest {
                    dcid: 0x41,
                    flags: 0,
                    options: alloc::vec![ConfOption::Mtu(672)],
                },
            ),
        ),
        (
            &[0x05, 0x02, 0x06, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00],
            cmd(
                2,
                Signal::ConfigResponse {
                    scid: 0x40,
                    flags: 0,
                    result: 0,
                    options: alloc::vec![],
                },
            ),
        ),
        (
            &[0x06, 0x03, 0x04, 0x00, 0x41, 0x00, 0x40, 0x00],
            cmd(
                3,
                Signal::DisconnectRequest {
                    dcid: 0x41,
                    scid: 0x40,
                },
            ),
        ),
        (
            &[0x0A, 0x04, 0x02, 0x00, 0x02, 0x00],
            cmd(4, Signal::InformationRequest { info_type: 2 }),
        ),
        (
            &[0x08, 0x05, 0x02, 0x00, 0xAB, 0xCD],
            cmd(5, Signal::EchoRequest(alloc::vec![0xAB, 0xCD])),
        ),
    ];
    for (bytes, c) in golden {
        assert_eq!(c.encode(), bytes, "{c:?}");
        assert_eq!(parse_commands(bytes), Ok(alloc::vec![c]));
    }
}

#[test]
fn several_commands_in_one_pdu() {
    let mut b = cmd(1, Signal::EchoRequest(alloc::vec![])).encode();
    b.extend(cmd(2, Signal::InformationRequest { info_type: 3 }).encode());
    assert_eq!(parse_commands(&b).unwrap().len(), 2);
    // Too many is abuse.
    let many: alloc::vec::Vec<u8> = (0..MAX_COMMANDS_PER_PDU as u8 + 1)
        .flat_map(|i| cmd(i + 1, Signal::EchoRequest(alloc::vec![])).encode())
        .collect();
    assert_eq!(parse_commands(&many), Err(Error::TooLong));
}

#[test]
fn malformed_commands_are_refused() {
    // Length points past the end.
    assert_eq!(
        parse_commands(&[0x02, 0x01, 0x09, 0x00, 0x11, 0x00]),
        Err(Error::Truncated)
    );
    // Ident 0.
    assert_eq!(
        parse_commands(&[0x08, 0x00, 0x00, 0x00]),
        Err(Error::Invalid)
    );
    // Connection request with the wrong length.
    assert_eq!(
        parse_commands(&[0x02, 0x01, 0x03, 0x00, 1, 2, 3]),
        Err(Error::BadLength)
    );
    // Truncated header.
    assert_eq!(parse_commands(&[0x02, 0x01]), Err(Error::Truncated));
    // An option whose length runs off the end.
    assert_eq!(
        parse_commands(&[0x04, 0x01, 0x07, 0x00, 0x41, 0x00, 0x00, 0x00, 0x01, 0x09, 0x00]),
        Err(Error::Truncated)
    );
    // MTU option with a wrong inner length.
    assert_eq!(
        parse_commands(&[0x04, 0x01, 0x07, 0x00, 0x41, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00]),
        Err(Error::BadLength)
    );
}

#[test]
fn too_many_options_are_refused() {
    let mut b = alloc::vec![0x04, 0x01, 0, 0, 0x41, 0x00, 0x00, 0x00];
    for _ in 0..MAX_OPTIONS + 1 {
        b.extend_from_slice(&[0x01, 0x02, 0xA0, 0x02]);
    }
    let len = (b.len() - 4) as u16;
    b[2..4].copy_from_slice(&len.to_le_bytes());
    assert_eq!(parse_commands(&b), Err(Error::TooLong));
}

#[test]
fn unknown_options_and_commands_survive() {
    let c = cmd(
        9,
        Signal::ConfigRequest {
            dcid: 0x41,
            flags: 0,
            options: alloc::vec![
                ConfOption::Unknown {
                    kind: 0x83,
                    data: alloc::vec![1, 2, 3]
                },
                ConfOption::Fcs(0)
            ],
        },
    );
    assert_eq!(parse_commands(&c.encode()), Ok(alloc::vec![c]));
    let u = cmd(
        1,
        Signal::Unknown {
            code: 0x77,
            data: alloc::vec![9],
        },
    );
    assert_eq!(parse_commands(&u.encode()), Ok(alloc::vec![u]));
}
