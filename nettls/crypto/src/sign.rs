//! Server-side signing, for the in-memory handshakes of the ABI fixture
//! `tlsfix` only (feature `server`): ECDSA P-256 with SHA-256 from a PKCS#8
//! key. The clients never sign and are built without this module.

use std::sync::Arc;

use p256::ecdsa::signature::Signer as _;
use p256::pkcs8::DecodePrivateKey;
use rustls::pki_types::PrivateKeyDer;
use rustls::sign::{Signer, SigningKey};
use rustls::{Error, SignatureAlgorithm, SignatureScheme};

/// Load a PKCS#8 P-256 key; anything else is refused.
pub fn load(key: PrivateKeyDer<'static>) -> Result<Arc<dyn SigningKey>, Error> {
    let PrivateKeyDer::Pkcs8(der) = key else {
        return Err(Error::General(
            "only PKCS#8 P-256 keys are supported".into(),
        ));
    };
    let key = p256::ecdsa::SigningKey::from_pkcs8_der(der.secret_pkcs8_der())
        .map_err(|_| Error::General("not a PKCS#8 P-256 key".into()))?;
    Ok(Arc::new(EcdsaP256(Arc::new(key))))
}

#[derive(Debug)]
struct EcdsaP256(Arc<p256::ecdsa::SigningKey>);

impl SigningKey for EcdsaP256 {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        offered
            .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
            .then(|| Box::new(EcdsaP256Signer(self.0.clone())) as Box<dyn Signer>)
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ECDSA
    }
}

#[derive(Debug)]
struct EcdsaP256Signer(Arc<p256::ecdsa::SigningKey>);

impl Signer for EcdsaP256Signer {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        // RFC 6979 deterministic nonces: no randomness to get wrong.
        let sig: p256::ecdsa::Signature = self.0.sign(message);
        Ok(sig.to_der().as_bytes().to_vec())
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ECDSA_NISTP256_SHA256
    }
}
