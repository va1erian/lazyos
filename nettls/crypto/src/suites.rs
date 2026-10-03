//! The cipher suites: AEAD-only, ECDHE-only, as modern servers offer them.

use rustls::crypto::tls12::PrfUsingHmac;
use rustls::crypto::tls13::HkdfUsingHmac;
use rustls::crypto::CipherSuiteCommon;
use rustls::{
    CipherSuite, SignatureScheme, SupportedCipherSuite, Tls12CipherSuite, Tls13CipherSuite,
};

use crate::aead;
use crate::hash::{SHA256, SHA384};
use crate::hmac::{HMAC_SHA256, HMAC_SHA384};

/// Preference order: TLS 1.3 first, AES-GCM before ChaCha20 (AES-NI is
/// usable on LazyOS), then TLS 1.2 ECDSA before RSA.
pub static ALL: &[SupportedCipherSuite] = &[
    TLS13_AES_256_GCM_SHA384,
    TLS13_AES_128_GCM_SHA256,
    TLS13_CHACHA20_POLY1305_SHA256,
    TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
];

// RFC 8446 §5.5 limits for AES-GCM (records before rekeying); ChaCha20 has
// no practical limit.
const GCM_LIMIT: u64 = 1 << 24;

pub static TLS13_AES_128_GCM_SHA256: SupportedCipherSuite =
    SupportedCipherSuite::Tls13(&Tls13CipherSuite {
        common: CipherSuiteCommon {
            suite: CipherSuite::TLS13_AES_128_GCM_SHA256,
            hash_provider: &SHA256,
            confidentiality_limit: GCM_LIMIT,
        },
        hkdf_provider: &HkdfUsingHmac(&HMAC_SHA256),
        aead_alg: &aead::TLS13_AES_128_GCM,
        quic: None,
    });

pub static TLS13_AES_256_GCM_SHA384: SupportedCipherSuite =
    SupportedCipherSuite::Tls13(&Tls13CipherSuite {
        common: CipherSuiteCommon {
            suite: CipherSuite::TLS13_AES_256_GCM_SHA384,
            hash_provider: &SHA384,
            confidentiality_limit: GCM_LIMIT,
        },
        hkdf_provider: &HkdfUsingHmac(&HMAC_SHA384),
        aead_alg: &aead::TLS13_AES_256_GCM,
        quic: None,
    });

pub static TLS13_CHACHA20_POLY1305_SHA256: SupportedCipherSuite =
    SupportedCipherSuite::Tls13(&Tls13CipherSuite {
        common: CipherSuiteCommon {
            suite: CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
            hash_provider: &SHA256,
            confidentiality_limit: u64::MAX,
        },
        hkdf_provider: &HkdfUsingHmac(&HMAC_SHA256),
        aead_alg: &aead::TLS13_CHACHA20_POLY1305,
        quic: None,
    });

const ECDSA_SCHEMES: &[SignatureScheme] = &[
    SignatureScheme::ED25519,
    SignatureScheme::ECDSA_NISTP384_SHA384,
    SignatureScheme::ECDSA_NISTP256_SHA256,
];

const RSA_SCHEMES: &[SignatureScheme] = &[
    SignatureScheme::RSA_PSS_SHA512,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA256,
    SignatureScheme::RSA_PKCS1_SHA512,
    SignatureScheme::RSA_PKCS1_SHA384,
    SignatureScheme::RSA_PKCS1_SHA256,
];

/// One TLS 1.2 ECDHE suite.
macro_rules! tls12 {
    ($name:ident, $suite:ident, $hash:expr, $hmac:expr, $schemes:expr, $aead:expr, $limit:expr) => {
        pub static $name: SupportedCipherSuite = SupportedCipherSuite::Tls12(&Tls12CipherSuite {
            common: CipherSuiteCommon {
                suite: CipherSuite::$suite,
                hash_provider: $hash,
                confidentiality_limit: $limit,
            },
            kx: rustls::crypto::KeyExchangeAlgorithm::ECDHE,
            sign: $schemes,
            aead_alg: $aead,
            prf_provider: &PrfUsingHmac($hmac),
        });
    };
}

tls12!(
    TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    &SHA256,
    &HMAC_SHA256,
    ECDSA_SCHEMES,
    &aead::TLS12_AES_128_GCM,
    GCM_LIMIT
);
tls12!(
    TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    &SHA384,
    &HMAC_SHA384,
    ECDSA_SCHEMES,
    &aead::TLS12_AES_256_GCM,
    GCM_LIMIT
);
tls12!(
    TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    &SHA256,
    &HMAC_SHA256,
    ECDSA_SCHEMES,
    &aead::TLS12_CHACHA20_POLY1305,
    u64::MAX
);
tls12!(
    TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    &SHA256,
    &HMAC_SHA256,
    RSA_SCHEMES,
    &aead::TLS12_AES_128_GCM,
    GCM_LIMIT
);
tls12!(
    TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    &SHA384,
    &HMAC_SHA384,
    RSA_SCHEMES,
    &aead::TLS12_AES_256_GCM,
    GCM_LIMIT
);
tls12!(
    TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    &SHA256,
    &HMAC_SHA256,
    RSA_SCHEMES,
    &aead::TLS12_CHACHA20_POLY1305,
    u64::MAX
);
