//! Trust anchors: the system CA bundle as a rustls `RootCertStore`
//! (docs/tls-plan.md §5.2).
//!
//! The bundle is `--cacert`/`--ca-certificate` when given, else
//! `SSL_CERT_FILE` when set, else `/etc/ssl/certs/ca-certificates.crt`
//! (`fhs::etc::LINUX_CA_BUNDLE`). There is no compiled-in root set and no
//! fallback: a missing, empty or corrupt bundle is an error that names the
//! file, because silently trusting a different set of roots (or none) is
//! exactly what an attacker would want.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use rustls::RootCertStore;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::CertificateDer;

/// The environment variable that overrides the bundle path, as OpenSSL and
/// `rustls-native-certs` read it.
pub const ENV_CERT_FILE: &str = "SSL_CERT_FILE";

/// A system bundle is ~200 KiB; anything this large is not one, and reading
/// it whole would only waste memory.
const MAX_BUNDLE_BYTES: u64 = 16 * 1024 * 1024;

/// The bundle to load and why that one.
#[derive(Debug, PartialEq, Eq)]
pub struct BundleChoice {
    pub path: PathBuf,
    pub source: &'static str,
}

/// Pick the bundle: an explicit option, then `SSL_CERT_FILE` (`env`), then
/// the system path. An empty `SSL_CERT_FILE` counts as unset.
pub fn choose(explicit: Option<&Path>, env: Option<String>) -> BundleChoice {
    if let Some(path) = explicit {
        return BundleChoice {
            path: path.to_path_buf(),
            source: "command line",
        };
    }
    match env {
        Some(value) if !value.is_empty() => BundleChoice {
            path: PathBuf::from(value),
            source: ENV_CERT_FILE,
        },
        _ => BundleChoice {
            path: PathBuf::from(fhs::etc::LINUX_CA_BUNDLE),
            source: "system",
        },
    }
}

/// The loaded store and how many certificates webpki could not use.
#[derive(Debug)]
pub struct Roots {
    pub store: RootCertStore,
    pub ignored: usize,
}

/// Read and parse the chosen bundle.
pub fn load(choice: &BundleChoice) -> Result<Roots, String> {
    let shown = choice.path.display();
    let file = File::open(&choice.path).map_err(|e| {
        format!(
            "cannot read the CA bundle {shown} ({}): {e}; without trust anchors no \
             certificate can be verified",
            choice.source
        )
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_BUNDLE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read the CA bundle {shown}: {e}"))?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES {
        return Err(format!(
            "the CA bundle {shown} is larger than 16 MiB; refusing it"
        ));
    }
    from_pem(&bytes).map_err(|why| format!("the CA bundle {shown} {why}"))
}

/// Build a store from PEM text. A malformed PEM section fails the whole
/// bundle (a corrupt file must not quietly lose roots); certificates that
/// parse as PEM but that webpki cannot use are counted in `ignored`.
pub fn from_pem(bytes: &[u8]) -> Result<Roots, String> {
    let mut ders = Vec::new();
    for item in CertificateDer::pem_slice_iter(bytes) {
        ders.push(item.map_err(|e| format!("is not valid PEM: {e}"))?);
    }
    if ders.is_empty() {
        return Err("holds no certificates".into());
    }
    let mut store = RootCertStore::empty();
    let (added, ignored) = store.add_parsable_certificates(ders);
    if added == 0 {
        return Err(format!(
            "holds {ignored} certificate(s), none usable as a trust anchor"
        ));
    }
    Ok(Roots { store, ignored })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real self-signed CA (P-256), generated once for these tests.
    pub(crate) const TEST_CA: &str = include_str!("../tests/data/test-ca.pem");

    #[test]
    fn choice_order() {
        let explicit = choose(Some(Path::new("/x.pem")), Some("/env.pem".into()));
        assert_eq!(explicit.path, PathBuf::from("/x.pem"));
        assert_eq!(choose(None, Some("/env.pem".into())).source, ENV_CERT_FILE);
        let system = choose(None, Some(String::new()));
        assert_eq!(
            system.path,
            PathBuf::from("/etc/ssl/certs/ca-certificates.crt")
        );
        assert_eq!(choose(None, None), system);
    }

    #[test]
    fn loads_a_real_ca() {
        let roots = from_pem(TEST_CA.as_bytes()).unwrap();
        assert_eq!(roots.store.len(), 1);
        assert_eq!(roots.ignored, 0);
    }

    #[test]
    fn empty_or_text_is_an_error() {
        assert!(from_pem(b"").unwrap_err().contains("no certificates"));
        assert!(from_pem(b"just some text\n")
            .unwrap_err()
            .contains("no certificates"));
    }

    #[test]
    fn corrupt_pem_is_an_error() {
        let broken = "-----BEGIN CERTIFICATE-----\n!!!notbase64!!!\n-----END CERTIFICATE-----\n";
        assert!(from_pem(broken.as_bytes())
            .unwrap_err()
            .contains("not valid PEM"));
    }

    #[test]
    fn garbage_der_is_not_a_root() {
        // Valid base64, valid PEM framing, but not a certificate.
        let fake = "-----BEGIN CERTIFICATE-----\nAAECAwQFBgcICQ==\n-----END CERTIFICATE-----\n";
        assert!(from_pem(fake.as_bytes())
            .unwrap_err()
            .contains("none usable"));
    }

    #[test]
    fn missing_file_names_the_path() {
        let choice = choose(Some(Path::new("/nonexistent/ca.pem")), None);
        let err = load(&choice).err().unwrap();
        assert!(err.contains("/nonexistent/ca.pem"), "{err}");
    }
}
