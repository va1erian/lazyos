//! Generated-codec edges and sustained load for the `authorize_topic` gate
//! (issue #299). Split from `topics_suite` to keep both files small; the
//! helpers are shared with it.

use super::topics_suite::{
    auth_parcel, authorize_raw, deny_rule, failed, fresh, in_space, wrap_body,
};
use super::*;
use crate::ipc::credentials::Cred;
use crate::ipc::syscalls::errno;
use crate::ipc::{acl, audit, credentials, topics};
use libmessenger::Encoder;

/// The request decoder is the generated one: unknown trailing fields are
/// ignored (forward compatibility), and a truncated body is `-EINVAL`, never
/// a panic or a silent allow.
pub fn gate_codec_edges() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let request = topics::publish_scope::AuthorizeTopicArgs {
            name: String::from("system/events"),
            mode: topics::MODE_PUBLISH,
            txn: 9,
        };
        let mut body = topics::publish_scope::encode_authorize_topic_args(&request)
            .map_err(|error| error.message())?;
        // Field 9 is unknown to the request layout and must be skipped.
        let mut extra = Encoder::new();
        extra.u64(9, 0xdead).map_err(|error| error.message())?;
        body.extend_from_slice(&extra.finish());
        let (code, result) = authorize_raw(&wrap_body(body.clone())?);
        check!(code == 0 && result.value == 2, "unknown field -> {code:#x}");

        // Every strict prefix of a valid body is either decoded as a shorter
        // valid body or refused; none may crash the gate.
        let whole = topics::publish_scope::encode_authorize_topic_args(&request)
            .map_err(|error| error.message())?;
        for cut in 0..whole.len() {
            let (code, _) = authorize_raw(&wrap_body(whole[..cut].to_vec())?);
            check!(
                code == 0 || code == failed(errno::EINVAL),
                "truncated at {cut} -> {code:#x}"
            );
        }
        // Cutting mid-field is a codec error, not an allow.
        let (code, _) = authorize_raw(&wrap_body(whole[..3].to_vec())?);
        check!(code == failed(errno::EINVAL), "mid-field cut -> {code:#x}");
        Ok(())
    })
}

/// Sustained load through the gate: thousands of allowed and denied requests
/// must keep answering correctly, audit exactly the denials, and leak
/// nothing (the ring is bounded, so the monotonic total is the invariant).
pub fn gate_stress() -> Result<(), String> {
    const ROUNDS: u64 = 3000;
    fresh()?;
    in_space(|| -> Result<(), String> {
        credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
        // Deny only the *subscribe* scope's `secret`; publish stays allowed,
        // pinning that each mode authorizes on its own interface id.
        acl::load(&deny_rule(
            topics::SUBSCRIBE_INTERFACE,
            topics::segment_method("secret"),
        ));
        let allowed = auth_parcel("public/secret/x", topics::MODE_PUBLISH, 1)?;
        let denied = auth_parcel("public/secret/x", topics::MODE_SUBSCRIBE, 2)?;
        let before = audit::total();
        for round in 0..ROUNDS {
            let (code, result) = authorize_raw(&allowed);
            check!(
                code == 0 && result.value == 3,
                "round {round}: allowed publish -> {code:#x}"
            );
            let (code, _) = authorize_raw(&denied);
            check!(
                code == failed(errno::EACCES),
                "round {round}: denied subscribe -> {code:#x}"
            );
        }
        check!(
            audit::total() == before + ROUNDS,
            "expected {ROUNDS} audited denials, saw {}",
            audit::total() - before
        );
        Ok(())
    })
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_topic_gate_codec_edges", gate_codec_edges),
    ("ipc_topic_gate_stress", gate_stress),
];
