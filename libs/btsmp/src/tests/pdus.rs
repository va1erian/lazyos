//! PDU codec tests.

use alloc::format;
use alloc::vec::Vec;

use crate::pdu::*;

fn features() -> PairingFeatures {
    PairingFeatures {
        io_capability: IoCapability::KeyboardDisplay,
        oob_present: false,
        auth_req: AuthReq(0x2D),
        max_key_size: 16,
        initiator_keys: 0x0A,
        responder_keys: 0x02,
    }
}

fn every_pdu() -> Vec<Pdu> {
    alloc::vec![
        Pdu::PairingRequest(features()),
        Pdu::PairingResponse(features()),
        Pdu::PairingConfirm([1; 16]),
        Pdu::PairingRandom([2; 16]),
        Pdu::PairingFailed(Reason(0x0B)),
        Pdu::EncryptionInfo([3; 16]),
        Pdu::CentralIdentification {
            ediv: 0x1234,
            rand: [4; 8],
        },
        Pdu::IdentityInfo([5; 16]),
        Pdu::IdentityAddressInfo {
            addr_type: AddrType::Random,
            addr: [6; 6],
        },
        Pdu::SigningInfo([7; 16]),
        Pdu::SecurityRequest(AuthReq(0x0D)),
        Pdu::PublicKey([8; 64]),
        Pdu::DhKeyCheck([9; 16]),
        Pdu::KeypressNotification(3),
    ]
}

#[test]
fn every_command_round_trips() {
    for pdu in every_pdu() {
        let bytes = pdu.encode();
        assert_eq!(bytes[0], pdu.code());
        assert_eq!(Pdu::decode(&bytes), Ok(pdu));
    }
}

#[test]
fn pairing_request_matches_the_spec_layout() {
    let req = Pdu::PairingRequest(PairingFeatures {
        io_capability: IoCapability::DisplayYesNo,
        oob_present: false,
        auth_req: AuthReq(0x0D),
        max_key_size: 16,
        initiator_keys: 0,
        responder_keys: 2,
    });
    assert_eq!(req.encode(), [0x01, 0x01, 0x00, 0x0D, 0x10, 0x00, 0x02]);
}

#[test]
fn central_identification_is_little_endian() {
    let bytes = [0x07, 0x34, 0x12, 1, 2, 3, 4, 5, 6, 7, 8];
    assert_eq!(
        Pdu::decode(&bytes),
        Ok(Pdu::CentralIdentification {
            ediv: 0x1234,
            rand: [1, 2, 3, 4, 5, 6, 7, 8]
        })
    );
}

#[test]
fn every_truncation_and_extension_is_refused() {
    for pdu in every_pdu() {
        let bytes = pdu.encode();
        for len in 1..bytes.len() {
            assert_eq!(
                Pdu::decode(&bytes[..len]),
                Err(DecodeError::BadLength { code: bytes[0] })
            );
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(
            Pdu::decode(&longer),
            Err(DecodeError::BadLength { code: bytes[0] })
        );
    }
}

#[test]
fn out_of_range_fields_are_refused() {
    let good = Pdu::PairingRequest(features()).encode();
    for (index, bad) in [(1, 5), (2, 2), (4, 6), (4, 17), (4, 0)] {
        let mut bytes = good.clone();
        bytes[index] = bad;
        assert_eq!(
            Pdu::decode(&bytes),
            Err(DecodeError::BadValue { code: 1 }),
            "byte {index} = {bad}"
        );
    }
    assert_eq!(
        Pdu::decode(&[0x09, 2, 0, 0, 0, 0, 0, 0]),
        Err(DecodeError::BadValue { code: 9 })
    );
    assert_eq!(Pdu::decode(&[]), Err(DecodeError::Empty));
    assert_eq!(Pdu::decode(&[0x00]), Err(DecodeError::UnknownCode(0)));
    assert_eq!(Pdu::decode(&[0x0F]), Err(DecodeError::UnknownCode(0x0F)));
    assert_eq!(
        DecodeError::UnknownCode(0x0F).reason(),
        Reason::COMMAND_NOT_SUPPORTED
    );
}

#[test]
fn debug_names_the_command_and_nothing_else() {
    assert_eq!(
        format!("{:?}", Pdu::IdentityInfo([0x77; 16])),
        "IdentityInformation"
    );
    assert_eq!(
        format!("{:?}", Pdu::PairingConfirm([0x77; 16])),
        "PairingConfirm"
    );
}

#[test]
fn auth_req_bits() {
    let auth = AuthReq(0x0D);
    assert!(auth.bonding() && auth.mitm() && auth.secure_connections());
    assert!(!AuthReq(0x00).secure_connections());
    // Bonding flags 0b10 and 0b11 are reserved: not "bonding".
    assert!(!AuthReq(0x02).bonding());
}
