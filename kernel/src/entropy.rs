//! Kernel entropy pool and CSPRNG behind `getrandom(2)` and `AT_RANDOM`
//! (issue #232; `docs/security-model.md` section 8).
//!
//! The generator is ChaCha20 (RFC 8439) with fast key erasure: every request
//! expands the current 256-bit key into keystream, then a reserved block
//! becomes the *next* key, so a later compromise of the state cannot reveal
//! earlier output. The nonce carries a per-call counter and a fresh TSC
//! sample, so no two requests ever see the same keystream, and nothing is
//! derived from the tick counter.
//!
//! Seeding sources:
//!
//! * `RDSEED`/`RDRAND` when CPUID reports them (four 64-bit words per seed),
//! * TSC timing jitter, always mixed in as a supplement and used alone on
//!   CPUs without a hardware generator (e.g. QEMU's default `qemu64` model).
//!   Jitter is the weakest source: on such a machine the output is best
//!   effort, not a substitute for hardware entropy.
//!
//! The pool is seeded synchronously on first use (so `getrandom` never blocks
//! or returns `EAGAIN` at this stage) and re-keyed with fresh entropy every
//! [`RESEED_TICKS`] ticks or [`RESEED_CALLS`] requests, whichever comes first.
//! New entropy is XOR-folded into the key, which can add but never remove
//! unpredictability.

use spin::Mutex;

/// Reseed at least once per second of kernel ticks (100 Hz)...
const RESEED_TICKS: u64 = 100;

/// ...or after this many requests, whichever comes first.
const RESEED_CALLS: u32 = 256;

/// Longest keystream span produced under one key (bounds the 32-bit block
/// counter and the time the pool lock is held).
const MAX_SPAN: usize = 4096;

/// TSC jitter samples folded per reseed.
const JITTER_SAMPLES: usize = 256;

/// The four ChaCha constants, "expand 32-byte k".
const SIGMA: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

struct Pool {
    key: [u8; 32],
    calls: u64,
    calls_since_reseed: u32,
    last_reseed_tick: u64,
    reseeds: u64,
    seeded: bool,
}

static POOL: Mutex<Pool> = Mutex::new(Pool {
    key: [0; 32],
    calls: 0,
    calls_since_reseed: 0,
    last_reseed_tick: 0,
    reseeds: 0,
    seeded: false,
});

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One ChaCha20 block (RFC 8439 section 2.3): 64 bytes of keystream.
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut init = [0u32; 16];
    init[..4].copy_from_slice(&SIGMA);
    for (word, chunk) in init[4..12].iter_mut().zip(key.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*chunk);
    }
    init[12] = counter;
    for (word, chunk) in init[13..16].iter_mut().zip(nonce.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*chunk);
    }
    let mut s = init;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for ((chunk, word), start) in out.as_chunks_mut::<4>().0.iter_mut().zip(s).zip(init) {
        chunk.copy_from_slice(&word.wrapping_add(start).to_le_bytes());
    }
    out
}

fn tsc() -> u64 {
    // SAFETY: `rdtsc` is unprivileged, has no memory operands and no
    // preconditions on x86_64.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Whether CPUID advertises RDRAND (leaf 1 ECX bit 30).
fn has_rdrand() -> bool {
    core::arch::x86_64::__cpuid(1).ecx & (1 << 30) != 0
}

/// Whether CPUID advertises RDSEED (leaf 7 EBX bit 18).
fn has_rdseed() -> bool {
    // Leaf 7 is only queried after leaf 0 confirms it is supported.
    core::arch::x86_64::__cpuid(0).eax >= 7
        && core::arch::x86_64::__cpuid_count(7, 0).ebx & (1 << 18) != 0
}

/// One hardware random word, or `None` when the unit is absent or keeps
/// reporting failure over a bounded number of retries.
fn hardware_word(rdseed: bool, rdrand: bool) -> Option<u64> {
    let mut value = 0u64;
    if rdseed {
        for _ in 0..10 {
            // SAFETY: CPUID reported RDSEED; the intrinsic writes one word
            // through a valid `&mut u64`.
            if unsafe { core::arch::x86_64::_rdseed64_step(&mut value) } == 1 {
                return Some(value);
            }
        }
    }
    // RDSEED can fail transiently under contention; RDRAND is the fallback.
    if rdrand {
        for _ in 0..10 {
            // SAFETY: CPUID reported RDRAND; the intrinsic writes one word
            // through a valid `&mut u64`.
            if unsafe { core::arch::x86_64::_rdrand64_step(&mut value) } == 1 {
                return Some(value);
            }
        }
    }
    None
}

/// Gather 32 bytes of fresh entropy from hardware (when present) and jitter.
fn gather() -> [u8; 32] {
    let (rdseed, rdrand) = (has_rdseed(), has_rdrand());
    let mut acc = [0u64; 4];
    for slot in acc.iter_mut() {
        if let Some(word) = hardware_word(rdseed, rdrand) {
            *slot = word;
        }
    }
    // Timing jitter: the TSC delta around a little variable work (interrupts,
    // cache and bus contention make the low bits unpredictable), rotated into
    // every lane and then diffused by ChaCha below.
    let mut previous = tsc();
    for i in 0..JITTER_SAMPLES {
        for _ in 0..(i % 7) + 1 {
            core::hint::spin_loop();
        }
        let now = tsc();
        let lane = i % 4;
        acc[lane] = acc[lane].rotate_left(13) ^ now.wrapping_sub(previous) ^ now.rotate_left(32);
        previous = now;
    }
    let mut folded = [0u8; 32];
    for (chunk, word) in folded.as_chunks_mut::<8>().0.iter_mut().zip(acc) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    let block = chacha20_block(&folded, 0, &[0x5e; 12]);
    let mut out = [0u8; 32];
    out.copy_from_slice(&block[..32]);
    out
}

impl Pool {
    fn reseed(&mut self) {
        for (byte, new) in self.key.iter_mut().zip(gather()) {
            *byte ^= new;
        }
        self.reseeds += 1;
        self.calls_since_reseed = 0;
        self.last_reseed_tick = crate::task::ticks();
        // A jitter-only pool (no RDSEED/RDRAND) still counts as seeded, see
        // the module docs; otherwise getrandom would stall forever there.
        self.seeded = true;
    }

    fn due(&self) -> bool {
        !self.seeded
            || self.calls_since_reseed >= RESEED_CALLS
            || crate::task::ticks().wrapping_sub(self.last_reseed_tick) >= RESEED_TICKS
    }

    fn fill(&mut self, buffer: &mut [u8]) {
        if self.due() {
            self.reseed();
        }
        self.calls += 1;
        self.calls_since_reseed += 1;
        let mut nonce = [0u8; 12];
        nonce[..8].copy_from_slice(&self.calls.to_le_bytes());
        nonce[8..].copy_from_slice(&(tsc() as u32).to_le_bytes());
        // Block 0 is reserved for the next key; output starts at block 1.
        for (counter, chunk) in (1u32..).zip(buffer.chunks_mut(64)) {
            let block = chacha20_block(&self.key, counter, &nonce);
            chunk.copy_from_slice(&block[..chunk.len()]);
        }
        let next = chacha20_block(&self.key, 0, &nonce);
        self.key.copy_from_slice(&next[..32]);
    }
}

/// Fill `buffer` with cryptographically strong random bytes.
///
/// Seeds the pool on first use. Never blocks and never fails; see the module
/// docs for the strength on CPUs without a hardware generator.
pub fn fill(buffer: &mut [u8]) {
    for span in buffer.chunks_mut(MAX_SPAN) {
        POOL.lock().fill(span);
    }
}

/// How many times the pool has been re-keyed with fresh entropy.
#[cfg(lazyos_tests)]
pub fn reseed_count() -> u64 {
    POOL.lock().reseeds
}

/// Force an immediate reseed (test hook for the reseed path).
#[cfg(lazyos_tests)]
pub fn force_reseed() {
    POOL.lock().reseed();
}
