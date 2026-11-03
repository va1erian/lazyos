//! Golden packets: byte strings as they appear on the wire (the form HCI
//! traces and the Core Specification's examples show), checked both ways.

use crate::acl::{self, Boundary};
use crate::addr::BdAddr;
use crate::command::Command;
use crate::event::{AdvReport, Event, LeConnection};
use crate::opcode::{self as op, status};
use crate::ret;

const PEER: BdAddr = BdAddr([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);

fn command(packet: &[u8], cmd: Command) {
    assert_eq!(cmd.encode(), packet, "{cmd:?}");
    assert_eq!(Command::parse(packet), Ok(cmd));
}

fn event(packet: &[u8], ev: Event) {
    assert_eq!(ev.encode(), packet, "{ev:?}");
    assert_eq!(Event::parse(packet), Ok(ev));
}

#[test]
fn opcodes_from_ogf_and_ocf() {
    assert_eq!(op::RESET, 0x0C03);
    assert_eq!(op::READ_BD_ADDR, 0x1009);
    assert_eq!(op::CREATE_CONNECTION, 0x0405);
    assert_eq!(op::DISCONNECT, 0x0406);
    assert_eq!(op::LE_CREATE_CONNECTION, 0x200D);
    assert_eq!(op::LE_START_ENCRYPTION, 0x2019);
    assert_eq!(op::WRITE_SIMPLE_PAIRING_MODE, 0x0C56);
}

#[test]
fn commands_without_parameters() {
    command(&[0x03, 0x0C, 0x00], Command::Reset);
    command(&[0x09, 0x10, 0x00], Command::ReadBdAddr);
    command(&[0x01, 0x10, 0x00], Command::ReadLocalVersion);
    command(&[0x05, 0x10, 0x00], Command::ReadBufferSize);
    command(&[0x02, 0x20, 0x00], Command::LeReadBufferSize);
    command(&[0x10, 0x20, 0x00], Command::LeClearAcceptList);
}

#[test]
fn read_encryption_key_size_codec() {
    assert_eq!(op::READ_ENCRYPTION_KEY_SIZE, 0x1408);
    command(
        &[0x08, 0x14, 0x02, 0x40, 0x00],
        Command::ReadEncryptionKeySize { handle: 0x40 },
    );
    assert!(Command::parse(&[0x08, 0x14, 0x01, 0x40]).is_err());
    let rp = crate::ret::EncryptionKeySize {
        status: 0,
        handle: 0x40,
        key_size: 7,
    };
    let bytes = crate::ret::encode_encryption_key_size(rp);
    assert_eq!(bytes, [0, 0x40, 0, 7]);
    assert_eq!(crate::ret::parse_encryption_key_size(&bytes), Ok(rp));
    assert!(crate::ret::parse_encryption_key_size(&bytes[..3]).is_err());
}

#[test]
fn commands_with_parameters() {
    command(
        &[
            0x01, 0x0C, 0x08, 0xFF, 0xFF, 0xFB, 0xFF, 0x07, 0xF8, 0xBF, 0x3D,
        ],
        Command::SetEventMask(0x3DBF_F807_FFFB_FFFF),
    );
    command(&[0x1A, 0x0C, 0x01, 0x02], Command::WriteScanEnable(2));
    command(
        &[0x56, 0x0C, 0x01, 0x01],
        Command::WriteSimplePairingMode(true),
    );
    command(
        &[0x24, 0x0C, 0x03, 0x04, 0x01, 0x00],
        Command::WriteClassOfDevice(0x0104),
    );
    command(
        &[0x06, 0x04, 0x03, 0x40, 0x00, 0x13],
        Command::Disconnect {
            handle: 0x40,
            reason: 0x13,
        },
    );
    command(
        &[
            0x05, 0x04, 0x0D, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x18, 0xCC, 0x02, 0x00, 0x00,
            0x00, 0x01,
        ],
        Command::CreateConnection {
            addr: PEER,
            packet_type: 0xCC18,
            page_scan_repetition_mode: 2,
            clock_offset: 0,
            allow_role_switch: 1,
        },
    );
    command(
        &[0x09, 0x04, 0x07, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x01],
        Command::AcceptConnectionRequest {
            addr: PEER,
            role: 1,
        },
    );
    command(
        &[
            0x2B, 0x04, 0x09, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x01, 0x00, 0x04,
        ],
        Command::IoCapabilityRequestReply {
            addr: PEER,
            io_capability: 1,
            oob_present: 0,
            authentication_requirements: 4,
        },
    );
    command(
        &[0x13, 0x04, 0x03, 0x40, 0x00, 0x01],
        Command::SetConnectionEncryption {
            handle: 0x40,
            enable: true,
        },
    );
}

#[test]
fn write_local_name_pads_to_248_and_trims_on_parse() {
    let packet = Command::WriteLocalName(b"LazyOS".to_vec()).encode();
    assert_eq!(&packet[..3], &[0x13, 0x0C, 0xF8]);
    assert_eq!(packet.len(), 3 + 248);
    assert_eq!(&packet[3..9], b"LazyOS");
    assert!(packet[9..].iter().all(|&b| b == 0));
    assert_eq!(
        Command::parse(&packet),
        Ok(Command::WriteLocalName(b"LazyOS".to_vec()))
    );
    // A name above the limit is cut, never overflowing the length octet.
    let long = Command::WriteLocalName(alloc::vec![b'x'; 1000]).encode();
    assert_eq!(long.len(), 3 + 248);
}

#[test]
fn le_commands() {
    command(
        &[0x0C, 0x20, 0x02, 0x01, 0x01],
        Command::LeSetScanEnable {
            enable: true,
            filter_duplicates: true,
        },
    );
    command(
        &[0x11, 0x20, 0x07, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        Command::LeAddDeviceToAcceptList {
            address_type: 0,
            addr: PEER,
        },
    );
    let mut enc = alloc::vec![0x19, 0x20, 28, 0x40, 0x00];
    enc.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 0x34, 0x12]);
    enc.extend_from_slice(&[0xA5; 16]);
    command(
        &enc,
        Command::LeStartEncryption {
            handle: 0x40,
            random: 0x0807_0605_0403_0201,
            ediv: 0x1234,
            ltk: [0xA5; 16],
        },
    );
    let mut conn = alloc::vec![0x0D, 0x20, 25, 0x60, 0, 0x30, 0, 1, 0];
    conn.extend_from_slice(&[0; 6]);
    conn.extend_from_slice(&[0, 0x18, 0, 0x28, 0, 0, 0, 0x90, 0x01, 0, 0, 0, 0]);
    command(&conn, crate::init::le_auto_connect());
}

#[test]
fn command_complete_and_status() {
    event(
        &[0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00],
        Event::CommandComplete {
            num_commands: 1,
            opcode: op::RESET,
            return_params: alloc::vec![0],
        },
    );
    event(
        &[0x0F, 0x04, 0x00, 0x01, 0x05, 0x04],
        Event::CommandStatus {
            status: 0,
            num_commands: 1,
            opcode: op::CREATE_CONNECTION,
        },
    );
}

#[test]
fn return_parameters() {
    let bd = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    assert_eq!(ret::parse_bd_addr(&bd), Ok(PEER));
    let version = [0, 9, 0x34, 0x12, 9, 0x02, 0x00, 0x78, 0x56];
    let v = ret::parse_local_version(&version).unwrap();
    assert_eq!(
        (
            v.hci_version,
            v.hci_revision,
            v.manufacturer,
            v.lmp_subversion
        ),
        (9, 0x1234, 2, 0x5678)
    );
    assert_eq!(ret::encode_local_version(&v), version);
    let buf = [0, 0x36, 0x01, 0x00, 0x0A, 0x00, 0x00, 0x00];
    let b = ret::parse_buffer_size(&buf).unwrap();
    assert_eq!((b.acl_len, b.acl_num), (0x136, 10));
    assert_eq!(ret::encode_buffer_size(b), buf);
    let le = ret::parse_le_buffer_size(&[0, 0xFB, 0, 8]).unwrap();
    assert_eq!((le.acl_len, le.acl_num), (251, 8));
    assert_eq!(ret::status(&[]), Err(crate::Error::Truncated));
}

#[test]
fn connection_events() {
    event(
        &[
            0x04, 0x0A, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x40, 0x05, 0x00, 0x01,
        ],
        Event::ConnectionRequest {
            addr: PEER,
            class_of_device: 0x0540,
            link_type: 1,
        },
    );
    event(
        &[
            0x03, 0x0B, 0x00, 0x40, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x01, 0x00,
        ],
        Event::ConnectionComplete {
            status: 0,
            handle: 0x40,
            addr: PEER,
            link_type: 1,
            encryption_enabled: false,
        },
    );
    event(
        &[0x05, 0x04, 0x00, 0x40, 0x00, 0x13],
        Event::DisconnectionComplete {
            status: 0,
            handle: 0x40,
            reason: 0x13,
        },
    );
    event(
        &[0x06, 0x03, 0x00, 0x40, 0x00],
        Event::AuthenticationComplete {
            status: 0,
            handle: 0x40,
        },
    );
    event(
        &[0x08, 0x04, 0x00, 0x40, 0x00, 0x01],
        Event::EncryptionChange {
            status: 0,
            handle: 0x40,
            enabled: true,
        },
    );
}

#[test]
fn pairing_events() {
    event(
        &[0x17, 0x06, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        Event::LinkKeyRequest { addr: PEER },
    );
    let mut notif = alloc::vec![0x18, 23, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    notif.extend_from_slice(&[7; 16]);
    notif.push(5);
    event(
        &notif,
        Event::LinkKeyNotification {
            addr: PEER,
            key: [7; 16],
            key_type: 5,
        },
    );
    event(
        &[
            0x33, 0x0A, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x40, 0xE2, 0x01, 0x00,
        ],
        Event::UserConfirmationRequest {
            addr: PEER,
            numeric_value: 123_456,
        },
    );
    event(
        &[
            0x3B, 0x0A, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x40, 0xE2, 0x01, 0x00,
        ],
        Event::UserPasskeyNotification {
            addr: PEER,
            passkey: 123_456,
        },
    );
    event(
        &[0x34, 0x06, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        Event::UserPasskeyRequest { addr: PEER },
    );
    event(
        &[0x36, 0x07, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        Event::SimplePairingComplete {
            status: status::SUCCESS,
            addr: PEER,
        },
    );
    event(
        &[0x31, 0x06, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        Event::IoCapabilityRequest { addr: PEER },
    );
}

#[test]
fn number_of_completed_packets() {
    event(
        &[
            0x13, 0x09, 0x02, 0x40, 0x00, 0x41, 0x00, 0x01, 0x00, 0x03, 0x00,
        ],
        Event::NumberOfCompletedPackets(alloc::vec![(0x40, 1), (0x41, 3)]),
    );
}

#[test]
fn le_meta_events() {
    let conn = LeConnection {
        status: 0,
        handle: 0x40,
        role: 0,
        peer_address_type: 1,
        peer: PEER,
        interval: 0x18,
        latency: 0,
        supervision_timeout: 0x190,
        clock_accuracy: 0,
    };
    event(
        &[
            0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x00, 0x01, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x18, 0x00, 0x00, 0x00, 0x90, 0x01, 0x00,
        ],
        Event::LeConnectionComplete(conn),
    );
    let enhanced = Event::LeEnhancedConnectionComplete {
        conn,
        local_rpa: BdAddr([9; 6]),
        peer_rpa: BdAddr([8; 6]),
    };
    assert_eq!(Event::parse(&enhanced.encode()), Ok(enhanced.clone()));
    assert_eq!(enhanced.encode()[..4], [0x3E, 31, 0x0A, 0x00]);
    event(
        &[
            0x3E, 0x0D, 0x05, 0x40, 0x00, 1, 2, 3, 4, 5, 6, 7, 8, 0x34, 0x12,
        ],
        Event::LeLongTermKeyRequest {
            handle: 0x40,
            random: 0x0807_0605_0403_0201,
            ediv: 0x1234,
        },
    );
}

#[test]
fn le_meta_ltk_request_length_matches() {
    // Length octet 0x0D = subevent + handle + rand + ediv.
    let p = Event::LeLongTermKeyRequest {
        handle: 1,
        random: 2,
        ediv: 3,
    }
    .encode();
    assert_eq!(p[1], 13);
}

#[test]
fn advertising_report() {
    let ev = Event::LeAdvertisingReport(alloc::vec![
        AdvReport {
            event_type: 0,
            address_type: 1,
            addr: PEER,
            data: alloc::vec![2, 1, 6],
            rssi: -60
        },
        AdvReport {
            event_type: 4,
            address_type: 0,
            addr: BdAddr([1; 6]),
            data: alloc::vec![],
            rssi: -10
        },
    ]);
    assert_eq!(Event::parse(&ev.encode()), Ok(ev));
}

#[test]
fn unknown_codes_are_kept() {
    event(
        &[0x7F, 0x02, 0xAA, 0xBB],
        Event::Unknown {
            code: 0x7F,
            params: alloc::vec![0xAA, 0xBB],
        },
    );
    event(
        &[0x3E, 0x03, 0x77, 1, 2],
        Event::UnknownLe {
            subevent: 0x77,
            params: alloc::vec![1, 2],
        },
    );
    command(
        &[0x00, 0xFC, 0x01, 0x05],
        Command::Unknown {
            opcode: 0xFC00,
            params: alloc::vec![5],
        },
    );
}

#[test]
fn acl_headers() {
    let p = acl::encode(0x0040, Boundary::FirstFlushable, &[1, 2, 3]).unwrap();
    assert_eq!(p, [0x40, 0x20, 0x03, 0x00, 1, 2, 3]);
    let (h, data) = acl::parse(&p).unwrap();
    assert_eq!(
        (h.handle, h.boundary, h.len, data),
        (0x40, Boundary::FirstFlushable, 3, &[1u8, 2, 3][..])
    );
    let c = acl::encode(0x0041, Boundary::Continuing, &[]).unwrap();
    assert_eq!(c, [0x41, 0x10, 0, 0]);
}

#[test]
fn addresses_print_msb_first_and_redact() {
    use alloc::format;
    assert_eq!(format!("{PEER}"), "66:55:44:33:22:11");
    assert_eq!(format!("{}", PEER.redacted()), "xx:xx:xx:xx:22:11");
    assert_eq!(BdAddr::from_msb([0x66, 0x55, 0x44, 0x33, 0x22, 0x11]), PEER);
    assert!(!format!("{PEER:?}").contains("44"));
}
