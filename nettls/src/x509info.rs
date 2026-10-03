//! Human-readable facts about a certificate for `-v`: subject, issuer and
//! validity.
//!
//! Display only: it runs on a chain webpki has already verified, and nothing
//! here takes part in a trust decision. It is a small DER walk over
//! `untrusted` (webpki's own panic-free reader) rather than a full X.509
//! crate, because the RustCrypto `x509-cert` pulls `flagset`, which is
//! Apache-2.0 only and so cannot go into a GPL-2.0 program
//! (`tools/nettls/licenses.py`). Any structure it does not expect yields
//! `None`, never a guess.

use untrusted::{Input, Reader};

use crate::sanitize::printable;

/// What `-v` prints for one certificate.
#[derive(Debug, PartialEq, Eq)]
pub struct CertSummary {
    pub subject: String,
    pub issuer: String,
    pub not_before: String,
    pub not_after: String,
}

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const INTEGER: u8 = 0x02;
const OID: u8 = 0x06;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const EXPLICIT_VERSION: u8 = 0xa0;

/// Read one DER TLV: the tag and the contents.
fn tlv<'a>(r: &mut Reader<'a>) -> Option<(u8, Input<'a>)> {
    let tag = r.read_byte().ok()?;
    let first = r.read_byte().ok()?;
    let len = match first {
        0..=0x7f => usize::from(first),
        0x81..=0x83 => {
            let mut len = 0usize;
            for _ in 0..(first & 0x7f) {
                len = (len << 8) | usize::from(r.read_byte().ok()?);
            }
            len
        }
        _ => return None,
    };
    Some((tag, r.read_bytes(len).ok()?))
}

/// Read a TLV that must have `tag`.
fn expect<'a>(r: &mut Reader<'a>, tag: u8) -> Option<Input<'a>> {
    match tlv(r)? {
        (t, value) if t == tag => Some(value),
        _ => None,
    }
}

/// Summarise a DER certificate, or `None` when it is not shaped as expected.
pub fn summarize(der: &[u8]) -> Option<CertSummary> {
    Input::from(der)
        .read_all((), |cert| {
            let summary = expect(cert, SEQUENCE).and_then(|outer| {
                outer
                    .read_all((), |outer| {
                        let tbs = expect(outer, SEQUENCE).ok_or(())?;
                        // signatureAlgorithm and signatureValue follow; not needed.
                        outer.skip_to_end();
                        tbs.read_all((), |r| tbs_summary(r).ok_or(()))
                    })
                    .ok()
            });
            summary.ok_or(())
        })
        .ok()
}

fn tbs_summary(r: &mut Reader<'_>) -> Option<CertSummary> {
    if r.peek(EXPLICIT_VERSION) {
        tlv(r)?;
    }
    expect(r, INTEGER)?; // serialNumber
    expect(r, SEQUENCE)?; // signature algorithm
    let issuer = name(expect(r, SEQUENCE)?)?;
    let (not_before, not_after) = expect(r, SEQUENCE)?
        .read_all((), |v| {
            let a = time(v).ok_or(())?;
            let b = time(v).ok_or(())?;
            Ok((a, b))
        })
        .ok()?;
    let subject = name(expect(r, SEQUENCE)?)?;
    r.skip_to_end(); // subjectPublicKeyInfo and extensions
    Some(CertSummary {
        subject,
        issuer,
        not_before,
        not_after,
    })
}

/// An X.501 Name as `CN=...,O=...` (RFC 4514 order: last RDN first).
fn name(input: Input<'_>) -> Option<String> {
    let mut parts = Vec::new();
    input
        .read_all((), |r| {
            while !r.at_end() {
                let set = expect(r, SET).ok_or(())?;
                set.read_all((), |s| {
                    while !s.at_end() {
                        let atv = expect(s, SEQUENCE).ok_or(())?;
                        atv.read_all((), |a| {
                            let oid = expect(a, OID).ok_or(())?;
                            let (_, value) = tlv(a).ok_or(())?;
                            parts.push(format!(
                                "{}={}",
                                attribute(oid.as_slice_less_safe()),
                                printable(value.as_slice_less_safe())
                            ));
                            Ok(())
                        })?;
                    }
                    Ok(())
                })?;
            }
            Ok(())
        })
        .ok()?;
    parts.reverse();
    Some(parts.join(","))
}

/// The short name of a common attribute type (id-at, RFC 4519).
fn attribute(oid: &[u8]) -> String {
    match oid {
        [0x55, 0x04, 0x03] => "CN".into(),
        [0x55, 0x04, 0x06] => "C".into(),
        [0x55, 0x04, 0x07] => "L".into(),
        [0x55, 0x04, 0x08] => "ST".into(),
        [0x55, 0x04, 0x0a] => "O".into(),
        [0x55, 0x04, 0x0b] => "OU".into(),
        [0x55, 0x04, 0x05] => "serialNumber".into(),
        other => {
            let hex: Vec<String> = other.iter().map(|b| format!("{b:02x}")).collect();
            format!("oid:{}", hex.join(""))
        }
    }
}

/// UTCTime (`YYMMDDHHMMSSZ`, RFC 5280 years 1950-2049) or GeneralizedTime
/// (`YYYYMMDDHHMMSSZ`) as `YYYY-MM-DD HH:MM:SS UTC`.
fn time(r: &mut Reader<'_>) -> Option<String> {
    let (tag, value) = tlv(r)?;
    let text = value.as_slice_less_safe();
    let digits = text.strip_suffix(b"Z")?;
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let digits = std::str::from_utf8(digits).ok()?;
    let full = match (tag, digits.len()) {
        (UTC_TIME, 12) => {
            let yy: u32 = digits[..2].parse().ok()?;
            format!("{}{digits}", if yy < 50 { "20" } else { "19" })
        }
        (GENERALIZED_TIME, 14) => digits.to_string(),
        _ => return None,
    };
    Some(format!(
        "{}-{}-{} {}:{}:{} UTC",
        &full[..4],
        &full[4..6],
        &full[6..8],
        &full[8..10],
        &full[10..12],
        &full[12..14]
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::CertificateDer;

    fn test_ca() -> Vec<u8> {
        let pem = include_bytes!("../tests/data/test-ca.pem");
        CertificateDer::from_pem_slice(pem).unwrap().to_vec()
    }

    #[test]
    fn summarizes_a_real_certificate() {
        let s = summarize(&test_ca()).unwrap();
        assert_eq!(s.subject, "CN=nettls unit-test CA");
        assert_eq!(s.issuer, "CN=nettls unit-test CA");
        assert!(
            s.not_before.starts_with("2026-") && s.not_before.ends_with(" UTC"),
            "{}",
            s.not_before
        );
        assert!(s.not_after.starts_with("21"), "{}", s.not_after);
    }

    #[test]
    fn truncated_or_garbage_is_none() {
        let der = test_ca();
        for cut in [0, 1, 2, 10, der.len() / 2, der.len() - 1] {
            assert_eq!(summarize(&der[..cut]), None, "cut at {cut}");
        }
        assert_eq!(summarize(b"\x30\x84\xff\xff\xff\xff"), None);
        let lines = chain_lines(&[b"junk".to_vec()]);
        assert_eq!(lines, vec!["TLS:CHAIN depth=0 (undecodable certificate)"]);
    }

    #[test]
    fn times() {
        let utc = [
            UTC_TIME, 13, b'4', b'9', b'1', b'2', b'3', b'1', b'2', b'3', b'5', b'9', b'5', b'9',
            b'Z',
        ];
        assert_eq!(
            time(&mut Reader::new(Input::from(&utc))).unwrap(),
            "2049-12-31 23:59:59 UTC"
        );
        let old = [
            UTC_TIME, 13, b'5', b'0', b'0', b'1', b'0', b'1', b'0', b'0', b'0', b'0', b'0', b'0',
            b'Z',
        ];
        assert_eq!(
            time(&mut Reader::new(Input::from(&old))).unwrap(),
            "1950-01-01 00:00:00 UTC"
        );
        let bad = [
            UTC_TIME, 13, b'4', b'9', b'1', b'2', b'3', b'1', b'2', b'3', b'5', b'9', b'5', b'x',
            b'Z',
        ];
        assert!(time(&mut Reader::new(Input::from(&bad))).is_none());
    }

    #[test]
    fn names_are_sanitised_and_reversed() {
        // SEQ { SET { SEQ { O, "Evil\x1b" } }, SET { SEQ { CN, "x" } } }
        let o = [
            0x31, 0x0e, 0x30, 0x0c, 0x06, 0x03, 0x55, 0x04, 0x0a, 0x0c, 0x05, b'E', b'v', b'i',
            b'l', 0x1b,
        ];
        let cn = [
            0x31, 0x0a, 0x30, 0x08, 0x06, 0x03, 0x55, 0x04, 0x03, 0x0c, 0x01, b'x',
        ];
        let body: Vec<u8> = o.iter().chain(cn.iter()).copied().collect();
        assert_eq!(name(Input::from(&body)).unwrap(), "CN=x,O=Evil?");
    }
}
