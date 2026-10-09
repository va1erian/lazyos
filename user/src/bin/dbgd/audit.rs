//! The audit trail: every request, and every refusal as a security record.
//!
//! Each record is one serial line, which also lands in the boot log (and so
//! in `logd`'s view of the console and the next boot's `dmesg`):
//!
//! ```text
//! DBGD:AUDIT peer=10.0.2.2 method=log.tail outcome=ok
//! DBGD:SECURITY peer=10.0.2.2 reason="bad key (failure 1)"
//! ```
//!
//! Every value is single-token or quoted and printable: a method name or a
//! reason that came off the network cannot forge a second line or a field.

use alloc::format;
use alloc::string::String;

use user::messenger::netsock::Addr;
use user::sys;

/// `text` made safe for one log token: printable ASCII, no spaces or quotes,
/// at most 40 characters.
fn token(text: &str) -> String {
    text.chars()
        .take(40)
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' {
                c
            } else {
                '?'
            }
        })
        .collect()
}

fn peer_text(peer: Addr) -> String {
    format!(
        "{}.{}.{}.{}:{}",
        peer.ip[0], peer.ip[1], peer.ip[2], peer.ip[3], peer.port
    )
}

/// One request (or connection event) and what came of it.
pub(crate) fn note(peer: Addr, method: &str, outcome: &str) {
    sys::write_str(&format!(
        "DBGD:AUDIT peer={} method={} outcome={}\n",
        peer_text(peer),
        token(method),
        token(outcome)
    ));
}

/// A refusal worth a security record: a wrong key, a request before
/// authentication, a peer that is not allowed.
pub(crate) fn security(peer: Addr, reason: &str) {
    let quoted: String = reason
        .chars()
        .take(80)
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .filter(|c| *c != '"')
        .collect();
    sys::write_str(&format!(
        "DBGD:SECURITY peer={} reason=\"{quoted}\"\n",
        peer_text(peer)
    ));
}
