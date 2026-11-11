//! `pkgd`'s connections to the services it drives (`confd`, `mimed`, `init`).
//!
//! `confd` and `mimed` are generated clients driven through [`retry`]; `init`
//! is still a hand-resolved [`Peer`] until its client is generated.
//!
//! `registry::resolve` opens a fresh handle in this task each time, and `pkgd`
//! is a long-lived service with a bounded handle table, so each peer is
//! resolved once and cached. Two things go wrong with a shared service
//! endpoint, and [`Peer::run`] absorbs both: the kernel refuses a second
//! synchronous call while another transaction is open on the channel
//! (`-EDEADLK`, `-EAGAIN`), which is retried a tick later, and a peer that
//! restarted leaves a dead endpoint (`-EPIPE`), which is released and resolved
//! again.

use libmessenger::client::{CallError, Errno};
use user::messenger::{self, errno, registry, Endpoint, Error, Result};

/// Retries before giving up on a busy or absent peer (about a second).
const ATTEMPTS: usize = 100;

/// Run `op` on a generated client, retrying a busy peer: the kernel refuses a
/// second synchronous call while another transaction is open on the channel
/// (`EDEADLK`, `EAGAIN`), which is retried a tick later; a peer that is gone
/// is re-resolved by the client's connection and, if it is still absent,
/// retried here until it registers. A service's own refusal returns at once.
pub(crate) fn retry<T, E>(
    mut op: impl FnMut() -> core::result::Result<T, CallError<E>>,
) -> core::result::Result<T, CallError<E>> {
    let mut last = CallError::Transport(Errno::NOENT);
    for _ in 0..ATTEMPTS {
        match op() {
            Err(CallError::Transport(errno))
                if [Errno::DEADLK, Errno::AGAIN, Errno::PIPE, Errno::NOENT].contains(&errno) =>
            {
                last = CallError::Transport(errno);
                messenger::park_tick();
            }
            other => return other,
        }
    }
    Err(last)
}

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
