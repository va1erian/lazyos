//! A rustls `CryptoProvider` built only from pure-Rust RustCrypto crates.
//!
//! Why not ring or aws-lc: a GPL-2.0-only program (a future NetSurf port)
//! will link this TLS stack, and neither of those is available under a
//! GPLv2-compatible licence. Why not `rustls-rustcrypto`: it is an alpha,
//! verifies RSA signatures from keys of any size, and pulls signing, PKCS#5
//! and QUIC code a client does not need. This crate is the small
//! (unaudited) subset docs/tls-plan.md §2 asks for, modelled on rustls's
//! `provider-example`:
//!
//! | Piece | Algorithms | Crates |
//! |---|---|---|
//! | Suites | TLS 1.3 AES-128/256-GCM, ChaCha20-Poly1305; TLS 1.2 ECDHE-ECDSA/RSA with the same AEADs | `aes-gcm`, `chacha20poly1305` |
//! | Key exchange | X25519, P-256, P-384 | `x25519-dalek`, `p256`, `p384` |
//! | Signatures (verify) | ECDSA P-256/P-384 (SHA-256/384), Ed25519, RSA PKCS#1 v1.5 and PSS (SHA-256/384/512, 2048-8192-bit keys) | `p256`, `p384`, `ed25519-dalek`, `rsa` |
//! | Hash, HMAC, HKDF, TLS 1.2 PRF | SHA-256, SHA-384 | `sha2`, `hmac`, rustls's generic HKDF/PRF |
//! | Randomness | the kernel (`getrandom`) | `getrandom` |
//!
//! CPU dispatch: AES, GHASH/POLYVAL, ChaCha20, SHA-2 and curve25519 pick
//! their SIMD backends at run time through the `cpufeatures` crate. Its AVX
//! and AVX2 checks require CPUID.1:ECX.XSAVE and OSXSAVE (bits 26 and 27) and
//! then XCR0's XMM/YMM bits (`__xgetbv!` in `cpufeatures` 0.2.17 `x86.rs`), so
//! on LazyOS, which saves state with FXSAVE and leaves CR4.OSXSAVE clear,
//! every AVX/AVX2 backend is rejected and the SSE2/SSSE3/AES-NI/PCLMULQDQ
//! (XMM-only) or portable code runs. The ABI fixture `tlsfix` checks this
//! on LazyOS.

mod aead;
mod hash;
mod hmac;
mod kx;
#[cfg(feature = "server")]
pub mod sign;
mod suites;
mod verify;

use std::sync::Arc;

use rustls::crypto::{CryptoProvider, GetRandomFailed, KeyProvider, SecureRandom};
use rustls::pki_types::PrivateKeyDer;

pub use suites::{
    TLS13_AES_128_GCM_SHA256, TLS13_AES_256_GCM_SHA384, TLS13_CHACHA20_POLY1305_SHA256,
    TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256, TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256, TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384, TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
};

/// The provider: every suite, group and verification algorithm above.
pub fn provider() -> CryptoProvider {
    CryptoProvider {
        cipher_suites: suites::ALL.to_vec(),
        kx_groups: kx::ALL.to_vec(),
        signature_verification_algorithms: verify::ALGORITHMS,
        secure_random: &Provider,
        key_provider: &Provider,
    }
}

/// The provider as an `Arc`, as rustls's config builders take it.
pub fn provider_arc() -> Arc<CryptoProvider> {
    Arc::new(provider())
}

#[derive(Debug)]
struct Provider;

impl SecureRandom for Provider {
    fn fill(&self, buf: &mut [u8]) -> Result<(), GetRandomFailed> {
        getrandom::getrandom(buf).map_err(|_| GetRandomFailed)
    }
}

impl KeyProvider for Provider {
    fn load_private_key(
        &self,
        key: PrivateKeyDer<'static>,
    ) -> Result<Arc<dyn rustls::sign::SigningKey>, rustls::Error> {
        #[cfg(feature = "server")]
        {
            sign::load(key)
        }
        #[cfg(not(feature = "server"))]
        {
            let _ = key;
            Err(rustls::Error::General(
                "this build has no signing keys (client only)".into(),
            ))
        }
    }
}
