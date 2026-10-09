//! Wire layouts: the FIS, command header and PRDT entry, IDENTIFY parsing.

use crate::cmd::{Header, Prd};
use crate::fis::{ata, H2d};
use crate::identify::{Disk, Refusal};
use crate::regs::{self, Cap};

#[test]
fn h2d_layout_and_round_trip() {
    let fis = H2d::lba48(ata::READ_DMA_EXT, 0x0000_0102_0304_0506, 0x0200);
    let raw = fis.encode();
    assert_eq!(raw[0], 0x27);
    assert_eq!(raw[1], 0x80);
    assert_eq!(raw[2], 0x25);
    assert_eq!(&raw[4..7], &[0x06, 0x05, 0x04]);
    assert_eq!(raw[7], 0x40);
    assert_eq!(&raw[8..11], &[0x03, 0x02, 0x01]);
    assert_eq!(&raw[12..14], &[0x00, 0x02]);
    assert_eq!(H2d::decode(&raw), Some(fis));
    let mut bad = raw;
    bad[0] = 0x34;
    assert_eq!(H2d::decode(&bad), None);
    let mut no_command = raw;
    no_command[1] = 0;
    assert_eq!(H2d::decode(&no_command), None);
}

#[test]
fn lba_is_truncated_to_48_bits() {
    let fis = H2d::lba48(ata::WRITE_DMA_EXT, u64::MAX, 1);
    assert_eq!(fis.lba, 0xFFFF_FFFF_FFFF);
}

#[test]
fn header_and_prd_layout() {
    let header = Header {
        write: true,
        prdtl: 3,
        ctba: 0x1_2345_6780,
    };
    let raw = header.encode();
    assert_eq!(raw[0] & 0x1F, 5, "CFL");
    assert_ne!(raw[0] & 0x40, 0, "W");
    assert_eq!(u16::from_le_bytes([raw[2], raw[3]]), 3);
    assert_eq!(&raw[4..8], &[0; 4], "PRDBC starts at zero");
    assert_eq!(Header::decode(&raw), header);
    let prd = Prd {
        addr: 0x1_0000_2000,
        bytes: 4096,
    };
    let raw = prd.encode();
    assert_eq!(
        u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]),
        4095
    );
    assert_eq!(Prd::decode(&raw), prd);
}

#[test]
fn cap_and_version_decode() {
    let cap = Cap::decode(0x8000_1F05 | 0x1F00);
    assert_eq!(cap.ports, 6);
    assert_eq!(cap.slots, 32);
    assert!(cap.s64a);
    assert_eq!(regs::version(0x0001_0301), (1, 3));
    assert_eq!(regs::bar_bytes(0), 0x180);
}

fn identify(change: impl FnOnce(&mut [u16; 256])) -> [u8; 512] {
    let mut words = [0u16; 256];
    words[83] = 0x4000 | 1 << 10 | 1 << 13;
    words[84] = 0x4000;
    words[85] = 1 << 5;
    words[100] = 0x1000;
    change(&mut words);
    let mut raw = [0u8; 512];
    for (index, word) in words.iter().enumerate() {
        raw[index * 2..index * 2 + 2].copy_from_slice(&word.to_le_bytes());
    }
    raw
}

#[test]
fn identify_accepts_a_plain_lba48_disk() {
    let disk = Disk::parse(&identify(|_| {})).unwrap();
    assert_eq!(disk.sectors, 0x1000);
    assert_eq!(disk.physical_bytes, 512);
    assert!(disk.write_cache);
    assert_eq!(disk.bytes(), 0x1000 * 512);
}

#[test]
fn identify_text_is_unswapped_and_sanitised() {
    let disk = Disk::parse(&identify(|words| {
        // "AB" "\x01C" "  ": bytes swap within the word; controls become blanks.
        words[27] = u16::from_be_bytes(*b"AB");
        words[28] = u16::from_be_bytes([0x01, b'C']);
        words[29] = u16::from_be_bytes(*b"  ");
    }))
    .unwrap();
    assert_eq!(disk.model.as_str(), "AB C");
}

#[test]
fn identify_refusals() {
    let refused = |change: fn(&mut [u16; 256])| Disk::parse(&identify(change)).unwrap_err();
    assert_eq!(refused(|w| w[83] &= !(1 << 10)), Refusal::NoLba48);
    assert_eq!(refused(|w| w[83] = 1 << 10 | 1 << 13), Refusal::NoLba48);
    assert_eq!(refused(|w| w[83] &= !(1 << 13)), Refusal::NoFlush);
    assert_eq!(refused(|w| w[100] = 0), Refusal::BadCapacity);
    assert_eq!(refused(|w| w[0] = 0x8000), Refusal::NotAta);
    // 4Kn: bit 12 set, 2048 words.
    assert_eq!(
        refused(|w| {
            w[106] = 0x4000 | 1 << 12;
            w[117] = 2048;
        }),
        Refusal::SectorSize(4096)
    );
    // A logical size that overflows.
    assert!(matches!(
        refused(|w| {
            w[106] = 0x4000 | 1 << 12;
            w[117] = 0xFFFF;
            w[118] = 0xFFFF;
        }),
        Refusal::SectorSize(_)
    ));
}

#[test]
fn identify_512e_reports_the_physical_size() {
    let disk = Disk::parse(&identify(|w| w[106] = 0x4000 | 1 << 13 | 3)).unwrap();
    assert_eq!(disk.physical_bytes, 4096);
    // Shift 31 would overflow: saturates instead of panicking.
    let disk = Disk::parse(&identify(|w| w[106] = 0x4000 | 1 << 13 | 15)).unwrap();
    assert!(disk.physical_bytes >= 512);
}

#[test]
fn identify_ignores_words_whose_validity_bits_are_clear() {
    // Word 106 not marked valid: its sector-size claims do not count.
    let disk = Disk::parse(&identify(|w| w[106] = 1 << 12 | 1 << 13 | 7)).unwrap();
    assert_eq!(disk.physical_bytes, 512);
    // Word 84 not valid: the write-cache bit is not believed.
    let disk = Disk::parse(&identify(|w| w[84] = 0)).unwrap();
    assert!(!disk.write_cache);
}

#[test]
fn identify_dead_bus_is_refused() {
    assert_eq!(Disk::parse(&[0xFF; 512]).unwrap_err(), Refusal::NotAta);
    assert_eq!(Disk::parse(&[0; 512]).unwrap_err(), Refusal::NotAta);
}
