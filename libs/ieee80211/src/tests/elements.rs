use std::vec;
use std::vec::Vec;

use super::beacon_ies;
use crate::ie::*;
use crate::Error;

#[test]
fn full_beacon_elements() {
    let ies = beacon_ies(b"LazyNet", 6);
    let e = Elements::parse(&ies).unwrap();
    assert_eq!(e.ssid, Some(&b"LazyNet"[..]));
    assert_eq!(e.rates.unwrap().len(), 8);
    assert_eq!(e.ext_rates.unwrap().len(), 2);
    assert_eq!(e.ds_channel, Some(6));
    assert_eq!(e.country_code(), Some(*b"US"));
    assert_eq!(e.ht_cap.unwrap().len(), 26);
    assert!(e.rsn.is_some() && e.rsn_ie.unwrap()[0] == ID_RSN);
    assert_eq!(e.vendor.len(), 1);
    assert_eq!(e.vendor[0].oui, [0x00, 0x50, 0xF2]);
    assert_eq!(e.vendor[0].kind, 2);
    assert!(e.wpa_ie.is_none());
    assert!(!e.truncated && e.duplicates == 0 && e.malformed == 0 && !e.hidden());
}

#[test]
fn ssid_zero_length_all_nul_and_non_utf8() {
    let empty = IeBuilder::new().ssid(b"").unwrap().finish();
    assert!(Elements::parse(&empty).unwrap().hidden());
    let nuls = IeBuilder::new().ssid(&[0; 7]).unwrap().finish();
    let e = Elements::parse(&nuls).unwrap();
    assert!(e.hidden() && e.ssid.unwrap().len() == 7);
    // One non-NUL octet makes it a name.
    let name = IeBuilder::new().ssid(&[0, 0, 1]).unwrap().finish();
    assert!(!Elements::parse(&name).unwrap().hidden());
    // Not UTF-8, kept as bytes.
    let raw = IeBuilder::new().ssid(&[0xFF, 0xFE, b'x']).unwrap().finish();
    assert_eq!(
        Elements::parse(&raw).unwrap().ssid,
        Some(&[0xFF, 0xFE, b'x'][..])
    );
    // No SSID element at all is hidden too.
    assert!(Elements::parse(&[]).unwrap().hidden());
}

#[test]
fn ssid_over_32_is_the_one_hard_error() {
    let mut ies = Vec::new();
    push_ie(&mut ies, ID_SSID, &[b'a'; 33]).unwrap();
    assert_eq!(Elements::parse(&ies), Err(Error::BadSsid));
    let mut max = Vec::new();
    push_ie(&mut max, ID_SSID, &[b'a'; 32]).unwrap();
    assert!(Elements::parse(&max).is_ok());
    assert_eq!(IeBuilder::new().ssid(&[0; 33]).unwrap_err(), Error::BadSsid);
}

#[test]
fn truncated_tails_keep_the_elements_before_them() {
    let mut ies = beacon_ies(b"net", 1);
    let whole = ies.len();
    // A lone ID octet.
    ies.push(0x30);
    let e = Elements::parse(&ies).unwrap();
    assert!(e.truncated && e.ssid == Some(&b"net"[..]));
    // A length that runs past the end.
    ies.truncate(whole);
    ies.extend_from_slice(&[221, 200, 1, 2, 3]);
    assert!(Elements::parse(&ies).unwrap().truncated);
    // Cutting inside the middle element keeps what came earlier.
    let cut = &beacon_ies(b"net", 1)[..33];
    let e = Elements::parse(cut).unwrap();
    assert!(e.truncated && e.ssid.is_some());
}

#[test]
fn duplicate_singletons_first_wins() {
    let ies = IeBuilder::new()
        .ssid(b"first")
        .unwrap()
        .ssid(b"second")
        .unwrap()
        .ds_channel(1)
        .unwrap()
        .ds_channel(11)
        .unwrap()
        .rsn(&crate::Rsn::wpa2_psk())
        .unwrap()
        .rsn(&crate::Rsn::wpa2_psk_sha256())
        .unwrap()
        .finish();
    let e = Elements::parse(&ies).unwrap();
    assert_eq!(e.ssid, Some(&b"first"[..]));
    assert_eq!(e.ds_channel, Some(1));
    assert_eq!(e.duplicates, 3);
    // The first RSN element is the one kept, whole.
    assert_eq!(
        e.rsn_ie.unwrap(),
        &crate::Rsn::wpa2_psk().to_ie().unwrap()[..]
    );
}

#[test]
fn wrong_length_known_elements_are_ignored_and_counted() {
    let ies = IeBuilder::new()
        .raw(ID_DS_PARAMS, &[1, 2])
        .unwrap()
        .raw(ID_DS_PARAMS, &[])
        .unwrap()
        .raw(ID_COUNTRY, b"US")
        .unwrap()
        .raw(ID_VENDOR, &[1, 2, 3])
        .unwrap()
        .finish();
    let e = Elements::parse(&ies).unwrap();
    assert_eq!((e.ds_channel, e.country), (None, None));
    assert_eq!(e.malformed, 4);
}

#[test]
fn vendor_elements_wpa1_and_cap() {
    let wpa = [0x00, 0x50, 0xF2, 0x01, 0x01, 0x00];
    let ies = IeBuilder::new().raw(ID_VENDOR, &wpa).unwrap().finish();
    let e = Elements::parse(&ies).unwrap();
    assert_eq!(e.wpa_ie, Some(&ies[..]));
    let mut many = Vec::new();
    for i in 0..(MAX_VENDOR + 5) {
        push_ie(&mut many, ID_VENDOR, &[1, 2, 3, i as u8]).unwrap();
    }
    let e = Elements::parse(&many).unwrap();
    assert_eq!(e.vendor.len(), MAX_VENDOR);
    assert_eq!(e.vendor_dropped, 5);
}

#[test]
fn capabilities_are_kept_as_raw_ranges() {
    let ies = IeBuilder::new()
        .raw(ID_HT_CAP, &[1; 26])
        .unwrap()
        .raw(ID_VHT_CAP, &[2; 12])
        .unwrap()
        .raw(ID_EXTENSION, &[EXT_HE_CAP, 3, 3, 3])
        .unwrap()
        .raw(ID_EXTENSION, &[99, 3])
        .unwrap()
        .finish();
    let e = Elements::parse(&ies).unwrap();
    assert_eq!(e.ht_cap.unwrap(), &[1; 26][..]);
    assert_eq!(e.vht_cap.unwrap(), &[2; 12][..]);
    assert_eq!(e.he_cap.unwrap(), &[3, 3, 3][..]);
}

#[test]
fn builder_limits_and_rate_split() {
    assert_eq!(
        push_ie(&mut Vec::new(), 1, &[0; 256]),
        Err(Error::IeTooLong)
    );
    let ies = IeBuilder::new()
        .rates(&[1, 2, 3, 4, 5, 6, 7, 8, 9])
        .unwrap()
        .finish();
    let e = Elements::parse(&ies).unwrap();
    assert_eq!((e.rates.unwrap().len(), e.ext_rates.unwrap().len()), (8, 1));
    let none: Vec<u8> = vec![];
    assert!(ElementIter::new(&none).next().is_none());
}
