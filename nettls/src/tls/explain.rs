//! Turning TLS outcomes into words: the `-v` handshake report and the
//! explanation of a failure.
//!
//! A clock problem gets special care (docs/tls-plan.md §5.3): a machine with
//! a wrong clock fails every handshake with "expired" or "not yet valid",
//! which otherwise looks like an attack. The message shows the system time
//! next to the certificate's bound so the cause is obvious.

use std::io;

use rustls::{CertificateError, ClientConnection};

use crate::report::TlsFailure;
use crate::sanitize::printable_str;
use crate::timefmt;
use crate::x509info;

/// The `-v` lines for an established connection: one `TLS:HANDSHAKE` line,
/// then one `TLS:CHAIN` line per certificate the server sent, leaf first.
pub fn handshake_lines(conn: &ClientConnection, host: &str) -> Vec<String> {
    let version = conn
        .protocol_version()
        .map(|v| v.as_str().unwrap_or("unknown").replace('_', "."))
        .unwrap_or_else(|| "unknown".into());
    let suite = conn
        .negotiated_cipher_suite()
        .and_then(|s| s.suite().as_str())
        .unwrap_or("unknown");
    let group = conn
        .negotiated_key_exchange_group()
        .and_then(|g| g.name().as_str())
        .unwrap_or("unknown");
    let alpn = conn
        .alpn_protocol()
        .map(crate::sanitize::printable)
        .unwrap_or_else(|| "none".into());
    let mut lines = vec![format!(
        "TLS:HANDSHAKE version={version} suite={suite} group={group} alpn={alpn} sni={}",
        printable_str(host)
    )];
    if let Some(chain) = conn.peer_certificates() {
        lines.extend(x509info::chain_lines(chain));
    }
    lines
}

/// Explain a failed handshake. `error` is what rustls's `complete_io`
/// returned: a rustls error wrapped in `InvalidData`, or a transport error.
pub fn failure(error: &io::Error, host: &str) -> TlsFailure {
    let tls = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>());
    match tls {
        Some(rustls::Error::InvalidCertificate(cert)) => certificate_failure(cert, host),
        Some(other) => TlsFailure {
            reason: protocol_reason(other),
            certificate: false,
            clock: None,
        },
        None => TlsFailure {
            reason: transport_reason(error),
            certificate: false,
            clock: None,
        },
    }
}

fn transport_reason(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::UnexpectedEof => {
            "the server closed the connection during the handshake".into()
        }
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "the handshake timed out".into(),
        _ => format!(
            "connection error during the handshake: {}",
            printable_str(&error.to_string())
        ),
    }
}

fn protocol_reason(error: &rustls::Error) -> String {
    match error {
        rustls::Error::AlertReceived(alert) => {
            format!("the server refused the handshake (alert: {alert:?})")
        }
        rustls::Error::PeerIncompatible(why) => format!(
            "the server offers no protocol version or cipher suite this client accepts \
             (TLS 1.2+ with AEAD suites only): {why:?}"
        ),
        rustls::Error::PeerMisbehaved(why) => format!("the server broke the protocol: {why:?}"),
        rustls::Error::NoApplicationProtocol => "no common application protocol (ALPN)".into(),
        other => printable_str(&other.to_string()),
    }
}

/// Explain a certificate webpki rejected.
pub fn certificate_failure(error: &CertificateError, host: &str) -> TlsFailure {
    let host = printable_str(host);
    let (reason, clock) = match error {
        CertificateError::UnknownIssuer => (
            "certificate verify failed: unknown issuer (the server's chain does not lead \
             to a CA in the trusted bundle; a self-signed certificate or a private CA \
             looks like this)"
                .to_string(),
            None,
        ),
        CertificateError::NotValidForName => (
            format!("certificate verify failed: the certificate is not valid for {host}"),
            None,
        ),
        CertificateError::NotValidForNameContext { presented, .. } => (
            format!(
                "certificate verify failed: the certificate is not valid for {host} \
                 (it names: {})",
                printable_str(&presented.join(", "))
            ),
            None,
        ),
        CertificateError::ExpiredContext { time, not_after } => (
            "certificate verify failed: certificate has expired".to_string(),
            Some(expired_clock(time.as_secs(), not_after.as_secs())),
        ),
        CertificateError::NotValidYetContext { time, not_before } => (
            "certificate verify failed: certificate is not yet valid".to_string(),
            Some(not_yet_clock(time.as_secs(), not_before.as_secs())),
        ),
        CertificateError::Expired | CertificateError::NotValidYet => (
            format!(
                "certificate verify failed: certificate is outside its validity period ({error:?})"
            ),
            Some(format!(
                "The system clock reads {}. If that is wrong, set the correct time and retry.",
                timefmt::now_utc()
            )),
        ),
        CertificateError::BadSignature => (
            "certificate verify failed: a signature in the chain is invalid".to_string(),
            None,
        ),
        CertificateError::Revoked => ("certificate verify failed: revoked".to_string(), None),
        other => (format!("certificate verify failed: {other:?}"), None),
    };
    TlsFailure {
        reason: printable_str(&reason),
        certificate: true,
        clock,
    }
}

/// The clock lines for an expired certificate.
pub fn expired_clock(now: u64, not_after: u64) -> String {
    format!(
        "The certificate expired at {}; the system clock reads {}.\n\
         If the system clock is wrong (behind or ahead), set the correct time and retry; \
         if it is right, the server's certificate really has expired.",
        timefmt::utc(not_after),
        timefmt::utc(now)
    )
}

/// The clock lines for a certificate that is not valid yet: almost always a
/// clock that is behind (a dead RTC falls back to 2026-01-01 on LazyOS).
pub fn not_yet_clock(now: u64, not_before: u64) -> String {
    format!(
        "The certificate is valid only from {}; the system clock reads {}.\n\
         The system clock is probably wrong (behind): set the correct time and retry.",
        timefmt::utc(not_before),
        timefmt::utc(now)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::UnixTime;
    use std::time::Duration;

    fn unix(secs: u64) -> UnixTime {
        UnixTime::since_unix_epoch(Duration::from_secs(secs))
    }

    #[test]
    fn not_yet_valid_names_both_times() {
        let f = certificate_failure(
            &CertificateError::NotValidYetContext {
                time: unix(1_767_225_600),
                not_before: unix(1_789_000_000),
            },
            "example.com",
        );
        assert!(f.certificate);
        let clock = f.clock.unwrap();
        assert!(clock.contains("2026-01-01 00:00:00 UTC"), "{clock}");
        assert!(clock.contains("valid only from 2026-09-10"), "{clock}");
        assert!(clock.contains("probably wrong"));
    }

    #[test]
    fn expired_names_both_times() {
        let f = certificate_failure(
            &CertificateError::ExpiredContext {
                time: unix(1_791_051_045),
                not_after: unix(1_767_225_600),
            },
            "h",
        );
        assert!(f.reason.contains("expired"));
        assert!(f.clock.unwrap().contains("expired at 2026-01-01"));
    }

    #[test]
    fn unknown_issuer_and_name() {
        let f = certificate_failure(&CertificateError::UnknownIssuer, "h");
        assert!(f.reason.contains("unknown issuer") && f.certificate && f.clock.is_none());
        let f = certificate_failure(&CertificateError::NotValidForName, "evil\x1b]0;x\x07.com");
        assert!(!f.reason.contains('\x1b'), "{}", f.reason);
    }

    #[test]
    fn wrapped_rustls_errors_are_found() {
        let wrapped = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer),
        );
        let f = failure(&wrapped, "h");
        assert!(f.certificate);
        let eof = io::Error::new(io::ErrorKind::UnexpectedEof, "eof");
        let f = failure(&eof, "h");
        assert!(!f.certificate && f.reason.contains("closed"));
    }
}
