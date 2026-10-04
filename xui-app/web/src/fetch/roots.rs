//! Trust anchors: a PEM CA bundle as a rustls [`RootCertStore`].
//!
//! There is no compiled-in root set and no fallback to "trust everything": a
//! missing or unusable bundle makes every `https:` fetch fail with a message
//! naming the file, while `http:` keeps working.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use rustls::RootCertStore;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::CertificateDer;

/// The variable that names another bundle, as OpenSSL reads it.
pub const ENV_CERT_FILE: &str = "SSL_CERT_FILE";

/// A real bundle is a few hundred KiB; refuse to read anything absurd.
const MAX_BUNDLE_BYTES: u64 = 16 * 1024 * 1024;

/// The system bundle: `SSL_CERT_FILE` when set and not empty, else the
/// LazyOS path, else the conventional Linux one (the same file on LazyOS;
/// the latter also finds a host's bundle when the browser runs there).
pub fn system_bundle() -> PathBuf {
    if let Some(path) = std::env::var_os(ENV_CERT_FILE).filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    [fhs::etc::CA_BUNDLE, fhs::etc::LINUX_CA_BUNDLE]
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(fhs::etc::CA_BUNDLE))
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
        let err = load(Path::new("/nonexistent/lazyweb-ca.pem")).unwrap_err();
        assert!(err.contains("/nonexistent/lazyweb-ca.pem"), "{err}");
    }
}
