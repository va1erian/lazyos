//! P-256 ECDH in the form Bluetooth LE Secure Connections uses
//! (Core Specification Vol 3, Part H, 2.3.5.6.1; docs/bluetooth-plan.md B4).
//!
//! SMP sends a public key as `X || Y`, each coordinate a 32-byte
//! **little-endian** integer, and treats the DH key the same way. SEC1 (and
//! RustCrypto `p256`) are big-endian, so every crossing reverses each 32-byte
//! coordinate; [`le_to_be`] is the one place that does it.
//!
//! A peer public key is attacker input. A point that is not on the curve lets
//! a peer steer the shared secret into a small subgroup (the "invalid curve"
//! attack on SMP, CVE-2018-5383); `PublicKey::from_encoded_point` checks the
//! curve equation and rejects the identity, and [`PeerPublicKey::from_smp`]
//! is the only way to build the type, so [`KeyPair::dh_key`] cannot see an
//! unchecked point.

use p256::ecdh::diffie_hellman;
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{EncodedPoint, PublicKey, SecretKey};
use zeroize::Zeroizing;

use crate::rng::Entropy;

/// One coordinate or the DH key, in bytes.
pub const COORD_LEN: usize = 32;
/// A public key as SMP sends it: `X || Y`, each little-endian.
pub const PUBLIC_KEY_LEN: usize = 64;

/// Why a key operation failed. Deliberately not `crate::Error`: callers match
/// on these to pick the SMP failure reason (`Invalid Parameters`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EcdhError {
    /// The peer's point is not on P-256 (or is the identity).
    InvalidPublicKey,
    /// The RNG has no seed material; a key from it would be predictable.
    Unseeded,
}

/// A peer's public key that is known to be a valid point on P-256.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PeerPublicKey(PublicKey);

impl PeerPublicKey {
    /// Validate a key received over SMP (`X || Y`, little-endian).
    pub fn from_smp(bytes: &[u8; PUBLIC_KEY_LEN]) -> Result<PeerPublicKey, EcdhError> {
        let (x, y) = bytes.split_at(COORD_LEN);
        let x = le_to_be(x);
        let y = le_to_be(y);
        let point = EncodedPoint::from_affine_coordinates(&x.into(), &y.into(), false);
        // `from_encoded_point` checks the curve equation; the identity cannot
        // be written in uncompressed form, and a coordinate >= p is refused.
        Option::<PublicKey>::from(PublicKey::from_encoded_point(&point))
            .map(PeerPublicKey)
            .ok_or(EcdhError::InvalidPublicKey)
    }

    /// The key in SMP form.
    pub fn to_smp(&self) -> [u8; PUBLIC_KEY_LEN] {
        public_to_smp(&self.0)
    }
}

/// An ephemeral P-256 key pair. The secret is zeroized on drop by `p256`.
pub struct KeyPair {
    secret: SecretKey,
}

// Hand-written so a `{:?}` can never print the scalar.
impl core::fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("KeyPair(<redacted>)")
    }
}

impl KeyPair {
    /// Draw a fresh pair from `rng`.
    pub fn generate(rng: &mut Entropy) -> Result<KeyPair, EcdhError> {
        if !rng.is_seeded() {
            return Err(EcdhError::Unseeded);
        }
        loop {
            let bytes = Zeroizing::new(rng.fill_32());
            // Zero and values >= the group order are refused (probability
            // about 2^-32); draw again rather than reduce, which would bias.
            if let Ok(secret) = SecretKey::from_slice(&bytes[..]) {
                return Ok(KeyPair { secret });
            }
        }
    }

    /// Build a pair from a big-endian secret scalar (test vectors and the
    /// peripheral in tests; production code uses [`KeyPair::generate`]).
    pub fn from_secret_be(bytes: &[u8; COORD_LEN]) -> Result<KeyPair, EcdhError> {
        SecretKey::from_slice(bytes)
            .map(|secret| KeyPair { secret })
            .map_err(|_| EcdhError::InvalidPublicKey)
    }

    /// Our public key in SMP form.
    pub fn public_smp(&self) -> [u8; PUBLIC_KEY_LEN] {
        public_to_smp(&self.secret.public_key())
    }

    /// The DH key with a validated peer key, 32 bytes little-endian as SMP
    /// feeds it to `f5`/`f6`. Zeroized when dropped.
    pub fn dh_key(&self, peer: &PeerPublicKey) -> Zeroizing<[u8; COORD_LEN]> {
        let shared = diffie_hellman(self.secret.to_nonzero_scalar(), peer.0.as_affine());
        let mut out = Zeroizing::new([0u8; COORD_LEN]);
        out.copy_from_slice(shared.raw_secret_bytes());
        out.reverse();
        out
    }
}

fn public_to_smp(key: &PublicKey) -> [u8; PUBLIC_KEY_LEN] {
    let point = key.to_encoded_point(false);
    let mut out = [0u8; PUBLIC_KEY_LEN];
    // Uncompressed SEC1 is 0x04 || X || Y; the identity never reaches here.
    out[..COORD_LEN].copy_from_slice(point.x().expect("finite point"));
    out[COORD_LEN..].copy_from_slice(point.y().expect("uncompressed point"));
    out[..COORD_LEN].reverse();
    out[COORD_LEN..].reverse();
    out
}

fn le_to_be(coord: &[u8]) -> [u8; COORD_LEN] {
    let mut out = [0u8; COORD_LEN];
    out.copy_from_slice(coord);
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }

    fn rng() -> Entropy {
        let mut rng = Entropy::new();
        rng.seed(b"p256 tests");
        rng
    }

    /// Core Specification Vol 3 Part H D.1 (ECDH sample data), the secret
    /// scalars and the expected DH key, big-endian as the spec prints them.
    #[test]
    fn spec_sample_dh_key() {
        let a = KeyPair::from_secret_be(&hex32(
            "3f49f6d4a3c55f3874c9b3e3d2103f504aff607beb40b7995899b8a6cd3c1abd",
        ))
        .unwrap();
        let b = KeyPair::from_secret_be(&hex32(
            "55188b3d32f6bb9a900afcfbeed4e72a59cb9ac2f19d7cfb6b4fdd49f47fc5fd",
        ))
        .unwrap();
        let want = hex32("ec0234a357c8ad05341010a60a397d9b99796b13b4f866f1868d34f373bfa698");
        let a_pub = a.public_smp();
        let mut x = [0u8; 32];
        x.copy_from_slice(&a_pub[..32]);
        x.reverse();
        assert_eq!(
            x,
            hex32("20b003d2f297be2c5e2c83a7e9f9a5b9eff49111acf4fddbcc0301480e359de6")
        );
        let k_ab = a.dh_key(&PeerPublicKey::from_smp(&b.public_smp()).unwrap());
        let k_ba = b.dh_key(&PeerPublicKey::from_smp(&a_pub).unwrap());
        assert_eq!(*k_ab, *k_ba);
        let mut be = *k_ab;
        be.reverse();
        assert_eq!(be, want);
    }

    #[test]
    fn round_trip_with_generated_keys() {
        let mut rng = rng();
        let a = KeyPair::generate(&mut rng).unwrap();
        let b = KeyPair::generate(&mut rng).unwrap();
        let ka = a.dh_key(&PeerPublicKey::from_smp(&b.public_smp()).unwrap());
        let kb = b.dh_key(&PeerPublicKey::from_smp(&a.public_smp()).unwrap());
        assert_eq!(*ka, *kb);
        assert_ne!(*ka, [0u8; 32]);
        assert_ne!(a.public_smp(), b.public_smp());
    }

    /// CVE-2018-5383: a point off the curve must never reach the multiply.
    #[test]
    fn rejects_points_not_on_the_curve() {
        let mut rng = rng();
        let good = KeyPair::generate(&mut rng).unwrap().public_smp();
        assert!(PeerPublicKey::from_smp(&good).is_ok());
        // Flip one bit of Y: almost surely off the curve.
        let mut bad = good;
        bad[32] ^= 1;
        assert_eq!(
            PeerPublicKey::from_smp(&bad),
            Err(EcdhError::InvalidPublicKey)
        );
        // The origin and all-ones are not on P-256.
        for fill in [0x00u8, 0xff] {
            assert_eq!(
                PeerPublicKey::from_smp(&[fill; 64]),
                Err(EcdhError::InvalidPublicKey)
            );
        }
        let mut tiny = [0u8; 64];
        tiny[32] = 1;
        assert_eq!(
            PeerPublicKey::from_smp(&tiny),
            Err(EcdhError::InvalidPublicKey)
        );
    }

    #[test]
    fn unseeded_rng_is_refused_and_debug_is_redacted() {
        assert_eq!(
            KeyPair::generate(&mut Entropy::new()).unwrap_err(),
            EcdhError::Unseeded
        );
        let pair = KeyPair::generate(&mut rng()).unwrap();
        assert_eq!(alloc::format!("{pair:?}"), "KeyPair(<redacted>)");
    }
}
