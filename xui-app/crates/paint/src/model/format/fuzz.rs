//! Seeded fuzzing of the file readers: valid files are mutated by a small
//! generator and every reader must return `Ok` or `Err`, never panic.
//!
//! `FUZZ_CASES` and `FUZZ_SEED` override the run, as in the other crates'
//! harnesses (`lazyarc`, `lazypdf`); a failure prints the seed and case to
//! replay.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::tests::layered;
use super::zip::{ZipReader, ZipWriter};
use super::*;

/// xorshift64*: small, seedable and good enough for mutations.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| {
            let v = v.trim();
            match v.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16).ok(),
                None => v.parse().ok(),
            }
        })
        .unwrap_or(default)
}

/// Values that break size arithmetic.
const EXTREMES: [u32; 10] = [
    0,
    1,
    0x7F,
    0xFF,
    0xFFFF,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
    16384,
    16385,
];

fn mutate(rng: &mut Rng, base: &[u8]) -> Vec<u8> {
    let mut data = base.to_vec();
    for _ in 0..1 + rng.below(4) {
        if data.is_empty() {
            data.push(rng.next() as u8);
        }
        let at = rng.below(data.len());
        match rng.below(7) {
            0 => data[at] ^= 1 << rng.below(8),
            1 => data[at] = rng.next() as u8,
            2 => data.truncate(at),
            3 => {
                let v = EXTREMES[rng.below(EXTREMES.len())].to_le_bytes();
                for (i, b) in v.iter().enumerate() {
                    if let Some(slot) = data.get_mut(at + i) {
                        *slot = *b;
                    }
                }
            }
            4 => {
                let end = (at + 1 + rng.below(16)).min(data.len());
                data[at..end].fill(0);
            }
            5 => {
                let end = (at + 1 + rng.below(32)).min(data.len());
                let chunk = data[at..end].to_vec();
                let to = rng.below(data.len());
                data.splice(to..to, chunk);
            }
            _ => {
                let n = 1 + rng.below(8);
                let junk: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                data.splice(at..at, junk);
            }
        }
    }
    data
}

/// A valid ORA whose stack.xml is `xml` and whose layer pictures are real.
fn with_xml(xml: &[u8], png: &[u8]) -> Vec<u8> {
    let mut zip = ZipWriter::new();
    zip.add("mimetype", b"image/openraster").unwrap();
    zip.add("stack.xml", xml).unwrap();
    zip.add("data/layer0.png", png).unwrap();
    zip.add("data/layer1.png", png).unwrap();
    zip.add("data/layer2.png", png).unwrap();
    zip.finish().unwrap()
}

fn run(data: &[u8]) {
    let _ = open(data);
    let _ = bmp::read(data);
    let _ = sniff_size(data);
    if let Ok(zip) = ZipReader::new(data) {
        for name in ["mimetype", "stack.xml", "data/layer0.png"] {
            let _ = zip.read(name, 1 << 20);
        }
    }
    let _ = xml::tags(&String::from_utf8_lossy(data), 1000);
    let _ = inflate::inflate(data, 1 << 16);
}

#[test]
fn seeded_mutations_never_panic() {
    let doc = layered();
    let ora = save(Format::OpenRaster, &doc).unwrap();
    let stack = ZipReader::new(&ora)
        .unwrap()
        .read("stack.xml", 1 << 20)
        .unwrap()
        .unwrap();
    let png = Bitmap::new(5, 4, [1, 2, 3, 255]).encode_png().unwrap();
    let mut alpha = Bitmap::new(7, 3, [9, 8, 7, 100]);
    alpha.put(2, 1, [1, 1, 1, 0]);
    let seeds: Vec<Vec<u8>> = vec![
        ora,
        save(Format::Bmp, &doc).unwrap(),
        bmp::write(&alpha).unwrap(),
        save(Format::Png, &doc).unwrap(),
        vec![
            0xFF, 0xD8, 0xFF, 0xC0, 0, 11, 8, 0, 16, 0, 32, 1, 1, 0x11, 0,
        ],
        super::tests::FOREIGN.to_vec(),
        Vec::new(),
    ];
    let cases = env_number("FUZZ_CASES", 600);
    let seed = env_number("FUZZ_SEED", 0x9a1d_7e55);
    let mut rng = Rng::new(seed);
    for case in 0..cases {
        let data = if rng.below(2) == 0 {
            // Mutate a whole file: container, headers, checksums.
            let pick = rng.below(seeds.len());
            mutate(&mut rng, &seeds[pick])
        } else {
            // Mutate stack.xml only and repack it with valid checksums, so the
            // XML and layer logic is reached instead of the CRC check.
            with_xml(&mutate(&mut rng, &stack), &png)
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| run(&data)));
        assert!(
            outcome.is_ok(),
            "case {case} panicked; replay with FUZZ_SEED=0x{seed:x} FUZZ_CASES={}",
            case + 1
        );
    }
}

#[test]
fn every_truncation_of_a_valid_file_is_handled() {
    let doc = layered();
    for format in [Format::OpenRaster, Format::Bmp, Format::Png] {
        let bytes = save(format, &doc).unwrap();
        // Every prefix up to the whole file, stepping fast through big ones.
        let step = (bytes.len() / 400).max(1);
        for n in (0..bytes.len()).step_by(step) {
            let result = catch_unwind(AssertUnwindSafe(|| open(&bytes[..n])));
            assert!(result.is_ok(), "{format:?} cut at {n}");
        }
    }
}
