//! Ephemeral key exchange: X25519, P-256 and P-384 (ECDHE).
//!
//! Each `start` draws a fresh secret from the kernel's CSPRNG; the secret is
//! zeroised on drop by the curve crates.

use rand_core::OsRng;
use rustls::crypto::{ActiveKeyExchange, SharedSecret, SupportedKxGroup};
use rustls::{Error, NamedGroup, PeerMisbehaved};

/// The groups offered, in preference order (X25519 first, as browsers do).
pub static ALL: &[&dyn SupportedKxGroup] = &[&X25519, &SECP256R1, &SECP384R1];

fn bad_share() -> Error {
    Error::PeerMisbehaved(PeerMisbehaved::InvalidKeyShare)
}

#[derive(Debug)]
pub struct X25519;

impl SupportedKxGroup for X25519 {
    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
        let secret = x25519_dalek::EphemeralSecret::random_from_rng(OsRng);
        let public = x25519_dalek::PublicKey::from(&secret);
        Ok(Box::new(X25519Exchange {
            secret,
            public: public.to_bytes(),
        }))
    }

    fn name(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

struct X25519Exchange {
    secret: x25519_dalek::EphemeralSecret,
    public: [u8; 32],
}

impl ActiveKeyExchange for X25519Exchange {
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
        let peer: [u8; 32] = peer.try_into().map_err(|_| bad_share())?;
        let shared = self
            .secret
            .diffie_hellman(&x25519_dalek::PublicKey::from(peer));
        // RFC 7748 §6.1 / RFC 8446 §7.4.2: an all-zero result means the
        // peer sent a low-order point; abort.
        if !shared.was_contributory() {
            return Err(bad_share());
        }
        Ok(SharedSecret::from(&shared.as_bytes()[..]))
    }

    fn pub_key(&self) -> &[u8] {
        &self.public
    }

    fn group(&self) -> NamedGroup {
        NamedGroup::X25519
    }
}

/// ECDHE over one of the NIST curves the `elliptic-curve` crates provide.
macro_rules! nist_group {
    ($group:ident, $exchange:ident, $curve:ident, $name:expr) => {
        #[derive(Debug)]
        pub struct $group;

        impl SupportedKxGroup for $group {
            fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
                let secret = $curve::ecdh::EphemeralSecret::random(&mut OsRng);
                // Uncompressed SEC1 point, as TLS requires.
                let public = $curve::EncodedPoint::from(secret.public_key());
                Ok(Box::new($exchange {
                    secret,
                    public: public.as_bytes().to_vec(),
                }))
            }

            fn name(&self) -> NamedGroup {
                $name
            }
        }

        struct $exchange {
            secret: $curve::ecdh::EphemeralSecret,
            public: Vec<u8>,
        }

        impl ActiveKeyExchange for $exchange {
            fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
                // RFC 8446 §4.2.8.2: only the uncompressed form is valid.
                if peer.first() != Some(&0x04) {
                    return Err(bad_share());
                }
                // `from_sec1_bytes` rejects points not on the curve and the
                // identity, so no invalid-curve attack reaches the scalar.
                let point = $curve::PublicKey::from_sec1_bytes(peer).map_err(|_| bad_share())?;
                let shared = self.secret.diffie_hellman(&point);
                Ok(SharedSecret::from(&shared.raw_secret_bytes()[..]))
            }

            fn pub_key(&self) -> &[u8] {
                &self.public
            }

            fn group(&self) -> NamedGroup {
                $name
            }
        }
    };
}

nist_group!(Secp256r1, Secp256r1Exchange, p256, NamedGroup::secp256r1);
nist_group!(Secp384r1, Secp384r1Exchange, p384, NamedGroup::secp384r1);

pub static SECP256R1: Secp256r1 = Secp256r1;
pub static SECP384R1: Secp384r1 = Secp384r1;

#[cfg(test)]
mod tests {
    use super::*;

    fn agree(group: &dyn SupportedKxGroup) {
        let a = group.start().unwrap();
        let b = group.start().unwrap();
        let (a_pub, b_pub) = (a.pub_key().to_vec(), b.pub_key().to_vec());
        let s_ab = a.complete(&b_pub).unwrap();
        let s_ba = b.complete(&a_pub).unwrap();
        assert_eq!(s_ab.secret_bytes(), s_ba.secret_bytes());
    }

    #[test]
    fn groups_agree() {
        for group in ALL {
            agree(*group);
        }
    }

    #[test]
    fn bad_shares_are_refused() {
        let x = X25519.start().unwrap();
        assert!(x.complete(&[0u8; 32]).is_err(), "low-order point");
        let p = SECP256R1.start().unwrap();
        assert!(p.complete(&[0x04; 65]).is_err(), "point not on the curve");
        let p = SECP256R1.start().unwrap();
        assert!(p.complete(&[0x02; 33]).is_err(), "compressed form");
    }
}
