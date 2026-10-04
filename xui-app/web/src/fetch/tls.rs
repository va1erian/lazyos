//! TLS for `https:` fetches: one rustls client configuration, built on first
//! use, and a ureq connector that wraps each new connection in it.
//!
//! ureq's built-in rustls connector is not used: it sets no ALPN and would
//! drag in its own provider and root choices. Here the provider is always
//! `nettls-crypto`, verification is always webpki against [`super::roots`],
//! ALPN offers only `http/1.1` (ureq speaks nothing else), and the handshake
//! runs on the fetch thread.

use std::cell::RefCell;
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, OnceLock};

use rustls::pki_types::ServerName;
use rustls::{CertificateError, ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, Transport,
    TransportAdapter,
};
use ureq::Error;

use super::{roots, Roots};

thread_local! {
    /// Why this thread's last handshake failed, in words: ureq's error type
    /// only carries a static string.
    static LAST_FAILURE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Takes the reason the calling thread's last TLS handshake failed.
pub(crate) fn take_failure() -> Option<String> {
    LAST_FAILURE.with(|slot| slot.borrow_mut().take())
}

fn record_failure(why: String) {
    LAST_FAILURE.with(|slot| *slot.borrow_mut() = Some(why));
}

/// The client configuration for `roots`: TLS 1.2 and 1.3 with
/// `nettls-crypto`'s suites, SNI on, ALPN `http/1.1`.
pub(crate) fn client_config(roots: RootCertStore) -> Result<Arc<ClientConfig>, String> {
    let mut config = ClientConfig::builder_with_provider(nettls_crypto::provider_arc())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS configuration: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The configuration, made from the roots the first time an `https:` URL is
/// fetched (so a browser that only shows local pages never reads the
/// bundle). A failure is kept and reported by every later `https:` fetch.
pub(crate) struct LazyConfig {
    roots: Roots,
    made: OnceLock<Result<Arc<ClientConfig>, String>>,
}

impl LazyConfig {
    pub(crate) fn new(roots: Roots) -> LazyConfig {
        LazyConfig {
            roots,
            made: OnceLock::new(),
        }
    }

    fn get(&self) -> Result<Arc<ClientConfig>, String> {
        self.made
            .get_or_init(|| {
                let store = match &self.roots {
                    Roots::System => roots::load(&roots::system_bundle()),
                    Roots::Bundle(path) => roots::load(path),
                    Roots::Pem(bytes) => roots::from_pem(bytes),
                }?;
                client_config(store)
            })
            .clone()
    }
}

/// The ureq connector adding TLS to `https:` connections.
pub(crate) struct TlsConnector {
    config: Arc<LazyConfig>,
}

impl TlsConnector {
    pub(crate) fn new(config: Arc<LazyConfig>) -> TlsConnector {
        TlsConnector { config }
    }
}

impl fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TlsConnector")
    }
}

impl<In: Transport> Connector<In> for TlsConnector {
    type Out = Either<In, TlsTransport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, Error> {
        let Some(transport) = chained else {
            return Ok(None);
        };
        if !details.needs_tls() || transport.is_tls() {
            return Ok(Some(Either::A(transport)));
        }
        let config = self.config.get().map_err(|why| {
            record_failure(why);
            Error::Tls("no trust anchors")
        })?;
        let host = details
            .uri
            .host()
            .ok_or(Error::Tls("the URL has no host"))?;
        // An IPv6 literal arrives bracketed; the server name is the bare address.
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let name = ServerName::try_from(bare.to_string())
            .map_err(|_| Error::Tls("invalid TLS server name"))?;
        let mut conn = ClientConnection::new(config, name)
            .map_err(|_| Error::Tls("cannot start a TLS session"))?;
        let mut sock = TransportAdapter::new(transport.boxed());
        sock.set_timeout(details.timeout);
        if let Err(e) = handshake(&mut conn, &mut sock) {
            if matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) {
                return Err(Error::Timeout(details.timeout.reason));
            }
            record_failure(explain(&e, bare));
            return Err(Error::Tls("TLS handshake failed"));
        }
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        let stream = StreamOwned::new(conn, sock);
        Ok(Some(Either::B(TlsTransport { buffers, stream })))
    }
}

/// Runs the handshake to the end, our Finished message included.
fn handshake(conn: &mut ClientConnection, sock: &mut TransportAdapter) -> io::Result<()> {
    while conn.is_handshaking() {
        conn.complete_io(sock)?;
    }
    while conn.wants_write() {
        conn.write_tls(sock)?;
    }
    sock.flush()
}

/// A failed handshake in words a status line can show.
fn explain(error: &io::Error, host: &str) -> String {
    let Some(tls) = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<rustls::Error>())
    else {
        return format!("TLS connection to {host} failed: {error}");
    };
    let what = match tls {
        rustls::Error::InvalidCertificate(cert) => match cert {
            CertificateError::UnknownIssuer => {
                "its certificate is not from a trusted authority".into()
            }
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
                "its certificate is for another name".into()
            }
            CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
                "its certificate has expired (or the clock is wrong)".into()
            }
            CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
                "its certificate is not valid yet (or the clock is wrong)".into()
            }
            CertificateError::Revoked => "its certificate is revoked".into(),
            other => format!("its certificate was refused ({other:?})"),
        },
        other => other.to_string(),
    };
    format!("secure connection to {host} refused: {what}")
}

/// An established TLS connection as a ureq transport.
pub(crate) struct TlsTransport {
    buffers: LazyBuffers,
    stream: StreamOwned<ClientConnection, TransportAdapter>,
}

impl fmt::Debug for TlsTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TlsTransport")
    }
}

impl Transport for TlsTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        self.stream.get_mut().set_timeout(timeout);
        let output = &self.buffers.output()[..amount];
        self.stream.write_all(output)?;
        self.stream.flush()?;
        Ok(())
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        self.stream.get_mut().set_timeout(timeout);
        let input = self.buffers.input_append_buf();
        let amount = match self.stream.read(input) {
            Ok(amount) => amount,
            // A server that closes without close_notify. Many do, and
            // browsers accept it; a body framed by Content-Length or chunking
            // that ends early is still caught by ureq.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => 0,
            Err(e) => return Err(e.into()),
        };
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        self.stream.get_mut().get_mut().is_open()
    }

    fn is_tls(&self) -> bool {
        true
    }
}
