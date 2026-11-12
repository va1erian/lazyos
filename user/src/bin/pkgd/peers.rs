//! `pkgd`'s calls to the services it drives (`confd`, `mimed`, `init`,
//! `accountsd`): generated clients, each built once (a connection resolves on
//! its first call and again after a restart), driven through [`retry`].

use libmessenger::client::{CallError, Errno};
use user::messenger;

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
