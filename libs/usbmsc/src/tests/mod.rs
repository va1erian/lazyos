//! Host tests: descriptors, the CBW/CSW wire format, BOT recovery against
//! the model device, the SCSI parsers and the disk layer.

mod disk;
pub(crate) mod golden;
pub(crate) mod model;
mod transport;

use crate::desc::{find_bot, BulkEndpoint};
use crate::scsi;
use crate::Error;

#[test]
fn qemu_high_speed_stick_is_found() {
    let found = find_bot(&golden::HS_CONFIG).unwrap().unwrap();
    assert_eq!(found.config_value, 1);
    assert_eq!(found.number, 0);
    assert_eq!(
        found.bulk_in,
        BulkEndpoint {
            address: 0x81,
            max_packet: 512,
            max_burst: 0
        }
    );
    assert_eq!(found.bulk_out.address, 0x02);
    assert_eq!(found.bulk_out.max_packet, 512);
}

#[test]
fn superspeed_companion_sets_the_burst() {
    let found = find_bot(&golden::SS_CONFIG).unwrap().unwrap();
    assert_eq!(found.bulk_in.max_packet, 1024);
    assert_eq!(found.bulk_in.max_burst, 15);
    assert_eq!(found.bulk_out.max_packet, 1024);
    assert_eq!(found.bulk_out.max_burst, 3);
}

#[test]
fn a_hid_device_has_no_storage_interface() {
    assert_eq!(find_bot(&golden::KBD_CONFIG).unwrap(), None);
}

#[test]
fn composite_device_finds_the_second_interface() {
    let found = find_bot(&golden::COMPOSITE_CONFIG).unwrap().unwrap();
    assert_eq!(found.number, 1);
    assert_eq!(found.bulk_in.address, 0x83);
}

#[test]
fn an_alternate_setting_or_a_missing_endpoint_is_not_enough() {
    let mut alt = golden::HS_CONFIG;
    alt[9 + 3] = 1; // bAlternateSetting 1
    assert_eq!(find_bot(&alt).unwrap(), None);
    // Turn the OUT endpoint into an interrupt endpoint: no bulk-OUT left.
    let mut no_out = golden::HS_CONFIG;
    no_out[25 + 3] = 3;
    assert_eq!(find_bot(&no_out).unwrap(), None);
    // A bulk endpoint 0 or a packet size of 0 is refused.
    let mut zero = golden::HS_CONFIG;
    zero[18 + 4] = 0;
    zero[18 + 5] = 0;
    assert_eq!(find_bot(&zero).unwrap(), None);
}

#[test]
fn hostile_lengths_are_refused() {
    let mut zero_len = golden::HS_CONFIG;
    zero_len[18] = 0;
    assert_eq!(find_bot(&zero_len), Err(Error::BadLength));
    let mut overlong = golden::HS_CONFIG;
    overlong[18] = 200;
    assert_eq!(find_bot(&overlong), Err(Error::BadLength));
    let mut total = golden::HS_CONFIG;
    total[2] = 0xFF;
    assert_eq!(find_bot(&total), Err(Error::BadLength));
    assert_eq!(find_bot(&golden::HS_CONFIG[..5]), Err(Error::Short));
    let mut short_endpoint = golden::HS_CONFIG;
    short_endpoint[18] = 4;
    assert!(find_bot(&short_endpoint).is_err());
}

#[test]
fn rw_picks_the_ten_or_sixteen_byte_form() {
    let ten = scsi::rw(false, 0x1234_5678, 128).unwrap();
    assert_eq!(
        ten.as_bytes(),
        &[0x28, 0, 0x12, 0x34, 0x56, 0x78, 0, 0, 128, 0]
    );
    let write = scsi::rw(true, 7, 1).unwrap();
    assert_eq!(write.as_bytes()[0], 0x2A);
    let big = scsi::rw(false, 1 << 32, 8).unwrap();
    assert_eq!(big.as_bytes().len(), 16);
    assert_eq!(big.as_bytes()[0], 0x88);
    assert_eq!(&big.as_bytes()[2..10], &(1u64 << 32).to_be_bytes());
    assert_eq!(&big.as_bytes()[10..14], &8u32.to_be_bytes());
    let many = scsi::rw(true, 0, 70_000).unwrap();
    assert_eq!(many.as_bytes()[0], 0x8A);
    assert!(scsi::rw(false, 0, 0).is_none());
}

#[test]
fn sense_formats_and_actions() {
    let mut fixed = [0u8; 18];
    fixed[0] = 0x70;
    fixed[2] = 0x06;
    fixed[12] = 0x28;
    let sense = scsi::parse_sense(&fixed).unwrap();
    assert_eq!(sense.action(), scsi::Action::Retry);
    fixed[2] = 0x02;
    fixed[12] = 0x3A;
    assert_eq!(
        scsi::parse_sense(&fixed).unwrap().action(),
        scsi::Action::NoMedium
    );
    fixed[12] = 0x04;
    assert_eq!(
        scsi::parse_sense(&fixed).unwrap().action(),
        scsi::Action::Wait
    );
    let descriptor = [0x72, 0x07, 0x27, 0x00];
    assert_eq!(
        scsi::parse_sense(&descriptor).unwrap().action(),
        scsi::Action::WriteProtected
    );
    // A short fixed record keeps its key.
    assert_eq!(scsi::parse_sense(&[0xF0, 0, 0x03]).unwrap().key, 3);
    assert_eq!(scsi::parse_sense(&[]), Err(Error::Short));
    assert_eq!(scsi::parse_sense(&[0x00; 18]), Err(Error::Malformed));
    assert_eq!(scsi::parse_sense(&[0x72, 0]), Err(Error::Short));
}

#[test]
fn capacity_is_checked() {
    let mut ten = [0u8; 8];
    ten[0..4].copy_from_slice(&999u32.to_be_bytes());
    ten[4..8].copy_from_slice(&512u32.to_be_bytes());
    let capacity = scsi::parse_capacity_10(&ten).unwrap().unwrap();
    assert_eq!(capacity.blocks, 1000);
    ten[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(scsi::parse_capacity_10(&ten).unwrap(), None);
    ten[4..8].copy_from_slice(&520u32.to_be_bytes());
    ten[0] = 0;
    assert_eq!(scsi::parse_capacity_10(&ten), Err(Error::Malformed));
    assert_eq!(scsi::parse_capacity_10(&ten[..7]), Err(Error::Short));
    let mut sixteen = [0u8; 32];
    sixteen[0..8].copy_from_slice(&u64::MAX.to_be_bytes());
    sixteen[8..12].copy_from_slice(&512u32.to_be_bytes());
    assert_eq!(scsi::parse_capacity_16(&sixteen), Err(Error::BadLength));
    sixteen[0..8].copy_from_slice(&(1u64 << 40).to_be_bytes());
    assert_eq!(
        scsi::parse_capacity_16(&sixteen).unwrap().blocks,
        (1 << 40) + 1
    );
}

#[test]
fn inquiry_strings_are_sanitized() {
    let mut data = [0u8; 36];
    data[1] = 0x80;
    data[8..16].copy_from_slice(b"VEND\x1b\xffOR");
    let inquiry = scsi::parse_inquiry(&data).unwrap();
    assert!(inquiry.removable);
    assert_eq!(&inquiry.vendor, b"VEND??OR");
    assert_eq!(&inquiry.product, b"????????????????");
    let short = scsi::parse_inquiry(&[0x00, 0x00, 0x05, 0x02, 0x1F]).unwrap();
    assert_eq!(&short.vendor, b"        ");
    assert_eq!(scsi::parse_inquiry(&[0; 4]), Err(Error::Short));
}

#[test]
fn write_protect_bit() {
    assert!(scsi::parse_write_protect(&[3, 0, 0x80, 0]).unwrap());
    assert!(!scsi::parse_write_protect(&[3, 0, 0x00, 0]).unwrap());
    assert_eq!(scsi::parse_write_protect(&[3, 0]), Err(Error::Short));
}
