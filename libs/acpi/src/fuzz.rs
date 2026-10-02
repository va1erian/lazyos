//! Fuzz entry point, shared by the seeded tests below and the cargo-fuzz
//! target (`fuzz/fuzz_targets/acpi.rs`), so a crash found by one replays
//! under the other.
//!
//! [`run`] takes a byte image of physical memory:
//!
//! ```text
//! u8 mode | u64 rsdp | { u64 phys | u32 len | bytes }*      (little endian)
//! ```
//!
//! which is the golden dump format (`tools/acpi/dump_tables.py`) without its
//! magic, plus a mode byte: bit 0 re-seals every table's checksum (and the
//! RSDP's) before parsing, so mutations reach the decoders instead of dying at
//! the checksum. Nothing may panic, and whatever [`Platform::discover`]
//! accepts must be self-consistent: every table it returns lies in readable
//! memory and sums to zero, and every decoded field is in range.

use std::vec::Vec;

use crate::sdt::{checksum, MAX_ROOT_ENTRIES};
use crate::{AddressSpace, PhysMem, Platform};

/// Segments read from one input at most.
const MAX_SEGMENTS: usize = 64;
/// Largest segment accepted (keeps an input's memory bounded).
const MAX_SEGMENT: usize = 1 << 20;

/// Physical memory made of separate segments; a read must fall inside one.
#[derive(Clone, Debug, Default)]
pub struct Image {
    pub rsdp: u64,
    pub segments: Vec<(u64, Vec<u8>)>,
}

impl Image {
    /// Parse a dump body (after any magic). Malformed tails are ignored.
    pub fn parse(body: &[u8]) -> Image {
        let mut image = Image::default();
        let Some(rsdp) = body.get(..8) else {
            return image;
        };
        image.rsdp = u64::from_le_bytes(rsdp.try_into().unwrap_or([0; 8]));
        let mut at = 8;
        while image.segments.len() < MAX_SEGMENTS && at + 12 <= body.len() {
            let phys = u64::from_le_bytes(body[at..at + 8].try_into().unwrap_or([0; 8]));
            let len = u32::from_le_bytes(body[at + 8..at + 12].try_into().unwrap_or([0; 4]));
            let len = (len as usize).min(MAX_SEGMENT).min(body.len() - at - 12);
            image
                .segments
                .push((phys, body[at + 12..at + 12 + len].to_vec()));
            at += 12 + len;
        }
        image
    }

    /// Serialize as a dump body (the inverse of [`Image::parse`]).
    pub fn to_body(&self) -> Vec<u8> {
        let mut out = self.rsdp.to_le_bytes().to_vec();
        for (phys, data) in &self.segments {
            out.extend_from_slice(&phys.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
        }
        out
    }

    /// The segment that holds a table with `signature`, mutable.
    pub fn table_mut(&mut self, signature: &[u8; 4]) -> Option<&mut Vec<u8>> {
        self.segments
            .iter_mut()
            .map(|(_, data)| data)
            .find(|data| data.starts_with(signature))
    }

    /// Recompute the checksum of every segment that looks like a table
    /// (byte 9) or an RSDP (bytes 8 and, for revision 2, 32).
    pub fn seal(&mut self) {
        for (_, data) in &mut self.segments {
            seal(data);
        }
    }
}

/// Fix the checksum bytes of one table or RSDP in place.
pub fn seal(data: &mut [u8]) {
    if data.starts_with(b"RSD PTR ") && data.len() >= 20 {
        data[8] = 0;
        data[8] = 0u8.wrapping_sub(sum(&data[..20]));
        if data.len() >= 36 && data[15] >= 2 {
            data[32] = 0;
            data[32] = 0u8.wrapping_sub(sum(&data[..36]));
        }
    } else if data.len() >= 36 && !data.starts_with(b"FACS") {
        let len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let len = len.min(data.len());
        if len >= 10 {
            data[9] = 0;
            data[9] = 0u8.wrapping_sub(sum(&data[..len]));
        }
    }
}

fn sum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, b| acc.wrapping_add(*b))
}

impl PhysMem for Image {
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool {
        let Some(end) = addr.checked_add(buf.len() as u64) else {
            return false;
        };
        for (base, data) in &self.segments {
            let seg_end = base + data.len() as u64;
            if addr >= *base && end <= seg_end {
                let from = (addr - base) as usize;
                buf.copy_from_slice(&data[from..from + buf.len()]);
                return true;
            }
        }
        false
    }
}

/// Parse one input; panics on an inconsistent result.
pub fn run(data: &[u8]) {
    decoded(data);
}

/// [`run`], returning whether a FADT, MADT or HPET table was decoded (the
/// seeded test checks that mutations still reach the decoders).
fn decoded(data: &[u8]) -> bool {
    let Some((&mode, body)) = data.split_first() else {
        return false;
    };
    let mut image = Image::parse(body);
    if mode & 1 != 0 {
        image.seal();
    }
    let Ok(platform) = Platform::discover(&image, image.rsdp) else {
        return false;
    };
    check_sealed(&image, platform.root.phys, platform.root.length);
    assert!(platform.bad_entries as u32 <= MAX_ROOT_ENTRIES);
    if let Ok(fadt) = &platform.fadt {
        check_sealed(&image, fadt.table.phys, fadt.table.length);
        if let Some(timer) = fadt.pm_timer {
            match timer.block.space {
                AddressSpace::Io => assert!(timer.block.port(4).is_some(), "PM timer port"),
                AddressSpace::Memory => assert!(timer.block.address % 4 == 0),
                other => panic!("PM timer in {other:?}"),
            }
        }
    }
    if let Ok(madt) = &platform.madt {
        check_sealed(&image, madt.table.phys, madt.table.length);
        assert!(madt.lapic_address != 0 && madt.lapic_address % 4096 == 0);
        assert!(madt.ioapics.len() <= crate::madt::MAX_IOAPICS);
        assert!(madt.processors <= madt.table.length);
    }
    if let Ok(hpet) = &platform.hpet {
        check_sealed(&image, hpet.table.phys, hpet.table.length);
        assert!(hpet.address % crate::hpet::BLOCK_LEN == 0);
    }
    if let Ok(dsdt) = &platform.dsdt {
        assert_eq!(&dsdt.signature, b"DSDT");
        check_sealed(&image, dsdt.phys, dsdt.length);
    }
    platform.fadt.is_ok() || platform.madt.is_ok() || platform.hpet.is_ok()
}

fn check_sealed(image: &Image, phys: u64, len: u32) {
    assert_eq!(
        checksum(image, phys, len),
        Ok(0),
        "accepted table at {phys:#x}"
    );
}

#[cfg(test)]
mod seeded {
    use super::*;
    use crate::tests::golden::{all, synthetic_modern};
    use fuzzkit::{for_seeds, Rng};

    /// One mutated input built from a golden image: bit flips, a retargeted
    /// pointer or length, a truncated segment, then (usually) re-sealed.
    fn mutate(rng: &mut Rng, image: &Image) -> Vec<u8> {
        let mut image = image.clone();
        let count = image.segments.len() as u64;
        for _ in 0..rng.range(1, 6) {
            let pick = rng.below(count) as usize;
            let data = &mut image.segments[pick].1;
            match rng.below(4) {
                0 => {
                    let flips = rng.range(1, 8) as usize;
                    rng.flip_bits(data, flips);
                }
                1 if data.len() > 8 => {
                    // A length or pointer field: overwrite a random u32.
                    let at = rng.below(data.len() as u64 - 4) as usize;
                    let random = rng.next_u32();
                    let value = *rng.pick(&[0u32, 1, 35, 36, 0xFFFF_FFFF, random]);
                    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
                }
                2 => {
                    let keep = rng.below(data.len() as u64 + 1) as usize;
                    data.truncate(keep);
                }
                _ => {
                    let at = rng.below(data.len() as u64 + 1) as usize;
                    let byte = rng.byte();
                    data.insert(at, byte);
                }
            }
        }
        let mode = if rng.one_in(4) { 0 } else { 1 };
        let mut input = std::vec![mode];
        input.extend_from_slice(&image.to_body());
        input
    }

    #[test]
    fn fuzz_mutated_goldens() {
        let mut images: Vec<Image> = all().into_iter().map(|(_, image)| image).collect();
        images.push(synthetic_modern());
        let (mut cases, mut deep) = (0u64, 0u64);
        for_seeds("fuzz_mutated_goldens", |_, rng| {
            let image = rng.pick(&images).clone();
            cases += 1;
            deep += u64::from(decoded(&mutate(rng, &image)));
        });
        // Most mutations must still reach a table decoder, or the fuzz only
        // exercises the RSDP checksum.
        assert!(
            deep * 4 >= cases,
            "only {deep} of {cases} inputs decoded a table"
        );
    }

    #[test]
    fn fuzz_random_bytes() {
        for_seeds("fuzz_random_bytes", |_, rng| {
            let len = rng.below(600) as usize;
            let mut input = rng.bytes(len);
            if input.len() > 9 {
                // Point the RSDP into the first segment often enough to matter.
                input[1..9].copy_from_slice(&0x1000u64.to_le_bytes());
            }
            run(&input);
        });
    }
}
