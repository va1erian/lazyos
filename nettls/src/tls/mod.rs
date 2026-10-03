//! The TLS layer: a rustls client configuration and a ureq connector that
//! wraps each `https` connection in it.
//!
//! ureq's own rustls connector is not used: it sets no ALPN, cannot report
//! the negotiated parameters, and comes with switches (`disable_verification`,
//! compiled-in `webpki-roots`) this program must not have. This one is the
//! only TLS path, verification is always webpki against [`crate::roots`], and
//! the handshake runs on the calling thread.

pub mod explain;

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, Transport,
    TransportAdapter,
};
use ureq::Error;

use crate::report::TlsFailure;

/// The client configuration: TLS 1.2 and 1.3 with `nettls-crypto`'s suites
/// (AES-GCM and ChaCha20-Poly1305 with ECDHE only), SNI on, ALPN
/// `http/1.1`, server certificates verified by webpki against `roots`.
pub fn client_config(roots: RootCertStore) -> Result<Arc<ClientConfig>, String> {
    let provider = nettls_crypto::provider_arc();
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS configuration: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// What the connector learned about the last handshake, for the caller.
#[derive(Default)]
pub struct TlsLog {
    /// Set when the last handshake failed: the explained failure.
    pub failure: Option<TlsFailure>,
}

/// The ureq connector that adds TLS to `https` connections.
pub struct TlsConnector {
    config: Arc<ClientConfig>,
    verbose: bool,
    log: Arc<Mutex<TlsLog>>,
}

impl TlsConnector {
    /// A connector for `config`; with `verbose`, each handshake prints its
    /// parameters and the server's chain to stderr. Failures are recorded in
    /// `log` so the caller can explain them and pick the exit code.
    pub fn new(config: Arc<ClientConfig>, verbose: bool, log: Arc<Mutex<TlsLog>>) -> Self {
        TlsConnector {
            config,
            verbose,
            log,
        }
    }

    fn record_failure(&self, failure: TlsFailure) {
        if let Ok(mut log) = self.log.lock() {
            log.failure = Some(failure);
        }
    }
}

impl fmt::Debug for TlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsConnector")
            .field("verbose", &self.verbose)
            .finish()
    }
}

/// The host name for SNI and certificate matching: the URI's host without
/// IPv6 brackets.
pub fn server_name(host: &str) -> Result<ServerName<'static>, String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(bare.to_string())
        .map_err(|_| format!("'{host}' is not a valid TLS server name"))
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
        let host = details.uri.host().ok_or(Error::Tls("URL has no host"))?;
        let name = server_name(host).map_err(|_| Error::Tls("invalid TLS server name"))?;
        let mut conn = ClientConnection::new(self.config.clone(), name)
            .map_err(|_| Error::Tls("cannot start a TLS session"))?;
        let mut sock = TransportAdapter::new(transport.boxed());
        sock.set_timeout(details.timeout);
        if let Err(e) = handshake(&mut conn, &mut sock) {
            let failure = explain::failure(&e, host);
            if self.verbose {
                eprintln!(
                    "TLS:FAIL reason=\"{}\" sni={}",
                    failure.reason,
                    crate::sanitize::printable_str(host)
                );
            }
            let timed_out = matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            );
            self.record_failure(failure);
            return Err(if timed_out {
                Error::Timeout(details.timeout.reason)
            } else {
                Error::Tls("TLS handshake failed")
            });
        }
        if self.verbose {
            for line in explain::handshake_lines(&conn, host) {
                eprintln!("{line}");
            }
        }
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        let stream = StreamOwned::new(conn, sock);
        Ok(Some(Either::B(TlsTransport { buffers, stream })))
    }
}

/// Drive the handshake to completion (including flushing our Finished).
fn handshake(conn: &mut ClientConnection, sock: &mut TransportAdapter) -> io::Result<()> {
    while conn.is_handshaking() {
        conn.complete_io(sock)?;
    }
    while conn.wants_write() {
        conn.write_tls(sock)?;
    }
    sock.flush()
}

/// An established TLS connection, as a ureq transport.
pub struct TlsTransport {
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
        let amount = self.stream.read(input)?;
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
