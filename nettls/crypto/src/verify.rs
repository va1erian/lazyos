//! Signature verification for webpki (certificate chains) and for the TLS
//! handshake signature, mirroring the set rustls's own providers accept.
//!
//! Public-key input comes from certificates the server chose, so every
//! parser here rejects rather than repairs: SEC1 points must be on the
//! curve, ECDSA signatures must be DER, and RSA keys must be 2048-8192 bits
//! (as webpki's `RSA_*_2048_8192_*` algorithms require) with an exponent the
//! `rsa` crate accepts.

use p256::ecdsa::signature::hazmat::PrehashVerifier;
use rsa::traits::PublicKeyParts;
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{
    alg_id, AlgorithmIdentifier, InvalidSignature, SignatureVerificationAlgorithm,
};
use rustls::SignatureScheme;
use sha2::{Digest, Sha256, Sha384, Sha512};
use signature::Verifier;

/// Smallest and largest RSA moduli accepted, in bits.
const RSA_MIN_BITS: usize = 2048;
const RSA_MAX_BITS: usize = 8192;

#[derive(Clone, Copy, Debug)]
enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    fn digest(self, message: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha256 => Sha256::digest(message).to_vec(),
            Hash::Sha384 => Sha384::digest(message).to_vec(),
            Hash::Sha512 => Sha512::digest(message).to_vec(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Curve {
    P256,
    P384,
}

/// ECDSA over `curve` with `hash`, DER signatures (RFC 5480 / RFC 8446).
#[derive(Debug)]
struct Ecdsa {
    curve: Curve,
    hash: Hash,
    signature_alg: AlgorithmIdentifier,
}

impl SignatureVerificationAlgorithm for Ecdsa {
    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        let prehash = self.hash.digest(message);
        match self.curve {
            Curve::P256 => {
                let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(public_key)
                    .map_err(|_| InvalidSignature)?;
                let sig =
                    p256::ecdsa::DerSignature::try_from(signature).map_err(|_| InvalidSignature)?;
                let sig = p256::ecdsa::Signature::try_from(sig).map_err(|_| InvalidSignature)?;
                key.verify_prehash(&prehash, &sig)
                    .map_err(|_| InvalidSignature)
            }
            Curve::P384 => {
                let key = p384::ecdsa::VerifyingKey::from_sec1_bytes(public_key)
                    .map_err(|_| InvalidSignature)?;
                let sig =
                    p384::ecdsa::DerSignature::try_from(signature).map_err(|_| InvalidSignature)?;
                let sig = p384::ecdsa::Signature::try_from(sig).map_err(|_| InvalidSignature)?;
                key.verify_prehash(&prehash, &sig)
                    .map_err(|_| InvalidSignature)
            }
        }
    }

    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        match self.curve {
            Curve::P256 => alg_id::ECDSA_P256,
            Curve::P384 => alg_id::ECDSA_P384,
        }
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        self.signature_alg
    }
}

/// Ed25519 (RFC 8032), strict verification.
#[derive(Debug)]
struct Ed25519;

impl SignatureVerificationAlgorithm for Ed25519 {
    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        let key: [u8; 32] = public_key.try_into().map_err(|_| InvalidSignature)?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&key).map_err(|_| InvalidSignature)?;
        let sig = ed25519_dalek::Signature::from_slice(signature).map_err(|_| InvalidSignature)?;
        key.verify_strict(message, &sig)
            .map_err(|_| InvalidSignature)
    }

    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::ED25519
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::ED25519
    }
}

#[derive(Clone, Copy, Debug)]
enum Padding {
    Pkcs1,
    Pss,
}

/// RSA with `padding` and `hash` over an `rsaEncryption` key.
#[derive(Debug)]
struct Rsa {
    padding: Padding,
    hash: Hash,
    signature_alg: AlgorithmIdentifier,
}

/// Parse a PKCS#1 `RSAPublicKey` and enforce the size window.
fn rsa_key(der: &[u8]) -> Result<rsa::RsaPublicKey, InvalidSignature> {
    use rsa::pkcs1::der::Decode;
    let parsed = rsa::pkcs1::RsaPublicKey::from_der(der).map_err(|_| InvalidSignature)?;
    let n = rsa::BigUint::from_bytes_be(parsed.modulus.as_bytes());
    let e = rsa::BigUint::from_bytes_be(parsed.public_exponent.as_bytes());
    let key =
        rsa::RsaPublicKey::new_with_max_size(n, e, RSA_MAX_BITS).map_err(|_| InvalidSignature)?;
    if key.n().bits() < RSA_MIN_BITS {
        return Err(InvalidSignature);
    }
    Ok(key)
}

impl SignatureVerificationAlgorithm for Rsa {
    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        let key = rsa_key(public_key)?;
        let ok = match (self.padding, self.hash) {
            (Padding::Pkcs1, Hash::Sha256) => pkcs1::<Sha256>(key, message, signature),
            (Padding::Pkcs1, Hash::Sha384) => pkcs1::<Sha384>(key, message, signature),
            (Padding::Pkcs1, Hash::Sha512) => pkcs1::<Sha512>(key, message, signature),
            (Padding::Pss, Hash::Sha256) => pss::<Sha256>(key, message, signature),
            (Padding::Pss, Hash::Sha384) => pss::<Sha384>(key, message, signature),
            (Padding::Pss, Hash::Sha512) => pss::<Sha512>(key, message, signature),
        };
        ok.map_err(|_| InvalidSignature)
    }

    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_ENCRYPTION
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        self.signature_alg
    }
}

fn pkcs1<D>(
    key: rsa::RsaPublicKey,
    message: &[u8],
    signature: &[u8],
) -> Result<(), signature::Error>
where
    D: Digest + rsa::pkcs8::AssociatedOid,
{
    let sig = rsa::pkcs1v15::Signature::try_from(signature)?;
    rsa::pkcs1v15::VerifyingKey::<D>::new(key).verify(message, &sig)
}

fn pss<D>(key: rsa::RsaPublicKey, message: &[u8], signature: &[u8]) -> Result<(), signature::Error>
where
    D: Digest + sha2::digest::FixedOutputReset,
{
    let sig = rsa::pss::Signature::try_from(signature)?;
    // TLS fixes the salt length to the hash length (RFC 8446 §4.2.3).
    rsa::pss::VerifyingKey::<D>::new(key).verify(message, &sig)
}

static ECDSA_P256_SHA256: Ecdsa = Ecdsa {
    curve: Curve::P256,
    hash: Hash::Sha256,
    signature_alg: alg_id::ECDSA_SHA256,
};
static ECDSA_P256_SHA384: Ecdsa = Ecdsa {
    curve: Curve::P256,
    hash: Hash::Sha384,
    signature_alg: alg_id::ECDSA_SHA384,
};
static ECDSA_P384_SHA256: Ecdsa = Ecdsa {
    curve: Curve::P384,
    hash: Hash::Sha256,
    signature_alg: alg_id::ECDSA_SHA256,
};
static ECDSA_P384_SHA384: Ecdsa = Ecdsa {
    curve: Curve::P384,
    hash: Hash::Sha384,
    signature_alg: alg_id::ECDSA_SHA384,
};
static ED25519: Ed25519 = Ed25519;
static RSA_PKCS1_SHA256: Rsa = Rsa {
    padding: Padding::Pkcs1,
    hash: Hash::Sha256,
    signature_alg: alg_id::RSA_PKCS1_SHA256,
};
static RSA_PKCS1_SHA384: Rsa = Rsa {
    padding: Padding::Pkcs1,
    hash: Hash::Sha384,
    signature_alg: alg_id::RSA_PKCS1_SHA384,
};
static RSA_PKCS1_SHA512: Rsa = Rsa {
    padding: Padding::Pkcs1,
    hash: Hash::Sha512,
    signature_alg: alg_id::RSA_PKCS1_SHA512,
};
static RSA_PSS_SHA256: Rsa = Rsa {
    padding: Padding::Pss,
    hash: Hash::Sha256,
    signature_alg: alg_id::RSA_PSS_SHA256,
};
static RSA_PSS_SHA384: Rsa = Rsa {
    padding: Padding::Pss,
    hash: Hash::Sha384,
    signature_alg: alg_id::RSA_PSS_SHA384,
};
static RSA_PSS_SHA512: Rsa = Rsa {
    padding: Padding::Pss,
    hash: Hash::Sha512,
    signature_alg: alg_id::RSA_PSS_SHA512,
};

/// The algorithms webpki may use for chains, and the mapping from TLS
/// handshake signature schemes to them (as in rustls's ring provider).
pub static ALGORITHMS: WebPkiSupportedAlgorithms = WebPkiSupportedAlgorithms {
    all: &[
        &ECDSA_P256_SHA256,
        &ECDSA_P256_SHA384,
        &ECDSA_P384_SHA256,
        &ECDSA_P384_SHA384,
        &ED25519,
        &RSA_PSS_SHA256,
        &RSA_PSS_SHA384,
        &RSA_PSS_SHA512,
        &RSA_PKCS1_SHA256,
        &RSA_PKCS1_SHA384,
        &RSA_PKCS1_SHA512,
    ],
    mapping: &[
        // For TLS 1.2 the curve is not fixed by the scheme; for 1.3 it is.
        (
            SignatureScheme::ECDSA_NISTP384_SHA384,
            &[&ECDSA_P384_SHA384, &ECDSA_P256_SHA384],
        ),
        (
            SignatureScheme::ECDSA_NISTP256_SHA256,
            &[&ECDSA_P256_SHA256, &ECDSA_P384_SHA256],
        ),
        (SignatureScheme::ED25519, &[&ED25519]),
        (SignatureScheme::RSA_PSS_SHA512, &[&RSA_PSS_SHA512]),
        (SignatureScheme::RSA_PSS_SHA384, &[&RSA_PSS_SHA384]),
        (SignatureScheme::RSA_PSS_SHA256, &[&RSA_PSS_SHA256]),
        (SignatureScheme::RSA_PKCS1_SHA512, &[&RSA_PKCS1_SHA512]),
        (SignatureScheme::RSA_PKCS1_SHA384, &[&RSA_PKCS1_SHA384]),
        (SignatureScheme::RSA_PKCS1_SHA256, &[&RSA_PKCS1_SHA256]),
    ],
};

#[cfg(test)]
mod tests;
