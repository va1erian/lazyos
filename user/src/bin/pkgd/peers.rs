//! `pkgd`'s connections to the services it drives (`confd`, `mimed`, `init`).
//!
//! `registry::resolve` opens a fresh handle in this task each time, and `pkgd`
//! is a long-lived service with a bounded handle table, so each peer is
//! resolved once and cached. Two things go wrong with a shared service
//! endpoint, and [`Peer::run`] absorbs both: the kernel refuses a second
//! synchronous call while another transaction is open on the channel
//! (`-EDEADLK`, `-EAGAIN`), which is retried a tick later, and a peer that
//! restarted leaves a dead endpoint (`-EPIPE`), which is released and resolved
//! again.

use user::messenger::{self, errno, registry, Endpoint, Error, Result};

/// Retries before giving up on a busy or absent peer (about a second).
const ATTEMPTS: usize = 100;

/// One cached connection to a named service.
pub(crate) struct Peer {
    name: &'static str,
    endpoint: Option<Endpoint>,
}

impl Peer {
    pub(crate) const fn new(name: &'static str) -> Peer {
        Peer {
            name,
            endpoint: None,
        }
    }

    /// Run `op` against the peer, resolving it first and retrying the
    /// transient failures. A failure that is the peer's own answer (a service
    /// error code) is returned at once.
    pub(crate) fn run<T>(&mut self, mut op: impl FnMut(Endpoint) -> Result<T>) -> Result<T> {
        let mut last = Error::Errno(-errno::ENOENT);
        for _ in 0..ATTEMPTS {
            let endpoint = match self.endpoint {
                Some(endpoint) => endpoint,
                None => match registry::resolve(self.name) {
                    Ok(endpoint) => {
                        self.endpoint = Some(endpoint);
                        endpoint
                    }
                    Err(error) => {
                        last = error;
                        messenger::park_tick();
                        continue;
                    }
                },
            };
            match op(endpoint) {
                Ok(value) => return Ok(value),
                Err(error) => match error.errno() {
                    Some(code) if code == -errno::EDEADLK || code == -errno::EAGAIN => {
                        last = error;
                        messenger::park_tick();
                    }
                    Some(code)
                        if matches!(error, Error::Errno(_))
                            && (code == -errno::EPIPE || code == -errno::ENOENT) =>
                    {
                        // The peer went away (restarted): drop the stale handle
                        // and resolve the new one.
                        last = error;
                        self.forget();
                        messenger::park_tick();
                    }
                    _ => return Err(error),
                },
            }
        }
        Err(last)
    }

    /// Release the cached endpoint (the other holders keep theirs).
    fn forget(&mut self) {
        if let Some(endpoint) = self.endpoint.take() {
            let _ = endpoint.release();
        }
    }
}
