//! Vetted cryptography for LazyOS, shared by `keyd` and the kernel tests
//! (issue #102).
//!
//! `docs/security-model.md` section 8 is the contract: `keyd` owns long-term
//! secrets, clients only ever ask for operations, and the primitives must come
//! from vetted crates rather than being home-grown. This crate is the one place
//! the primitive crates are chosen, configured and pinned:
//!
//! | Need | Primitive | Crate |
//! |---|---|---|
//! | Hashing, KATs, the RNG pool | SHA-256 (FIPS 180-4) | `sha2` (`force-soft`) |
//! | Tags, `Sign`, PRF | HMAC-SHA256 (RFC 2104) | `hmac` |
//! | Subkey derivation for `wrap` | HKDF-SHA256 (RFC 5869) | `hkdf` |
//! | Password verifiers | Argon2id (RFC 9106) | `argon2` |
//! | Wi-Fi: PSK to PMK | PBKDF2-HMAC-SHA1 (RFC 8018) | `pbkdf2`, `sha1` (`force-soft`) |
//! | Wi-Fi: PRF, EAPOL MIC | HMAC-SHA1 (RFC 2104) | `hmac`, `sha1` |
//! | Wi-Fi: AKM 6 MIC | AES-CMAC (RFC 4493) | `cmac`, `aes` |
//! | Wi-Fi: GTK delivery | AES key wrap (RFC 3394) | `aes-kw`, `aes` |
//!
//! All are pure Rust and build for `x86_64-unknown-none` and the host;
//! the `force-soft` backend on `sha2` and `sha1` is required by the pinned
//! toolchain (see `libs/crypto/Cargo.toml` and [`wrap`]). The Wi-Fi rows are
//! wrapped by [`wifi`]. `aes` has no such feature; see `Cargo.toml` for how it
//! is kept off the SIMD path.
//!
//! The crate is `no_std` for the freestanding targets and uses `std` only in
//! `#[cfg(test)]`. Run the known-answer vectors with:
//!
//! ```text
//! cargo test -p lazyos-crypto
//! ```

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod hex;
pub mod hmac;
pub mod kdf;
pub mod rng;
pub mod sha256;
pub mod wifi;
pub mod wrap;

/// A crypto operation failed for a caller-visible reason.
///
/// Messages stay short and non-specific where an attacker could use the
/// distinction: `Unwrap` reports [`Error::BadTag`] for both a wrong key and a
/// tampered blob.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// A key is shorter than the construction allows.
    KeyTooShort,
    /// A wrapped blob is shorter than its framing or names an unknown format.
    Malformed,
    /// A tag did not verify (wrong key or tampering).
    BadTag,
    /// A KDF parameter set or buffer is invalid.
    Kdf,
    /// An input has a length (or content) the construction does not allow.
    BadLength,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub const fn message(self) -> &'static str {
        match self {
            Error::KeyTooShort => "the key is too short for this operation",
            Error::Malformed => "the wrapped value is malformed",
            Error::BadTag => "the wrapped value failed authentication",
            Error::Kdf => "the key-derivation parameters are invalid",
            Error::BadLength => "an input has an invalid length or content",
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}
