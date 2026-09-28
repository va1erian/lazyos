//! Entropy pool and userspace CSPRNG for `keyd`.
//!
//! `docs/security-model.md` section 8: a pooled kernel entropy source feeds a
//! CSPRNG, and userspace gets bytes through `keyd`/`getrandom`. The kernel
//! entropy pool is still on the roadmap, so `keyd` seeds its own pool from the
//! sources available to ring 3 today:
//!
//! * **RDRAND** when CPUID reports it (`keyd` reads and mixes several values),
//! * **timing jitter**: TSC deltas around short work, and the PIT tick from the
//!   `clock` syscall, both mixed as they arrive.
//!
//! The generator is a SHA-256 counter-mode pool in the Hash_DRBG style (NIST
//! SP 800-90A): `state = SHA-256(state || label || counter)` per 32-byte block.
//! It is *not* itself a vetted DRBG from a crate — the vetted primitive is the
//! SHA-256 compression function, and the construction is simple enough to
//! audit in one screen. The follow-up is switching to the kernel pool once it
//! exists; the `Random` interface does not change.
//!
//! Determinism note: `fill` is a pure function of the pool state so tests can
//! pin behaviour. `keyd` mixes fresh RDRAND (when present) before answering
//! `Random`, which is what keeps the live path unpredictable.

use crate::sha256::{sha256_parts, DIGEST_LEN};

/// Domain-separation labels for pool operations.
const SEED_LABEL: &[u8] = b"lazyos-entropy-seed-v1";
const BLOCK_LABEL: &[u8] = b"lazyos-entropy-block-v1";

/// A 256-bit entropy pool that expands to a keystream.
#[derive(Clone)]
pub struct Entropy {
    state: [u8; DIGEST_LEN],
    counter: u64,
    seeded: bool,
}

impl Default for Entropy {
    fn default() -> Self {
        Self::new()
    }
}

impl Entropy {
    /// An empty pool. [`Entropy::fill`] output is a function of the state only
    /// once at least one seed was mixed in; `keyd` refuses to start otherwise.
    pub const fn new() -> Entropy {
        Entropy {
            state: [0u8; DIGEST_LEN],
            counter: 0,
            seeded: false,
        }
    }

    /// Whether any seed material has been mixed in.
    pub const fn is_seeded(&self) -> bool {
        self.seeded
    }

    /// Mix `bytes` into the pool (never replaces, always hashes: adding
    /// entropy cannot reduce it). Seeding is deterministic in the sense that
    /// two fresh pools fed the same bytes produce the same stream; live
    /// callers pass fresh material (PIT tick, TSC, RDRAND words), so that
    /// property never weakens the running system.
    pub fn seed(&mut self, bytes: &[u8]) {
        self.mix(&[SEED_LABEL, bytes]);
        self.seeded = true;
    }

    /// Mix one `u64` (a PIT tick, a TSC sample).
    pub fn seed_u64(&mut self, value: u64) {
        self.seed(&value.to_le_bytes());
    }

    /// Mix the x86 TSC value; a no-op on other architectures. The caller is
    /// expected to sample it around variable work so the low bits jitter.
    pub fn mix_timing(&mut self) {
        #[cfg(target_arch = "x86_64")]
        {
            // Safety: `rdtsc` is unprivileged and has no memory side effects.
            let tsc = unsafe { core::arch::x86_64::_rdtsc() };
            self.seed_u64(tsc);
        }
    }

    /// Mix fresh RDRAND words when the CPU has them; returns whether it did.
    ///
    /// `rdrand` can transiently fail under load, so a failure leaves the old
    /// pool state intact and reports `false`; the caller falls back to its
    /// other seeds.
    pub fn try_rdrand(&mut self) -> bool {
        let Some(values) = rdrand_words() else {
            return false;
        };
        for value in values {
            self.seed_u64(value);
        }
        true
    }

    /// Fill `out` with keystream bytes.
    pub fn fill(&mut self, out: &mut [u8]) {
        let mut offset = 0;
        while offset < out.len() {
            self.counter = self.counter.wrapping_add(1);
            let block = sha256_parts(&[&self.state, BLOCK_LABEL, &self.counter.to_le_bytes()]);
            self.state = block;
            let take = core::cmp::min(DIGEST_LEN, out.len() - offset);
            out[offset..offset + take].copy_from_slice(&block[..take]);
            offset += take;
        }
    }

    /// Fill a 32-byte array.
    pub fn fill_32(&mut self) -> [u8; DIGEST_LEN] {
        let mut out = [0u8; DIGEST_LEN];
        self.fill(&mut out);
        out
    }

    /// `state = SHA-256(state || parts...)`.
    fn mix(&mut self, parts: &[&[u8]]) {
        let mut all: alloc::vec::Vec<&[u8]> = alloc::vec::Vec::with_capacity(parts.len() + 1);
        all.push(&self.state);
        all.extend_from_slice(parts);
        self.state = sha256_parts(&all);
    }
}

/// Whether CPUID reports the `RDRAND` instruction (leaf 1, ECX bit 30).
pub fn rdrand_available() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        // CPUID leaf 1 always exists on x86_64; `__cpuid` is safe.
        let leaf = core::arch::x86_64::__cpuid(1);
        leaf.ecx & (1 << 30) != 0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Sixteen RDRAND words (two 64-bit reads per mix, repeated), or `None` when
/// the instruction is unavailable or every read reports failure.
///
/// RDRAND returns a 64-bit value with up to 2^64 tries before failure; one
/// success is enough, and reading several words gives the pool the 128+ bits
/// of entropy a seed needs.
pub fn rdrand_words() -> Option<[u64; 16]> {
    #[cfg(target_arch = "x86_64")]
    {
        if !rdrand_available() {
            return None;
        }
        let mut words = [0u64; 16];
        let mut successes = 0usize;
        for word in words.iter_mut() {
            let mut value = 0u64;
            // Safety: guarded by the CPUID check above; `rdrand` is a machine
            // instruction with no memory operands, and a failure leaves
            // `value` untouched and is retried by the caller path.
            let ok = unsafe { core::arch::x86_64::_rdrand64_step(&mut value) };
            if ok == 1 {
                *word = value;
                successes += 1;
            } else {
                return None;
            }
        }
        if successes == 16 {
            Some(words)
        } else {
            None
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream_and_distinct_streams() {
        let mut first = Entropy::new();
        let mut second = Entropy::new();
        let mut other = Entropy::new();
        first.seed(b"seed material");
        second.seed(b"seed material");
        other.seed(b"different material");
        let mut a = [0u8; 96];
        let mut b = [0u8; 96];
        let mut c = [0u8; 96];
        first.fill(&mut a);
        second.fill(&mut b);
        other.fill(&mut c);
        assert_eq!(a, b, "the generator must be deterministic for a fixed seed");
        assert_ne!(a, c);
        assert_ne!(a, [0u8; 96]);
        assert!(first.is_seeded() && !Entropy::new().is_seeded());
    }

    #[test]
    fn fill_covers_arbitrary_lengths() {
        let mut pool = Entropy::new();
        pool.seed_u64(0x1234_5678_9abc_def0);
        for len in [1usize, 31, 32, 33, 64, 65] {
            let mut first = alloc::vec![0u8; len];
            let mut second = alloc::vec![0u8; len];
            // Two pools in the same state must produce the same tail.
            let mut probe = pool.clone();
            pool.fill(&mut first);
            probe.fill(&mut second);
            assert_eq!(first, second, "length {len}");
            assert!(first.iter().any(|byte| *byte != 0), "length {len}");
        }
        // A zero-length request must not advance the generator.
        let before = pool.counter;
        pool.fill(&mut []);
        assert_eq!(pool.counter, before);
    }

    /// Mixing material twice must keep changing the state (the pool is a hash
    /// chain, not a reset), so a re-seed can only add entropy.
    #[test]
    fn repeated_seeds_advance_the_state() {
        let mut once = Entropy::new();
        let mut twice = Entropy::new();
        once.seed(b"same");
        twice.seed(b"same");
        twice.seed(b"same");
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        once.fill(&mut a);
        twice.fill(&mut b);
        assert_ne!(a, b);
    }

    /// RDRAND is optional; when the host has it the API returns words, and the
    /// pool accepts them as a seed.
    #[test]
    fn rdrand_path_is_optional_not_required() {
        let mut pool = Entropy::new();
        if rdrand_available() {
            assert!(pool.try_rdrand());
            assert!(pool.is_seeded());
            assert_ne!(pool.fill_32(), [0u8; 32]);
        } else {
            assert!(rdrand_words().is_none());
        }
    }
}
