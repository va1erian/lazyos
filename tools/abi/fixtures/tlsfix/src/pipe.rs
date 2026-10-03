//! A rustls client and server joined by in-memory buffers: the full TLS
//! stack (key exchange, certificate verification by webpki, AEAD records)
//! runs on LazyOS without a network peer, all on one thread.

use std::io::{Read, Write};
use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{
    ClientConfig, Connection, NamedGroup, RootCertStore, ServerConfig, SupportedCipherSuite,
};

/// The test PKI from `gen_certs.py` (valid 2000..2099; test-only key).
pub const CA: &[u8] = include_bytes!("../testdata/ca.der");
pub const OTHER_CA: &[u8] = include_bytes!("../testdata/other-ca.der");
const LEAF: &[u8] = include_bytes!("../testdata/leaf.der");
const LEAF_KEY: &[u8] = include_bytes!("../testdata/leaf.key.pk8");

/// The provider restricted to one cipher suite, so each run proves that
/// suite works.
fn provider_with(suite: SupportedCipherSuite, group: Option<NamedGroup>) -> Arc<CryptoProvider> {
    let mut provider = nettls_crypto::provider();
    provider.cipher_suites = vec![suite];
    if let Some(group) = group {
        provider.kx_groups.retain(|g| g.name() == group);
    }
    Arc::new(provider)
}

fn server(suite: SupportedCipherSuite) -> Result<Connection, String> {
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(LEAF_KEY.to_vec()));
    let mut config = ServerConfig::builder_with_provider(provider_with(suite, None))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(LEAF.to_vec())], key)
        .map_err(|e| format!("server config: {e}"))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let conn = rustls::ServerConnection::new(Arc::new(config)).map_err(|e| e.to_string())?;
    Ok(Connection::Server(conn))
}

fn client(
    trust: &[u8],
    suite: SupportedCipherSuite,
    group: Option<NamedGroup>,
    name: &str,
) -> Result<Connection, String> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(trust.to_vec()))
        .map_err(|e| format!("root: {e}"))?;
    let mut config = ClientConfig::builder_with_provider(provider_with(suite, group))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let name = ServerName::try_from(name.to_string()).map_err(|e| e.to_string())?;
    let conn = rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| e.to_string())?;
    Ok(Connection::Client(conn))
}

/// Move every pending TLS record from `from` to `to`. Returns the bytes moved.
fn transfer(from: &mut Connection, to: &mut Connection) -> Result<usize, String> {
    let mut wire = Vec::new();
    while from.wants_write() {
        from.write_tls(&mut wire).map_err(|e| e.to_string())?;
    }
    let mut rest = &wire[..];
    while !rest.is_empty() {
        to.read_tls(&mut rest).map_err(|e| e.to_string())?;
        to.process_new_packets().map_err(|e| format!("{e}"))?;
    }
    Ok(wire.len())
}

/// Run both sides until neither has anything left to send.
fn settle(client: &mut Connection, server: &mut Connection) -> Result<(), String> {
    for _ in 0..64 {
        let moved = transfer(client, server)? + transfer(server, client)?;
        if moved == 0 && !client.is_handshaking() && !server.is_handshaking() {
            return Ok(());
        }
    }
    Err("handshake did not settle".into())
}

/// Read exactly `len` plaintext bytes on `reader`, pumping records from
/// `writer` as needed.
fn receive(
    reader: &mut Connection,
    writer: &mut Connection,
    len: usize,
) -> Result<Vec<u8>, String> {
    let mut got = Vec::with_capacity(len);
    let mut chunk = vec![0u8; 16384];
    while got.len() < len {
        settle(reader, writer)?;
        match reader.reader().read(&mut chunk) {
            Ok(0) => return Err("connection closed early".into()),
            Ok(n) => got.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err("no progress reading".into())
            }
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    Ok(got)
}

/// Send `data` from `from` to `to` in chunks rustls will buffer.
fn send(from: &mut Connection, to: &mut Connection, data: &[u8]) -> Result<Vec<u8>, String> {
    let mut back = Vec::with_capacity(data.len());
    for piece in data.chunks(32 * 1024) {
        from.writer()
            .write_all(piece)
            .map_err(|e| format!("write: {e}"))?;
        back.extend(receive(to, from, piece.len())?);
    }
    Ok(back)
}

/// One handshake with `suite`, an HTTP-shaped request and a `body_len`-byte
/// response. Returns the negotiated parameters.
/// `group` limits the client to one key-exchange group.
pub fn exchange(
    suite: SupportedCipherSuite,
    group: Option<NamedGroup>,
    body_len: usize,
) -> Result<String, String> {
    let mut client = client(CA, suite, group, "tlsfix.test")?;
    let mut server = server(suite)?;
    settle(&mut client, &mut server)?;
    let request = b"GET / HTTP/1.1\r\nHost: tlsfix.test\r\n\r\n";
    if send(&mut client, &mut server, request)? != request {
        return Err("server received a different request".into());
    }
    let body: Vec<u8> = (0..body_len).map(|i| (i * 7 + 3) as u8).collect();
    if send(&mut server, &mut client, &body)? != body {
        return Err("response body corrupted".into());
    }
    let Connection::Client(c) = &client else {
        unreachable!()
    };
    let version = c.protocol_version().and_then(|v| v.as_str()).unwrap_or("?");
    let negotiated = c
        .negotiated_cipher_suite()
        .and_then(|s| s.suite().as_str())
        .unwrap_or("?");
    let group = c
        .negotiated_key_exchange_group()
        .and_then(|g| g.name().as_str())
        .unwrap_or("?");
    let alpn = c
        .alpn_protocol()
        .map(|a| String::from_utf8_lossy(a).into_owned());
    Ok(format!(
        "version={version} suite={negotiated} group={group} alpn={} bytes={body_len}",
        alpn.unwrap_or_else(|| "none".into())
    ))
}

/// A handshake that must fail: `name` not on the certificate, or the client
/// trusting `trust` instead of the issuing CA. Returns the client's error.
pub fn refused(trust: &[u8], name: &str) -> Result<String, String> {
    let suite = nettls_crypto::TLS13_AES_128_GCM_SHA256;
    let mut client = client(trust, suite, None, name)?;
    let mut server = server(suite)?;
    match settle(&mut client, &mut server) {
        Ok(()) => Err(format!("handshake for {name} was accepted")),
        Err(why) => Ok(why),
    }
}
