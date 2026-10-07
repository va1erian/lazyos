//! Requests that arrive while a prompt is up (review of #659, H4).
//!
//! `elevd` answers one request at a time, but it does not stop reading while
//! the prompt waits for the person at the screen: a program could otherwise
//! queue requests back to back in the kernel and the next prompt would open
//! the moment the last one closed. What arrives meanwhile is sorted at once
//! (`elevpolicy::queue`, `elevpolicy::backoff`):
//!
//! * `Release` is answered;
//! * a request from a caller whose prompts went unanswered is refused
//!   (`EAGAIN`), and one from a caller that already has a request in hand,
//!   or beyond the queue's bound, is refused (`EBUSY`), audited, no prompt;
//! * anything else waits its turn ([`State::queue`]).

use alloc::format;
use alloc::string::String;

use elevpolicy::approvals::Caller;
use elevpolicy::backoff::Hold;
use messenger_generated::os_lazy_elevd_v1 as wire;
use user::messenger::wait::{self, WaitItem};
use user::messenger::{errno, Endpoint, Error, Message, Parcel};
use user::sys;

use super::audit::Entry;
use super::{answer_now, caller_of, error_reply, Refusal, State};

/// The refusal for a caller held after unanswered prompts.
pub(crate) fn held(hold: Hold) -> Refusal {
    let seconds = hold
        .until()
        .saturating_sub(sys::clock())
        .div_ceil(100)
        .max(1);
    match hold {
        Hold::Caller { .. } => Refusal(
            errno::EAGAIN,
            format!(
                "this program's last administrator prompt was cancelled; \
                 it may ask again in {seconds} s"
            ),
        ),
        Hold::Quiet { .. } => Refusal(
            errno::EAGAIN,
            format!("an administrator prompt was just cancelled; try again in {seconds} s"),
        ),
    }
}

/// Wait for `display`'s answer to call `txn`, sorting every request that
/// arrives meanwhile. The reply, or the call's failure.
pub(crate) fn await_prompt(
    state: &mut State,
    display: &Endpoint,
    txn: u64,
) -> Result<Parcel, Error> {
    loop {
        let items = [WaitItem::Endpoint(state.server), WaitItem::Call(txn)];
        match wait::wait_items(&items, 0, None) {
            Ok(ready) => {
                if ready & 1 != 0 {
                    take_new(state);
                }
                if ready & 2 != 0 {
                    return display.await_reply(txn);
                }
            }
            // Never spin on a refused wait: just wait for the answer.
            Err(_) => return display.await_reply(txn),
        }
    }
}

/// Park until `until` (ticks), sorting what arrives: the pause after a
/// cancelled prompt, before the next one opens.
pub(crate) fn pause(state: &mut State, until: u64) {
    while sys::clock() < until {
        match wait::wait_any(&[state.server], 0, Some(until)) {
            Ok(ready) if ready & 1 != 0 => take_new(state),
            _ => {}
        }
    }
}

/// Sort every message queued on the endpoint now.
fn take_new(state: &mut State) {
    let mut buffer = core::mem::take(&mut state.intake_buffer);
    while let Ok(Some(message)) = state.server.poll_recv_with(&mut buffer) {
        sort(state, message);
    }
    state.intake_buffer = buffer;
}

fn sort(state: &mut State, message: Message) {
    let is_request =
        message.interface_id() == wire::INTERFACE_ID && message.method() == wire::METHOD_REQUEST;
    if !is_request {
        // `Release` and anything malformed: answered at once.
        let reply = answer_now(state, &message);
        reply_to(state, &message, &reply);
        return;
    }
    let caller = caller_of(&message);
    if let Err(hold @ Hold::Caller { .. }) = state.backoff.check(caller, sys::clock()) {
        refuse(state, &message, caller, "held", &held(hold));
        return;
    }
    if let Err(message) = state.queue.admit(state.active, caller, message) {
        let refusal = busy(state, caller);
        refuse(state, &message, caller, "busy", &refusal);
    }
}

fn busy(state: &State, caller: Caller) -> Refusal {
    if state.active == Some(caller) {
        Refusal::new(
            errno::EBUSY,
            "this program is already waiting for an administrator",
        )
    } else {
        Refusal::new(
            errno::EBUSY,
            "too many requests are waiting for an administrator",
        )
    }
}

/// Refuse `message` now, without a prompt, and audit it.
pub(crate) fn refuse(
    state: &mut State,
    message: &Message,
    caller: Caller,
    outcome: &str,
    refusal: &Refusal,
) {
    let operation = wire::decode_request_args(&message.parcel.body)
        .map(|args| args.operation)
        .unwrap_or_else(|_| String::from("?"));
    state.audit.log(&Entry::new(&operation, caller), outcome);
    let reply = error_reply(message.method(), refusal.0, &refusal.1);
    reply_to(state, message, &reply);
}

fn reply_to(state: &State, message: &Message, reply: &Parcel) {
    if let Some(txn) = message.txn {
        // A caller that gave up is not our problem.
        let _ = state.server.reply_or_drop(txn, reply);
    }
}
