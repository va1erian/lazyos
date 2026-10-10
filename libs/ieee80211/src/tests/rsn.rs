use std::vec;
use std::vec::Vec;

use crate::rsn::*;

#[test]
fn wpa2_psk_bytes() {
    // 802.11-2020 9.4.2.24: version 1, CCMP group, one CCMP pairwise, one PSK
    // AKM, no capabilities. The classic 20-octet body.
    let ie = Rsn::wpa2_psk().to_ie().unwrap();
    assert_eq!(
        ie,
        [
            0x30, 0x14, 0x01, 0x00, 0x00, 0x0F, 0xAC, 0x04, 0x01, 0x00, 0x00, 0x0F, 0xAC, 0x04,
            0x01, 0x00, 0x00, 0x0F, 0xAC, 0x02, 0x00, 0x00
        ]
    );
    assert_eq!(Rsn::parse_ie(&ie), Ok(Rsn::wpa2_psk()));
}

#[test]
fn full_round_trip() {
    let rsn = Rsn {
        group: Cipher::Tkip,
        pairwise: vec![Cipher::Ccmp128, Cipher::Tkip, Cipher::Gcmp256],
        akms: vec![
            Akm::Psk,
            Akm::PskSha256,
            Akm::Sae,
            Akm::Other(Suite([0, 0x50, 0xF2, 2])),
        ],
        caps: RsnCaps(RsnCaps::MFPC | 0x000C),
        pmkids: vec![[7; 16], [9; 16]],
        group_mgmt: Some(Cipher::BipCmac128),
    };
    let ie = rsn.to_ie().unwrap();
    assert_eq!(Rsn::parse_ie(&ie), Ok(rsn.clone()));
    assert!(rsn.caps.mfp_capable() && !rsn.caps.mfp_required());
    // Group management cipher without PMKIDs writes a zero PMKID count.
    let gm = Rsn {
        pmkids: Vec::new(),
        ..rsn
    };
    assert_eq!(Rsn::parse_body(&gm.to_body()), Ok(gm));
}

#[test]
fn every_cipher_and_akm_selector_round_trips() {
    for kind in 0..=20u8 {
        let suite = Suite([0x00, 0x0F, 0xAC, kind]);
        assert_eq!(Cipher::from_suite(suite).to_suite(), suite);
        assert_eq!(Akm::from_suite(suite).to_suite(), suite);
    }
    let vendor = Suite([0x00, 0x50, 0xF2, 4]);
    assert_eq!(Cipher::from_suite(vendor), Cipher::Other(vendor));
    assert_eq!(Cipher::Tkip.name(), "TKIP");
    assert_eq!(Akm::PskSha256.name(), "PSK-SHA256");
}

#[test]
fn optional_fields_default_when_the_element_ends_early() {
    let only_version = Rsn::parse_body(&[1, 0]).unwrap();
    assert_eq!(only_version.group, Cipher::Ccmp128);
    assert_eq!(only_version.pairwise, vec![Cipher::Ccmp128]);
    assert_eq!(only_version.akms, vec![Akm::Ieee8021x]);
    // After the group cipher, after the pairwise list, after the AKM list.
    let ie = Rsn::wpa2_psk().to_body();
    for end in [6, 12, 18, 20] {
        assert!(Rsn::parse_body(&ie[..end]).is_ok(), "end {end}");
    }
}

#[test]
fn truncated_and_oversized_counts_are_refused() {
    let body = Rsn::wpa2_psk().to_body();
    for end in [0, 1, 3, 4, 5, 7, 8, 9, 10, 11, 13, 14, 15, 16, 17, 19] {
        assert!(Rsn::parse_body(&body[..end]).is_err(), "end {end}");
    }
    // A pairwise count of 65535 with 4 bytes behind it must not allocate.
    let mut huge = vec![1, 0, 0x00, 0x0F, 0xAC, 4, 0xFF, 0xFF];
    huge.extend_from_slice(&[0, 0x0F, 0xAC, 4]);
    assert_eq!(Rsn::parse_body(&huge), Err(RsnError::Truncated));
    // A PMKID count that outruns the element.
    let mut pmk = Rsn::wpa2_psk().to_body();
    pmk.extend_from_slice(&[3, 0, 1, 2, 3]);
    assert_eq!(Rsn::parse_body(&pmk), Err(RsnError::Truncated));
}

#[test]
fn bad_version_trailing_bytes_and_wrong_element() {
    assert_eq!(Rsn::parse_body(&[2, 0]), Err(RsnError::BadVersion(2)));
    assert_eq!(Rsn::parse_body(&[0, 0]), Err(RsnError::BadVersion(0)));
    let mut long = Rsn::wpa2_psk().to_body();
    long.extend_from_slice(&[0, 0, 0x00, 0x0F, 0xAC, 6, 9]);
    assert_eq!(Rsn::parse_body(&long), Err(RsnError::Trailing));
    let ie = Rsn::wpa2_psk().to_ie().unwrap();
    assert_eq!(Rsn::parse_ie(&ie[..ie.len() - 1]), Err(RsnError::NotRsn));
    let mut wrong_id = ie.clone();
    wrong_id[0] = 221;
    assert_eq!(Rsn::parse_ie(&wrong_id), Err(RsnError::NotRsn));
    let mut longer = ie;
    longer.push(0);
    assert_eq!(Rsn::parse_ie(&longer), Err(RsnError::NotRsn));
}

#[test]
fn too_many_suites_do_not_fit_one_element() {
    let rsn = Rsn {
        pairwise: vec![Cipher::Ccmp128; 70],
        ..Rsn::wpa2_psk()
    };
    assert_eq!(rsn.to_ie(), Err(crate::Error::IeTooLong));
}
