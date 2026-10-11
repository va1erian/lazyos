//! Trust anchors as a rustls [`RootCertStore`].
//!
//! There is no compiled-in root set and no fallback to "trust everything": a
//! missing or unusable bundle makes every `https:` request fail with a
//! message naming the file.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use rustls::RootCertStore;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::CertificateDer;

use crate::platform::Roots;

/// A real bundle is a few hundred KiB; refuse to read anything absurd.
const MAX_BUNDLE_BYTES: u64 = 16 * 1024 * 1024;

/// The store for `roots`.
pub fn store(roots: &Roots) -> Result<RootCertStore, String> {
    match roots {
        Roots::Bundle(path) => load(path),
        Roots::Pem(bytes) => from_pem(bytes),
        Roots::Native => native(),
    }
}

/// Reads the bundle at `path`.
pub fn load(path: &Path) -> Result<RootCertStore, String> {
    let shown = path.display();
    let file = File::open(path).map_err(|e| format!("cannot read the CA bundle {shown}: {e}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_BUNDLE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read the CA bundle {shown}: {e}"))?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(format!("the CA bundle {shown} is implausibly large"));
    }
    from_pem(&bytes).map_err(|why| format!("the CA bundle {shown} {why}"))
}

/// Builds a store from PEM text. A malformed section fails the whole bundle
/// (a corrupt file must not quietly lose roots); certificates webpki cannot
/// use as anchors are skipped as long as one is usable.
pub fn from_pem(bytes: &[u8]) -> Result<RootCertStore, String> {
    let certs = CertificateDer::pem_slice_iter(bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("is not valid PEM: {e}"))?;
    if certs.is_empty() {
        return Err("holds no certificates".into());
    }
    let mut store = RootCertStore::empty();
    let (added, _) = store.add_parsable_certificates(certs);
    if added == 0 {
        return Err("holds no certificate usable as a trust anchor".into());
    }
    Ok(store)
}

#[cfg(feature = "native-roots")]
fn native() -> Result<RootCertStore, String> {
    let found = rustls_native_certs::load_native_certs();
    let mut store = RootCertStore::empty();
    let (added, _) = store.add_parsable_certificates(found.certs);
    if added == 0 {
        return Err("the system trust store holds no usable certificate".into());
    }
    Ok(store)
}

#[cfg(not(feature = "native-roots"))]
fn native() -> Result<RootCertStore, String> {
    Err("this build has no system trust store (feature native-roots)".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_corrupt_bundles_are_refused() {
        assert!(from_pem(b"").unwrap_err().contains("no certificates"));
        let broken = b"-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n";
        assert!(from_pem(broken).unwrap_err().contains("not valid PEM"));
        let not_a_cert = b"-----BEGIN CERTIFICATE-----\nAAECAwQ=\n-----END CERTIFICATE-----\n";
        assert!(from_pem(not_a_cert).unwrap_err().contains("usable"));
    }

    #[test]
    fn a_missing_file_is_named() {
        let err = load(Path::new("/nonexistent/lazysky-ca.pem")).unwrap_err();
        assert!(err.contains("/nonexistent/lazysky-ca.pem"), "{err}");
    }
}
