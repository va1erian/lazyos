//! Human-readable facts about a certificate for `-v`: subject, issuer and
//! validity. This is display only, run on a chain webpki already verified;
//! nothing here takes part in a trust decision.

use x509_cert::der::Decode;
use x509_cert::Certificate;

use crate::sanitize::printable_str;
use crate::timefmt;

/// What `-v` prints for one certificate.
#[derive(Debug, PartialEq, Eq)]
pub struct CertSummary {
    pub subject: String,
    pub issuer: String,
    pub not_before: String,
    pub not_after: String,
}

/// Summarise a DER certificate, or `None` when it does not decode.
pub fn summarize(der: &[u8]) -> Option<CertSummary> {
    let cert = Certificate::from_der(der).ok()?;
    let tbs = &cert.tbs_certificate;
    let time = |t: &x509_cert::time::Time| timefmt::utc(t.to_unix_duration().as_secs());
    Some(CertSummary {
        // RFC 4514 strings come from the server: printable ASCII only.
        subject: printable_str(&tbs.subject.to_string()),
        issuer: printable_str(&tbs.issuer.to_string()),
        not_before: time(&tbs.validity.not_before),
        not_after: time(&tbs.validity.not_after),
    })
}

/// The `-v` lines for a verified chain, leaf first.
pub fn chain_lines(chain: &[impl AsRef<[u8]>]) -> Vec<String> {
    chain
        .iter()
        .enumerate()
        .map(|(depth, der)| match summarize(der.as_ref()) {
            Some(c) => format!(
                "TLS:CHAIN depth={depth} subject=\"{}\" issuer=\"{}\" valid={} .. {}",
                c.subject, c.issuer, c.not_before, c.not_after
            ),
            None => format!("TLS:CHAIN depth={depth} (undecodable certificate)"),
        })
        .collect()
}
