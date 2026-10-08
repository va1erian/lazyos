//! Waiting on several endpoints, pending calls and doorbells at once: the
//! native `wait` op (docs/performance-plan.md P1.3, P1.4; issue #309, design
//! note docs/architecture/wait-any.md).
//!
//! A service with two sources of work parks on both with [`wait_any`] and is
//! woken by whichever becomes ready, instead of sleeping on one with a short
//! deadline and polling the other. Nothing is received: take what is ready
//! with [`Endpoint::poll_recv_with`] (or drain the raw bus), or finish a
//! ready call with [`Endpoint::await_reply`], which then returns at once.
//! A topic subscription takes part through its doorbell endpoint
//! (`central::Subscription::bell`), or as the pending call of its
//! outstanding `NextEvent` ([`WaitItem::Call`]).

use super::endpoint::syscall;
use super::types::{MsgArgs, MsgResult, Result};
use super::{op, Endpoint};

/// The wait set's limit, doorbells and ready-mask bits (the kernel's
/// `channels::recv::waitset`), from `lazyos-sys`.
pub use lazyos_sys::msg::{
    CHILD_READY, DISPLAY_INPUT_READY, INET_READY, RAW_INPUT_READY, WAIT_CHILD, WAIT_DISPLAY_KEYS,
    WAIT_INET, WAIT_ITEM_CALL, WAIT_MAX_ENDPOINTS as MAX_ENDPOINTS, WAIT_RAW_INPUT,
};

/// One thing a [`wait_items`] set can wait for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitItem {
    /// Ready when the endpoint has a message or its peer closed.
    Endpoint(Endpoint),
    /// Ready when the transaction (from [`Endpoint::begin_call`], not yet
    /// awaited) ended: replied, timed out at its own deadline, canceled, or
    /// its peer died. It must be this task's call.
    Call(u64),
}

impl WaitItem {
    /// The word the kernel reads for this item.
    const fn word(self) -> u64 {
        match self {
            WaitItem::Endpoint(endpoint) => endpoint.handle(),
            WaitItem::Call(txn) => txn | WAIT_ITEM_CALL,
        }
    }
}

/// Park until one of `endpoints` has a message or a closed peer, or one of
/// the `doorbells` ([`WAIT_RAW_INPUT`], [`WAIT_DISPLAY_KEYS`], [`WAIT_INET`],
/// [`WAIT_CHILD`]) rings, or
/// `deadline` (absolute ticks; `None` waits forever) passes. Returns the
/// ready mask: bit `i` for `endpoints[i]`, [`RAW_INPUT_READY`],
/// [`DISPLAY_INPUT_READY`], [`INET_READY`] and [`CHILD_READY`] for the
/// doorbells. A deadline that passes first is
/// `-ETIMEDOUT`, as for `recv`; a doorbell this task may not use is
/// `-ENOENT`.
pub fn wait_any(endpoints: &[Endpoint], doorbells: u64, deadline: Option<u64>) -> Result<u64> {
    let mut words = [0u64; MAX_ENDPOINTS];
    for (word, endpoint) in words.iter_mut().zip(endpoints) {
        *word = endpoint.handle();
    }
    wait_words(&words, endpoints.len(), doorbells, deadline)
}

/// [`wait_any`] over endpoints and pending calls ([`WaitItem`]): bit `i` of
/// the mask is `items[i]`. A call's own deadline wakes the wait (the call is
/// then ready and its await returns `-ETIMEDOUT`); `deadline` bounds the wait
/// itself. A call that is not this task's is `-EPERM`, an unknown one
/// `-ENOENT`.
pub fn wait_items(items: &[WaitItem], doorbells: u64, deadline: Option<u64>) -> Result<u64> {
    let mut words = [0u64; MAX_ENDPOINTS];
    for (word, item) in words.iter_mut().zip(items) {
        *word = item.word();
    }
    wait_words(&words, items.len(), doorbells, deadline)
}

/// Issue the `wait` op over the first `len` of `words`.
fn wait_words(
    words: &[u64; MAX_ENDPOINTS],
    len: usize,
    doorbells: u64,
    deadline: Option<u64>,
) -> Result<u64> {
    let args = MsgArgs {
        parcel_ptr: words.as_ptr() as u64,
        // An oversized set reaches the kernel as its real length, which it
        // refuses before reading, rather than being silently cut short.
        parcel_len: len as u64,
        deadline: deadline.unwrap_or(0),
        flags: doorbells,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    syscall(op::WAIT, &args, &mut result)?;
    Ok(result.value)
}
