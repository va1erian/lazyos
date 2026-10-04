//! Byte-level fuzzing of the decoder and an encode/decode round trip.
//!
//! [`run`] is the shared entry point for the cargo-fuzz target
//! (`fuzz/fuzz_targets/pwgraster.rs`) and the seeded tests below. Whatever
//! the bytes, decoding must not panic or exceed its budget, and a stream that
//! decodes must re-encode to pages that decode to the same pixels.

use std::vec::Vec;

use crate::{decode, PageEncoder, SYNC};

/// The decode budget: a fuzz case stays small.
pub const BUDGET: usize = 1 << 20;

/// Decode `input`; when it is a stream, check that re-encoding it gives the
/// same pages back.
pub fn run(input: &[u8]) {
    let Ok(pages) = decode(input, BUDGET) else {
        return;
    };
    let mut stream = SYNC.to_vec();
    for page in &pages {
        let mut encoder = PageEncoder::new(page.header.clone());
        let line = page.header.bytes_per_line();
        for row in page.pixels.chunks_exact(line) {
            encoder.push_row(row).expect("a decoded row re-encodes");
        }
        stream.extend(encoder.finish().expect("every row given"));
    }
    let again = decode(&stream, BUDGET).expect("re-encoded pages decode");
    assert_eq!(again, pages);
    let _: Vec<_> = again.into_iter().map(|p| p.pixels.len()).collect();
}

#[cfg(test)]
mod seeded {
    use super::*;
    use crate::{ColorSpace, Header};
    use fuzzkit::{for_seeds, Rng};

    /// A page of rows drawn from a few patterns, so runs, literals and line
    /// repeats all occur.
    fn page(rng: &mut Rng) -> std::vec::Vec<u8> {
        let header = Header {
            width: rng.range(1, 300) as u32,
            height: rng.range(1, 600) as u32,
            dpi: 300,
            color: *rng.pick(&[ColorSpace::Sgray8, ColorSpace::Srgb8]),
            media: "iso_a4_210x297mm".into(),
            quality: 4,
            total_pages: 1,
        };
        let line = header.bytes_per_line();
        let patterns: std::vec::Vec<std::vec::Vec<u8>> = (0..rng.range(1, 4))
            .map(|_| {
                let mut row = std::vec![255u8; line];
                for _ in 0..rng.below(20) {
                    let at = rng.below(line as u64) as usize;
                    let len = (rng.range(1, 40) as usize).min(line - at);
                    if rng.one_in(2) {
                        row[at..at + len].fill(rng.byte());
                    } else {
                        rng.fill(&mut row[at..at + len]);
                    }
                }
                row
            })
            .collect();
        let mut encoder = PageEncoder::new(header.clone());
        let mut pixels = std::vec::Vec::new();
        for _ in 0..header.height {
            let row = rng.pick(&patterns);
            pixels.extend_from_slice(row);
            encoder.push_row(row).unwrap();
        }
        let mut stream = SYNC.to_vec();
        stream.extend(encoder.finish().unwrap());
        let pages = decode(&stream, BUDGET).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].header, header);
        assert_eq!(pages[0].pixels, pixels);
        stream
    }

    #[test]
    fn random_pages_round_trip() {
        for_seeds("raster_random_pages_round_trip", |_, rng| run(&page(rng)));
    }

    #[test]
    fn mutated_streams_never_panic() {
        for_seeds("raster_mutated_streams_never_panic", |_, rng| {
            let mut bytes = page(rng);
            let flips = rng.range(1, 12) as usize;
            rng.flip_bits(&mut bytes, flips);
            if rng.one_in(3) {
                let cut = rng.below(bytes.len() as u64 + 1) as usize;
                bytes.truncate(cut);
            }
            run(&bytes);
        });
    }
}

/// The checked-in cargo-fuzz seeds (`fuzz/gen_corpus.py`) replay here, and
/// the page seeds decode.
#[cfg(test)]
#[test]
fn the_fuzz_seeds_replay() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/pwgraster");
    let mut seen = 0;
    for entry in std::fs::read_dir(dir).expect("fuzz/seeds/pwgraster exists") {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        run(&bytes);
        if path.to_string_lossy().ends_with("_page") || path.ends_with("two_pages") {
            assert!(decode(&bytes, BUDGET).is_ok(), "{}", path.display());
        }
        seen += 1;
    }
    assert!(seen >= 4);
}
