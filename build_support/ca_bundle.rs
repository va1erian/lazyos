//! The TLS trust anchors (docs/tls-plan.md §5.2): one PEM bundle at
//! `fhs::etc::CA_BUNDLE`, which Linux programs see as
//! `/etc/ssl/certs/ca-certificates.crt`.
//!
//! The roots are the Mozilla root program's (CCADB), from the pinned
//! `webpki-root-certs` build-dependency (its exact version is in `Cargo.lock`),
//! so a rebuild after a deliberate bump is the only way the set changes. They
//! are DER; this module encodes each as a PEM `CERTIFICATE` block. The bundle
//! is written into every image, whether or not it carries a TLS client.
//!
//! `LAZYOS_TLS_TEST_CA=<file.pem>` is a test-only switch: the TLS harness
//! appends its own CA so the production verification path accepts its test
//! server. The file must hold only PEM `CERTIFICATE` blocks (a private key or
//! anything else fails the build), and the blocks are re-encoded from their
//! decoded DER, so nothing but certificates reaches the image.

use std::path::Path;

use crate::os_image::Sink;

/// The PEM armour of one certificate.
const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const END: &str = "-----END CERTIFICATE-----";

/// The base64 alphabet (RFC 4648 §4).
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with `=` padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for (index, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(ALPHABET[(n >> shift & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decode standard, padded base64; `None` for anything malformed (a foreign
/// character, a bad length, padding in the middle or non-zero padding bits).
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let value = |c: u8| ALPHABET.iter().position(|&a| a == c).map(|v| v as u32);
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let quads = bytes.len() / 4;
    for (index, quad) in bytes.chunks(4).enumerate() {
        let pad = quad.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && index + 1 != quads) {
            return None;
        }
        let mut n = 0u32;
        for &c in &quad[..4 - pad] {
            n = n << 6 | value(c)?;
        }
        n <<= 6 * pad as u32;
        let decoded = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        // The bits padding stands for must be zero (a canonical encoding).
        if decoded[3 - pad..].iter().any(|&b| b != 0) {
            return None;
        }
        out.extend_from_slice(&decoded[..3 - pad]);
    }
    Some(out)
}

/// One DER certificate as a PEM block, 64 columns, ending in a newline.
pub fn pem_encode(der: &[u8]) -> String {
    let body = base64_encode(der);
    let mut out = String::with_capacity(body.len() + body.len() / 64 + 64);
    out.push_str(BEGIN);
    out.push('\n');
    for line in body.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        out.push('\n');
    }
    out.push_str(END);
    out.push('\n');
    out
}

/// Whether `der` is shaped like a certificate: a DER `SEQUENCE` whose length
/// header covers exactly the bytes given. Not a full parse; it stops a file
/// of something else from being re-armoured as a certificate.
pub fn looks_like_certificate(der: &[u8]) -> bool {
    let [0x30, first, rest @ ..] = der else {
        return false;
    };
    let (length, body) = match *first {
        short @ 0..=0x7F => (usize::from(short), rest),
        0x81..=0x84 => {
            let count = usize::from(first & 0x7F);
            if rest.len() < count || rest[0] == 0 {
                return false;
            }
            let length = rest[..count]
                .iter()
                .fold(0usize, |acc, &b| acc << 8 | usize::from(b));
            (length, &rest[count..])
        }
        _ => return false,
    };
    length > 0 && body.len() == length
}

/// The DER certificates of a PEM file. Only `CERTIFICATE` blocks and blank
/// lines are accepted; any other block (a private key), stray text, a broken
/// block or a file without a certificate is an error naming the line.
pub fn parse_pem_certificates(text: &str) -> Result<Vec<Vec<u8>>, String> {
    let mut certificates = Vec::new();
    let mut body: Option<(usize, String)> = None;
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        match body.as_mut() {
            None if line.is_empty() => {}
            None if line == BEGIN => body = Some((number, String::new())),
            None => {
                return Err(format!(
                    "line {number}: not a PEM CERTIFICATE block: {line:.40}"
                ))
            }
            Some((start, base64)) if line == END => {
                let der = base64_decode(base64)
                    .ok_or(format!("line {start}: the certificate is not valid base64"))?;
                if !looks_like_certificate(&der) {
                    return Err(format!("line {start}: the block is not a DER certificate"));
                }
                certificates.push(der);
                body = None;
            }
            Some(_) if line.starts_with("-----") => {
                return Err(format!("line {number}: unexpected {line:.40}"));
            }
            Some((_, base64)) => base64.push_str(line),
        }
    }
    if let Some((start, _)) = body {
        return Err(format!("line {start}: the certificate block is not closed"));
    }
    if certificates.is_empty() {
        return Err("no certificate in it".into());
    }
    Ok(certificates)
}

/// The bundle: every root as a PEM block, in the order given, then the
/// `extra` (test) certificates.
pub fn bundle<'a>(roots: impl IntoIterator<Item = &'a [u8]>, extra: &[Vec<u8>]) -> String {
    let mut out = String::new();
    for der in roots {
        out.push_str(&pem_encode(der));
    }
    for der in extra {
        out.push_str(&pem_encode(der));
    }
    out
}

/// The certificates `LAZYOS_TLS_TEST_CA` names, or none when it is unset or
/// empty. A file that is missing or not certificates fails the build.
fn test_ca() -> Vec<Vec<u8>> {
    println!("cargo:rerun-if-env-changed=LAZYOS_TLS_TEST_CA");
    let Some(path) = std::env::var_os("LAZYOS_TLS_TEST_CA").filter(|v| !v.is_empty()) else {
        return Vec::new();
    };
    let path = Path::new(&path);
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("LAZYOS_TLS_TEST_CA: cannot read {}: {e}", path.display()));
    let certificates = parse_pem_certificates(&text)
        .unwrap_or_else(|e| panic!("LAZYOS_TLS_TEST_CA {}: {e}", path.display()));
    println!(
        "cargo:warning=LAZYOS_TLS_TEST_CA: {} test certificate(s) from {} appended to the \
         system CA bundle; this image trusts them (never ship it)",
        certificates.len(),
        path.display()
    );
    certificates
}

/// Write the bundle of `roots` (plus the test CA, if any) to
/// `fhs::etc::CA_BUNDLE`.
pub fn embed<'a>(sink: &mut dyn Sink, roots: impl IntoIterator<Item = &'a [u8]>) {
    // World-readable (0644, `os_layout::file_mode`): certificates are public.
    sink.add_bytes(fhs::etc::CA_BUNDLE, bundle(roots, &test_ca()).into_bytes());
}
