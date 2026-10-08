//! The compositor's private channel (docs/accounts-plan.md U2, issue #625).
//!
//! The compositor `Attach`es once on the shared service endpoint and hands
//! `inputd` one end of a channel for shell events. It then sends every other
//! call, shell and its own key session's `Open` alike, on that same channel:
//! no client holds an end of it, so a client that fills the shared endpoint's
//! queue (every client of `inputd` shares it) cannot hold back a focus change.
//! The trusted prompt depends on this: it opens only once `inputd` confirmed
//! that no client window has the keyboard (`xuid/prompt_keys.rs`).
//!
//! The channel is served ahead of the shared endpoint, and again before each
//! client request is handled, so a surface the compositor noted before
//! answering its client's `CreateSurface` is known by the time that client's
//! `Open` is handled (the order a single endpoint used to give).

use user::messenger::input::{self as api, shell_wire};
use user::messenger::{errno, Error};

use super::hub::Hub;

/// Most requests served from the channel per call: the compositor is
/// trusted, but the raw input bus must never wait behind an unbounded drain.
const MAX_PER_PASS: usize = 32;

/// Serve what the compositor queued on its channel. A dead channel means
/// the compositor is gone.
pub(super) fn serve(hub: &mut Hub, buffer: &mut [u8]) {
    for _ in 0..MAX_PER_PASS {
        let Some(channel) = hub.shell_endpoint() else {
            return;
        };
        let message = match channel.poll_recv_with(buffer) {
            Ok(Some(message)) => message,
            Ok(None) => return,
            Err(_) => {
                hub.shell_lost();
                return;
            }
        };
        // `Attach` belongs on the shared endpoint: the channel already is
        // the attached compositor's.
        let reply = if message.interface_id() == api::SHELL_INTERFACE
            && message.method() == shell_wire::METHOD_ATTACH
        {
            // The endpoint it carried stays the message's and closes with it.
            Hub::error_reply(&message, Error::Errno(-errno::EINVAL))
        } else {
            hub.handle(&message)
                .unwrap_or_else(|error| Hub::error_reply(&message, error))
        };
        if let Some(txn) = message.txn {
            let _ = channel.reply(txn, &reply);
        }
    }
}
