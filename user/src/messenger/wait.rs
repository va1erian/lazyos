//! Waiting on several endpoints, and on the raw input bus, at once: the
//! native `wait` op (docs/performance-plan.md P1.3, P1.4).
//!
//! A service with two sources of work parks on both with [`wait_any`] and is
//! woken by whichever becomes ready, instead of sleeping on one with a short
//! deadline and polling the other. Nothing is received: take what is ready
//! with [`Endpoint::poll_recv_with`] (or drain the raw bus).

use super::endpoint::syscall;
use super::types::{MsgArgs, MsgResult, Result};
use super::{op, Endpoint};

/// Most endpoints one [`wait_any`] may name (the kernel's limit).
pub const MAX_ENDPOINTS: usize = 8;
/// Doorbell: the caller's raw input ring has records (`inputd` only).
pub const WAIT_RAW_INPUT: u64 = 1;
/// Doorbell: a key reached the display input queue (the display owner only).
pub const WAIT_DISPLAY_KEYS: u64 = 2;
/// Bit of the ready mask that means "the raw input ring holds records".
pub const RAW_INPUT_READY: u64 = 1 << 63;
/// Bit of the ready mask that means "the display input queue has events".
pub const DISPLAY_INPUT_READY: u64 = 1 << 62;

/// Park until one of `endpoints` has a message or a closed peer, or one of
/// the `doorbells` ([`WAIT_RAW_INPUT`], [`WAIT_DISPLAY_KEYS`]) rings, or
/// `deadline` (absolute ticks; `None` waits forever) passes. Returns the
/// ready mask: bit `i` for `endpoints[i]`, [`RAW_INPUT_READY`] and
/// [`DISPLAY_INPUT_READY`] for the doorbells. A deadline that passes first is
/// `-ETIMEDOUT`, as for `recv`; a doorbell this task may not use is
/// `-ENOENT`.
pub fn wait_any(endpoints: &[Endpoint], doorbells: u64, deadline: Option<u64>) -> Result<u64> {
    let mut handles = [0u64; MAX_ENDPOINTS];
    let count = endpoints.len().min(MAX_ENDPOINTS);
    for (handle, endpoint) in handles.iter_mut().zip(endpoints) {
        *handle = endpoint.handle();
    }
    let args = MsgArgs {
        parcel_ptr: handles.as_ptr() as u64,
        parcel_len: if endpoints.len() > MAX_ENDPOINTS {
            // Let the kernel refuse it rather than silently drop some.
            endpoints.len() as u64
        } else {
            count as u64
        },
        deadline: deadline.unwrap_or(0),
        flags: doorbells,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    syscall(op::WAIT, &args, &mut result)?;
    Ok(result.value)
}
