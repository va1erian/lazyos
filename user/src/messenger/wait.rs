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

/// Most items (endpoints and calls) one wait may name (the kernel's limit).
pub const MAX_ENDPOINTS: usize = 8;
/// Marks a word of the wait set as a call's transaction id (the kernel's
/// `channels::WAIT_ITEM_CALL`).
pub const WAIT_ITEM_CALL: u64 = 1 << 63;
/// Doorbell: the caller's raw input ring has records (`inputd` only).
pub const WAIT_RAW_INPUT: u64 = 1;
/// Doorbell: a key reached the display input queue (the display owner only).
pub const WAIT_DISPLAY_KEYS: u64 = 2;
/// Doorbell: an application acted on an `AF_INET` socket (the attached
/// `netd` only, docs/performance-plan.md P4.1).
pub const WAIT_INET: u64 = 4;
/// Doorbell: a child of the caller finished and waits to be reaped with
/// [`crate::sys::wait`] (any task; docs/performance-plan.md P7.1). It stays
/// ready until every finished child is reaped.
pub const WAIT_CHILD: u64 = 8;
/// Bit of the ready mask that means "the raw input ring holds records".
pub const RAW_INPUT_READY: u64 = 1 << 63;
/// Bit of the ready mask that means "the display input queue has events".
pub const DISPLAY_INPUT_READY: u64 = 1 << 62;
/// Bit of the ready mask that means "the `AF_INET` pump has work".
pub const INET_READY: u64 = 1 << 61;
/// Bit of the ready mask that means "a child waits to be reaped".
pub const CHILD_READY: u64 = 1 << 60;

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
