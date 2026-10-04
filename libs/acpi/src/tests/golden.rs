//! Golden table images: real dumps of QEMU's firmware (written by
//! `tools/acpi/dump_tables.py`) and one synthetic image in the shape of a
//! modern UEFI PC (ACPI 2.0 XSDP, XSDT, a revision 6 FADT with `X_` blocks,
//! an MADT with x2APIC entries), since QEMU's own tables are RSDT-only.

use std::vec::Vec;

use crate::fuzz::{seal, Image};

const DUMPS: [(&str, &[u8]); 5] = [
    ("pc-seabios", include_bytes!("../../golden/pc-seabios.bin")),
    (
        "pc-seabios-nohpet",
        include_bytes!("../../golden/pc-seabios-nohpet.bin"),
    ),
    (
        "q35-seabios",
        include_bytes!("../../golden/q35-seabios.bin"),
    ),
    ("pc-ovmf", include_bytes!("../../golden/pc-ovmf.bin")),
    ("q35-ovmf", include_bytes!("../../golden/q35-ovmf.bin")),
];

/// One golden dump by name.
pub fn dump(name: &str) -> Image {
    let (_, bytes) = DUMPS
        .iter()
        .find(|(n, _)| *n == name)
        .expect("no such golden dump");
    assert_eq!(&bytes[..8], b"ACPIDUMP");
    Image::parse(&bytes[8..])
}

/// Every golden dump, with its name.
pub fn all() -> Vec<(&'static str, Image)> {
    DUMPS.iter().map(|(name, _)| (*name, dump(name))).collect()
}

// Layout of the synthetic image.
pub const RSDP: u64 = 0x7FF0_0000;
pub const XSDT: u64 = 0x7FF0_1000;
pub const RSDT: u64 = 0x7FF0_2000;
pub const FADT: u64 = 0x7FF0_3000;
pub const MADT: u64 = 0x7FF0_4000;
pub const HPET: u64 = 0x7FF0_5000;
pub const DSDT: u64 = 0x7FF0_6000;

/// A sealed table: the 36-byte header with `signature` and `revision`, then
/// `body`.
pub fn table(signature: &[u8; 4], revision: u8, body: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(36 + body.len());
    data.extend_from_slice(signature);
    data.extend_from_slice(&(36 + body.len() as u32).to_le_bytes());
    data.push(revision);
    data.push(0);
    data.extend_from_slice(b"LAZYOS");
    data.extend_from_slice(b"GOLDEN  ");
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(b"LZY ");
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(body);
    seal(&mut data);
    data
}

/// A 12-byte Generic Address Structure.
pub fn gas(space: u8, bits: u8, access: u8, address: u64) -> [u8; 12] {
    let mut raw = [0u8; 12];
    raw[0] = space;
    raw[1] = bits;
    raw[3] = access;
    raw[4..].copy_from_slice(&address.to_le_bytes());
    raw
}

fn put(body: &mut [u8], table_offset: usize, bytes: &[u8]) {
    let at = table_offset - 36;
    body[at..at + bytes.len()].copy_from_slice(bytes);
}

/// A revision 6 FADT (276 bytes) for an Intel PCH with ACPI base 0x1800.
pub fn modern_fadt() -> Vec<u8> {
    let mut body = std::vec![0u8; 276 - 36];
    put(&mut body, 40, &(DSDT as u32).to_le_bytes());
    put(&mut body, 46, &9u16.to_le_bytes());
    put(&mut body, 56, &0x1800u32.to_le_bytes());
    put(&mut body, 64, &0x1804u32.to_le_bytes());
    put(&mut body, 76, &0x1808u32.to_le_bytes());
    put(&mut body, 88, &[4, 2, 0, 4]);
    put(&mut body, 108, &[0x32]);
    put(&mut body, 109, &0x0001u16.to_le_bytes());
    let flags: u32 = (1 << 8) | (1 << 10);
    put(&mut body, 112, &flags.to_le_bytes());
    put(&mut body, 116, &gas(1, 8, 1, 0xCF9));
    put(&mut body, 128, &[0x06]);
    put(&mut body, 131, &[5]);
    put(&mut body, 140, &DSDT.to_le_bytes());
    put(&mut body, 148, &gas(1, 32, 3, 0x1800));
    put(&mut body, 172, &gas(1, 16, 2, 0x1804));
    put(&mut body, 208, &gas(1, 32, 3, 0x1808));
    table(b"FACP", 6, &body)
}

/// An MADT with legacy and x2APIC processor entries, one I/O APIC, two
/// overrides and both kinds of NMI entry.
pub fn modern_madt() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0xFEE0_0000u32.to_le_bytes());
    body.extend_from_slice(&1u32.to_le_bytes());
    // Processor local APIC: UID 0, APIC ID 0, enabled; UID 1 disabled.
    body.extend_from_slice(&[0, 8, 0, 0, 1, 0, 0, 0]);
    body.extend_from_slice(&[0, 8, 1, 2, 0, 0, 0, 0]);
    // I/O APIC 2 at 0xFEC00000, GSI base 0.
    body.extend_from_slice(&[1, 12, 2, 0]);
    body.extend_from_slice(&0xFEC0_0000u32.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());
    // Overrides: IRQ0 -> GSI2; IRQ9 -> GSI9 level, active low.
    body.extend_from_slice(&[2, 10, 0, 0, 2, 0, 0, 0, 0, 0]);
    body.extend_from_slice(&[2, 10, 0, 9, 9, 0, 0, 0, 0x0D, 0]);
    // Local APIC NMI: all processors, LINT1.
    body.extend_from_slice(&[4, 6, 0xFF, 0, 0, 1]);
    // x2APIC entries: IDs 0x100 and 0x108 enabled, 0x110 online-capable.
    for (id, flags) in [(0x100u32, 1u32), (0x108, 1), (0x110, 2)] {
        body.extend_from_slice(&[9, 16, 0, 0]);
        body.extend_from_slice(&id.to_le_bytes());
        body.extend_from_slice(&flags.to_le_bytes());
        body.extend_from_slice(&id.to_le_bytes());
    }
    // x2APIC NMI: all processors, LINT1.
    body.extend_from_slice(&[10, 12, 0, 0]);
    body.extend_from_slice(&u32::MAX.to_le_bytes());
    body.extend_from_slice(&[1, 0, 0, 0]);
    // An unknown entry kind (a future one) is skipped.
    body.extend_from_slice(&[0x7F, 4, 0xAA, 0xBB]);
    table(b"APIC", 5, &body)
}

/// An HPET table: Intel vendor, legacy-route capable, 64-bit, 8 comparators.
pub fn modern_hpet() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&0x8086_A701u32.to_le_bytes());
    body.extend_from_slice(&gas(0, 64, 0, 0xFED0_0000));
    body.extend_from_slice(&[0, 0x80, 0, 0]);
    table(b"HPET", 1, &body)
}

/// An ACPI 2.0 RSDP naming `xsdt` and `rsdt`.
pub fn rsdp(rsdt: u32, xsdt: u64) -> Vec<u8> {
    let mut data = std::vec![0u8; 36];
    data[..8].copy_from_slice(b"RSD PTR ");
    data[9..15].copy_from_slice(b"LAZYOS");
    data[15] = 2;
    data[16..20].copy_from_slice(&rsdt.to_le_bytes());
    data[20..24].copy_from_slice(&36u32.to_le_bytes());
    data[24..32].copy_from_slice(&xsdt.to_le_bytes());
    seal(&mut data);
    data
}

/// A root table (`XSDT` with 8-byte or `RSDT` with 4-byte entries).
pub fn root(signature: &[u8; 4], entries: &[u64]) -> Vec<u8> {
    let mut body = Vec::new();
    for entry in entries {
        if signature == b"XSDT" {
            body.extend_from_slice(&entry.to_le_bytes());
        } else {
            body.extend_from_slice(&(*entry as u32).to_le_bytes());
        }
    }
    table(signature, 1, &body)
}

/// The synthetic modern-PC image. The RSDT lists only the FADT, so a test can
/// tell which root was used.
pub fn synthetic_modern() -> Image {
    let mut dsdt = std::vec![0u8; 64];
    dsdt[..8].copy_from_slice(b"\x08_S5_\x12\x06\x04");
    Image {
        rsdp: RSDP,
        segments: std::vec![
            (RSDP, rsdp(RSDT as u32, XSDT)),
            (XSDT, root(b"XSDT", &[FADT, MADT, HPET])),
            (RSDT, root(b"RSDT", &[FADT])),
            (FADT, modern_fadt()),
            (MADT, modern_madt()),
            (HPET, modern_hpet()),
            (DSDT, table(b"DSDT", 2, &dsdt)),
        ],
    }
}
