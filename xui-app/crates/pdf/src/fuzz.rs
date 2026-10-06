//! The seeded fuzz entry point: any bytes, opened as a PDF, every page sized,
//! drawn small and read for text, must fail cleanly and never panic.
//!
//! [`run`] is what a cargo-fuzz target would call; the in-tree test mutates
//! the checked-in samples with a seeded generator (`FUZZ_CASES`, `FUZZ_SEED`
//! override the run, as in the other fuzzed libraries). On LazyOS the app
//! builds with `panic = "abort"`, so a panic here is a crash there.

use crate::{Document, Renderer};

/// Pages a case draws, so a mutated page count does not make it slow.
const PAGE_LIMIT: usize = 8;
/// Pixels per point a case draws at: enough to run every operator.
const SCALE: f32 = 0.2;

/// Feed `data` to the document, the renderer and the text layer.
pub fn run(data: &[u8]) {
    for password in ["", "lazyos"] {
        let Ok(doc) = Document::open(data.to_vec(), password) else {
            continue;
        };
        let _ = doc.info();
        let renderer = Renderer::new(&doc);
        for index in 0..doc.page_count().min(PAGE_LIMIT) {
            let _ = doc.page_size(index);
            // A mutated MediaBox can ask for an enormous page: draw a corner.
            let _ = renderer.render_tile(index, SCALE, 0, 0, 128, 128);
            let _ = renderer.page_text(index);
        }
    }
}

/// A small xorshift generator: deterministic per seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}

/// Tokens a mutation splices in: the values parsers trip over.
const TOKENS: &[&[u8]] = &[
    b"0",
    b"-1",
    b"999999999",
    b"1e308",
    b"null",
    b"<<",
    b">>",
    b"[",
    b"]",
    b"R",
    b"0 0 R",
    b"/Length 0",
    b"endstream",
    b"q",
    b"Q",
    b"cm",
    b"Tf",
    b"Do",
    b"BT",
    b"ET",
];

/// `seed` with a few random edits: flipped bytes, a cut, a duplicated run, a
/// token over a number.
pub fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut data = seed.to_vec();
    for _ in 0..=rng.below(4) {
        if data.is_empty() {
            data.push(rng.next_u64() as u8);
            continue;
        }
        let at = rng.below(data.len());
        match rng.below(5) {
            0 => data[at] ^= 1 << rng.below(8),
            1 => data[at] = rng.next_u64() as u8,
            2 => data.truncate(at.max(data.len() / 2)),
            3 => {
                let len = rng.below(64).min(data.len() - at);
                let run = data[at..at + len].to_vec();
                let to = rng.below(data.len());
                data.splice(to..to, run);
            }
            _ => {
                let token = TOKENS[rng.below(TOKENS.len())];
                let end = (at + rng.below(4)).min(data.len());
                data.splice(at..end, token.iter().copied());
            }
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    fn seeds() -> Vec<Vec<u8>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
        let mut seeds: Vec<Vec<u8>> = std::fs::read_dir(dir)
            .expect("testdata")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "pdf"))
            .filter_map(|e| std::fs::read(e.path()).ok())
            .collect();
        seeds.push(Vec::new());
        seeds
    }

    #[test]
    fn seeded_mutations_never_panic() {
        static PANICS: AtomicUsize = AtomicUsize::new(0);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANICS.fetch_add(1, Ordering::Relaxed);
            previous(info);
        }));
        // Few by default: unoptimised hayro is slow. Soak with `--release`.
        let cases = env_number("FUZZ_CASES", 60);
        let seed = env_number("FUZZ_SEED", 0x9d0f_00d5);
        let seeds = seeds();
        let mut rng = Rng::new(seed);
        for case in 0..cases {
            let base = &seeds[rng.below(seeds.len())];
            let data = mutate(&mut rng, base);
            let outcome = std::panic::catch_unwind(|| run(&data));
            assert!(
                outcome.is_ok() && PANICS.load(Ordering::Relaxed) == 0,
                "case {case} panicked; replay with FUZZ_SEED=0x{seed:x}"
            );
        }
    }
}
