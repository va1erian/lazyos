//! Password key derivation: **Argon2id** (RFC 9106), provided by the vetted
//! RustCrypto `argon2` crate.
//!
//! `docs/security-model.md` section 3 fixes the accounts design: password
//! verifiers are Argon2id and only `keyd` may verify them. This module is the
//! single place those parameters live, so kernel tests, host tests and `keyd`
//! agree.
//!
//! # Parameters
//!
//! Argon2id is the hybrid variant (side-channel-resistant first pass, then
//! data-independent like Argon2i); RFC 9106 recommends it for password
//! hashing. [`Params::INTERACTIVE`] scales RFC 9106 section 4's second
//! recommended option (t=3, p=4, 64 MiB) down to what LazyOS's native user
//! heap can hold:
//!
//! * `m_cost = 1024` KiB (1 MiB),
//! * `t_cost = 3`,
//! * `p_cost = 1` (LazyOS tasks are single-threaded today).
//!
//! The native heap is a bump allocator (`user/src/heap.rs`) with only about
//! 1.875 MiB of growth (`0x60_0000`..`0x7E_0000` in `kernel/src/process`), and
//! `Argon2::hash_password_into` allocates a fresh `m_cost`-sized block **per
//! call** that the bump allocator never reclaims. [`Arena`] is the fix on the
//! service side: it allocates the `m_cost` blocks once and reuses them for
//! every verification, so the cost is paid once at boot and the memory cost
//! parameter becomes the only limit. `keyd` uses the arena; the one-shot
//! [`argon2id`] helper is for tests and short-lived callers.
//!
//! Raising `m_cost` to the RFC's 64 MiB needs a real (reclaiming) allocator or
//! a larger native heap reservation; the wire format and call sites do not
//! change.
//!
//! # Argon2id versus PBKDF2
//!
//! The S3 issue allowed a PBKDF2-style fallback "if Argon2id does not
//! compile". It does compile for `x86_64-unknown-none` with the pinned crates,
//! so LazyOS uses Argon2id as the security model already promised; there is no
//! PBKDF2 path to document.

use alloc::vec;
use alloc::vec::Vec;

use argon2::{Algorithm, Argon2, Block, Params as Argon2Params, Version};

use crate::Error;

/// Verifier length the default parameters produce.
pub const VERIFIER_LEN: usize = 32;
/// Minimum salt length the Argon2 construction accepts (RFC 9106 requires at
/// least 8 bytes; `keyd` uses 16).
pub const MIN_SALT_LEN: usize = 8;

/// Argon2id cost parameters, the only tunable `keyd` and the tests share.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Params {
    /// Memory cost in KiB.
    pub m_cost_kib: u32,
    /// Time cost (passes).
    pub t_cost: u32,
    /// Parallelism (lanes).
    pub p_cost: u32,
}

impl Params {
    /// The interactive login parameters `keyd` uses (see the module docs).
    pub const INTERACTIVE: Params = Params {
        m_cost_kib: 1024,
        t_cost: 3,
        p_cost: 1,
    };

    /// RFC 9106 section 5.3's small test parameters (`m=32`, `t=3`, `p=4`),
    /// cheap enough for the host tests' known-answer work.
    pub const RFC9106_TEST: Params = Params {
        m_cost_kib: 32,
        t_cost: 3,
        p_cost: 4,
    };
}

/// Build the `argon2` parameter block, validating the cost values.
fn parameters(params: Params, output_len: usize) -> Result<Argon2Params, Error> {
    Argon2Params::new(
        params.m_cost_kib,
        params.t_cost,
        params.p_cost,
        Some(output_len),
    )
    .map_err(|_| Error::Kdf)
}

/// Derive a verifier from `password` and `salt` with Argon2id v1.3.
///
/// `out` gets the verifier; 32 bytes ([`VERIFIER_LEN`]) is what `keyd` stores.
/// Salt shorter than [`MIN_SALT_LEN`] or an empty `out` is [`Error::Kdf`].
///
/// Allocates the work memory for the call and drops it on return; a service
/// loop should hold an [`Arena`] instead (see the module docs).
pub fn argon2id(password: &[u8], salt: &[u8], params: Params, out: &mut [u8]) -> Result<(), Error> {
    if salt.len() < MIN_SALT_LEN || out.is_empty() {
        return Err(Error::Kdf);
    }
    let hasher = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        parameters(params, out.len())?,
    );
    hasher
        .hash_password_into(password, salt, out)
        .map_err(|_| Error::Kdf)
}

/// A reusable Argon2id work-memory arena.
///
/// The blocks a derivation fills are allocated once in [`Arena::new`] and
/// reused for every [`Arena::derive`] call, so a long-lived caller (like the
/// `keyd` service) pays the memory cost once instead of leaking one block per
/// verification under LazyOS's bump allocator. Each call still recomputes from
/// scratch: Argon2 has no state to carry between passwords.
pub struct Arena {
    hasher: Argon2<'static>,
    blocks: Vec<Block>,
}

impl Arena {
    /// Allocate the arena for `params`. Fails with [`Error::Kdf`] when the
    /// parameters are invalid or the allocation cannot be satisfied.
    pub fn new(params: Params) -> Result<Arena, Error> {
        let params = parameters(params, VERIFIER_LEN)?;
        let hasher = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let count = hasher.params().block_count();
        let blocks = vec![Block::default(); count];
        Ok(Arena { hasher, blocks })
    }

    /// The number of 1 KiB work blocks this arena owns (the effective memory
    /// cost).
    pub fn blocks(&self) -> usize {
        self.blocks.len()
    }

    /// Derive a verifier into `out`, reusing this arena's work memory.
    pub fn derive(&mut self, password: &[u8], salt: &[u8], out: &mut [u8]) -> Result<(), Error> {
        if salt.len() < MIN_SALT_LEN || out.is_empty() {
            return Err(Error::Kdf);
        }
        self.hasher
            .hash_password_into_with_memory(password, salt, out, self.blocks.as_mut_slice())
            .map_err(|_| Error::Kdf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Argon2id is deterministic for fixed inputs, and a different salt or
    /// password changes the verifier.
    #[test]
    fn deterministic_and_salt_bound() {
        let mut first = [0u8; VERIFIER_LEN];
        let mut again = [0u8; VERIFIER_LEN];
        let mut other_salt = [0u8; VERIFIER_LEN];
        let mut other_password = [0u8; VERIFIER_LEN];
        let salt_a = [0x11u8; 16];
        let salt_b = [0x22u8; 16];
        argon2id(b"correct horse", &salt_a, Params::RFC9106_TEST, &mut first).unwrap();
        argon2id(b"correct horse", &salt_a, Params::RFC9106_TEST, &mut again).unwrap();
        argon2id(
            b"correct horse",
            &salt_b,
            Params::RFC9106_TEST,
            &mut other_salt,
        )
        .unwrap();
        argon2id(
            b"battery staple",
            &salt_a,
            Params::RFC9106_TEST,
            &mut other_password,
        )
        .unwrap();
        assert_eq!(first, again);
        assert_ne!(first, other_salt);
        assert_ne!(first, other_password);
        assert_ne!(first, [0u8; VERIFIER_LEN], "verifier must not be all-zero");
    }

    /// The cost parameters feed through: a different memory cost gives a
    /// different verifier (guards against a silently ignored parameter).
    #[test]
    fn cost_parameters_matter() {
        let mut cheap = [0u8; VERIFIER_LEN];
        let mut dear = [0u8; VERIFIER_LEN];
        let salt = [0x33u8; 16];
        argon2id(
            b"pass",
            &salt,
            Params {
                m_cost_kib: 32,
                t_cost: 1,
                p_cost: 1,
            },
            &mut cheap,
        )
        .unwrap();
        argon2id(
            b"pass",
            &salt,
            Params {
                m_cost_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
            &mut dear,
        )
        .unwrap();
        assert_ne!(cheap, dear);
    }

    /// The reusable arena produces exactly the one-shot output for the same
    /// inputs, and repeated derivations stay independent (a reused buffer
    /// cannot feed the previous password's state into the next).
    #[test]
    fn arena_matches_oneshot_and_reuses_cleanly() {
        let params = Params::RFC9106_TEST;
        let mut arena = Arena::new(params).unwrap();
        assert_eq!(arena.blocks(), params.m_cost_kib as usize);
        let mut one = [0u8; VERIFIER_LEN];
        let mut reused_first = [0u8; VERIFIER_LEN];
        let mut reused_second = [0u8; VERIFIER_LEN];
        let salt = [0x44u8; 16];
        argon2id(b"first", &salt, params, &mut one).unwrap();
        arena.derive(b"first", &salt, &mut reused_first).unwrap();
        arena.derive(b"second", &salt, &mut reused_second).unwrap();
        assert_eq!(one, reused_first);
        assert_ne!(reused_first, reused_second);
        // A fresh arena agrees after the first arena has served two calls.
        let mut fresh = Arena::new(params).unwrap();
        let mut standalone = [0u8; VERIFIER_LEN];
        fresh.derive(b"second", &salt, &mut standalone).unwrap();
        assert_eq!(standalone, reused_second);
    }

    /// Short salts and empty outputs are refused before any work happens.
    #[test]
    fn rejects_bad_inputs() {
        let mut out = [0u8; VERIFIER_LEN];
        assert_eq!(
            argon2id(b"pass", b"short", Params::RFC9106_TEST, &mut out),
            Err(Error::Kdf)
        );
        assert_eq!(
            argon2id(b"pass", &[0u8; 16], Params::RFC9106_TEST, &mut []),
            Err(Error::Kdf)
        );
    }

    /// Implementation-stability pin: the exact verifier this crate's pinned
    /// `argon2` produces for fixed inputs. RFC 9106 section 5.3's vector uses
    /// associated data, which the high-level `hash_password_into` API does not
    /// expose, so this is a regression pin (generated once from `argon2`
    /// 0.5.3, not copied from another library) rather than the RFC vector: it
    /// fails loudly on a dependency or parameter change.
    #[test]
    fn implementation_stability_vector() {
        let mut out = [0u8; VERIFIER_LEN];
        argon2id(b"password", &[0x02u8; 16], Params::RFC9106_TEST, &mut out).unwrap();
        assert_eq!(
            crate::hex::encode(&out),
            "4626bbbb6f6a794d2e2eeae6dd7ccdd25c76f273cffc5ba5732b187339e4c1b6"
        );
    }
}
