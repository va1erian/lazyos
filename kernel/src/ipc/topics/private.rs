//! The per-uid topic namespace (issue #407): `user/<uid>/...` belongs to that
//! uid.
//!
//! `confd` announces a change to `user/<uid>/<path>` on
//! `user/<uid>/confd/changed/<path>`, so the desktop can follow one user's own
//! settings (a per-user theme). The segment rules of [`super::authorize`] key
//! on segment *text* and cannot say "only the uid this segment names", so this
//! rule is separate and always applies, whatever policy is loaded:
//!
//! * root (uid 0) may publish and subscribe anywhere, as it may read every
//!   `user/<uid>` key in `confd`;
//! * any other uid may name `user/<uid>/...` only with its own uid, spelled
//!   canonically (decimal, no leading zero), as the second segment;
//! * a filter that could match another uid's topics is refused: `user` with a
//!   wildcard or a missing second segment, and a filter whose *first* segment
//!   is a wildcard (`#`, `+/...`), which would match `user/<any>/...` too.
//!
//! The rule only narrows: a name it accepts still goes through the policy.

use crate::ipc::acl::reason;
use crate::ipc::audit::{self, AuditEvent};
use crate::ipc::credentials::Cred;

/// The first segment of the private namespace.
pub const ROOT_SEGMENT: &str = "user";

/// Whether `uid` may use the (validated) topic or filter `name`.
pub fn allows(uid: u32, name: &str) -> bool {
    if uid == 0 {
        return true;
    }
    let mut segments = name.split('/');
    match segments.next() {
        Some("#" | "+") => false,
        Some(ROOT_SEGMENT) => segments.next().is_some_and(|owner| is_uid(owner, uid)),
        _ => true,
    }
}

/// Whether `segment` is `uid` in canonical decimal. Compared digit by digit,
/// so the check allocates nothing.
fn is_uid(segment: &str, uid: u32) -> bool {
    let mut digits = [0u8; 10];
    let mut len = 0;
    let mut rest = uid;
    loop {
        digits[digits.len() - 1 - len] = b'0' + (rest % 10) as u8;
        len += 1;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    segment.as_bytes() == &digits[digits.len() - len..]
}

/// Audit a refusal: unlike a policy verdict it never reaches
/// [`crate::ipc::authorize`], so it is recorded here.
pub(super) fn deny(actor_slot: usize, cred: &Cred, interface_id: u64, txn_id: u64) {
    audit::record(AuditEvent {
        ticks: crate::task::ticks(),
        actor_slot,
        uid: cred.uid,
        label_id: cred.label_id,
        interface_id,
        method: super::segment_method(ROOT_SEGMENT),
        allow: false,
        reason_code: reason::PRIVATE_NAMESPACE,
        txn_id,
    });
}
