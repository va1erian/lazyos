//! The seeded fuzz entry point: any bytes, opened and tested as an archive
//! of every kind, must fail cleanly and never panic or hang.
//!
//! [`run`] is what a cargo-fuzz target would call; the in-tree test mutates
//! the checked-in fixtures and the library's own output with a seeded
//! generator (`FUZZ_CASES`, `FUZZ_SEED` override the run, as in the other
//! fuzzed libraries).

use std::io::{self, Cursor, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::codec::{self, Codec};
use crate::tar::{self, Flow};
use crate::{extract, Archive, Progress};

/// Largest output a fuzz case decompresses before giving up on it, so a
/// small bomb does not make a case slow.
const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;

/// Feed `data` to every parser.
pub fn run(data: &[u8]) {
    let _ = tar::walk(&mut Cursor::new(data), &mut |_, body| {
        io::copy(&mut body.take(OUTPUT_LIMIT), &mut io::sink())?;
        Ok(Flow::Continue)
    });
    for codec in [Codec::Gzip, Codec::Xz, Codec::Zstd] {
        if let Ok(mut stream) = codec::decoder(codec, Box::new(Cursor::new(data.to_vec()))) {
            let _ = io::copy(&mut (&mut stream).take(OUTPUT_LIMIT), &mut io::sink());
        }
    }
    let path = scratch();
    if std::fs::write(&path, data).is_ok() {
        let progress = Arc::new(Progress::new());
        if let Ok(archive) = Archive::open(&path, &progress) {
            if archive.total_size() <= OUTPUT_LIMIT {
                let _ = extract::test(&archive, &progress);
            }
        }
    }
    let _ = std::fs::remove_file(path);
}

fn scratch() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("lazyarc-fuzz-{}-{n}", std::process::id()))
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

/// `seed` with a few random edits: flipped bytes, a cut, a duplicated run.
pub fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut data = seed.to_vec();
    for _ in 0..=rng.below(4) {
        if data.is_empty() {
            data.push(rng.next_u64() as u8);
            continue;
        }
        match rng.below(4) {
            0 => {
                let at = rng.below(data.len());
                data[at] ^= 1 << rng.below(8);
            }
            1 => {
                let at = rng.below(data.len());
                data[at] = rng.next_u64() as u8;
            }
            2 => data.truncate(rng.below(data.len())),
            _ => {
                let at = rng.below(data.len());
                let len = rng.below(64).min(data.len() - at);
                let run = data[at..at + len].to_vec();
                let to = rng.below(data.len());
                data.splice(to..to, run);
            }
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut seeds: Vec<Vec<u8>> = std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().is_some_and(|x| x != "py" && x != "md"))
                    .filter_map(|e| std::fs::read(e.path()).ok())
                    .collect()
            })
            .unwrap_or_default();
        seeds.push(Vec::new());
        seeds
    }

    #[test]
    fn seeded_mutations_never_panic() {
        // The guest builds with `panic = "abort"`, so a panic on a decoder's
        // helper thread would kill the app: count panics on every thread.
        static PANICS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANICS.fetch_add(1, Ordering::Relaxed);
            previous(info);
        }));
        let cases = env_number("FUZZ_CASES", 400);
        let seed = env_number("FUZZ_SEED", 0x1a2a_3a4a);
        let seeds = seeds();
        let mut rng = Rng::new(seed);
        for case in 0..cases {
            let base = &seeds[rng.below(seeds.len())];
            let data = mutate(&mut rng, base);
            let outcome = std::panic::catch_unwind(|| run(&data));
            assert!(
                outcome.is_ok(),
                "case {case} panicked; replay with FUZZ_SEED=0x{seed:x}"
            );
            // Helper threads end with their stream, which `run` drained.
            assert_eq!(
                PANICS.load(Ordering::Relaxed),
                0,
                "case {case} panicked on a helper thread; replay with FUZZ_SEED=0x{seed:x}"
            );
        }
    }
}
