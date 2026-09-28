//! Messenger channels and transactions (#66).

use super::*;
use crate::ipc::channels::{self, Error as ChannelError};
use crate::ipc::handles;
use crate::task::{TaskState, WaitKind, WakeReason};
use libmessenger::{flags, Decoder, Encoder, Header, Kind, Parcel, VERSION};

/// Each channel test starts from an empty task table, an empty handle
/// table, an empty channel registry, and a runnable kernel task with no
/// stale wake reason.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    handles::reset_for_task(task::current());
    channels::reset();
    let me = task::current();
    let _ = task::harness::take_wake_reason(me);
    check!(
        task::harness::state(me) == Some(TaskState::Runnable),
        "kernel task is not runnable after reset: {:?}",
        task::harness::state(me)
    );
    Ok(())
}

/// Friendly-message adapter for `Result` plumbing.
fn reason(error: ChannelError) -> String {
    error.message().into()
}

/// Encode a complete parcel whose body carries one string field.
fn parcel(method: u32, parcel_flags: u16, text: &str) -> Result<Vec<u8>, String> {
    let mut body = Encoder::new();
    body.string(1, text).map_err(|error| error.message())?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: parcel_flags,
            interface_id: 0x1a2b_3c4d,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|error| error.message())?;
    Ok(bytes)
}

/// Decode the first string field of a parcel body.
fn payload(bytes: &[u8]) -> Result<String, String> {
    let parcel = Parcel::decode(bytes).map_err(|error| error.message())?;
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(|error| error.message())? {
        if field.kind == Kind::String {
            return Ok(field.as_str().map_err(|error| error.message())?.into());
        }
    }
    Err("parcel body has no string field".into())
}

fn blocked_call(slot: usize, deadline: Option<u64>) -> bool {
    matches!(
        task::harness::state(slot),
        Some(TaskState::Blocked {
            wait: WaitKind::Sleep,
            deadline: expected,
        }) if expected == deadline
    )
}

/// Spawn `count` client tasks, each holding its own handle to the callable
/// side `shared` of one channel, as `registry::resolve` hands every client
/// of a service an alias of the same endpoint. Returns `(slot, handle)`
/// pairs; `current()` is the kernel task again on return.
fn shared_clients(shared: u64, count: usize) -> Result<Vec<(usize, u64)>, String> {
    let entry = handles::get(shared).map_err(|error| error.message())?;
    let mut clients = Vec::new();
    for index in 0..count {
        task::harness::switch_current(task::KERNEL_TASK);
        let slot = task::spawn_fork().map_err(|error| format!("client {index}: {error}"))?;
        handles::reset_for_task(slot);
        let handle = handles::open_for_task(slot, entry.kind, entry.rights, entry.object_id)
            .map_err(|error| error.message())?;
        clients.push((slot, handle));
    }
    task::harness::switch_current(task::KERNEL_TASK);
    Ok(clients)
}

mod echo_and_calls;
mod resilience;

pub(super) use echo_and_calls::*;
pub(super) use resilience::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_channel_echo_roundtrip", echo_roundtrip),
    (
        "ipc_channel_one_way_order_and_limits",
        one_way_order_and_limits,
    ),
    ("ipc_channel_deadline_timeout", deadline_timeout),
    ("ipc_channel_deadline_reply_race", deadline_reply_race),
    ("ipc_channel_call_deadline_zero", call_deadline_zero),
    ("ipc_channel_cancel_wakes", cancel_wakes),
    ("ipc_channel_peer_died", peer_died),
    ("ipc_channel_deadlock_refused", deadlock_refused),
    (
        "ipc_channel_concurrent_clients_allowed",
        concurrent_clients_allowed,
    ),
    (
        "ipc_channel_concurrent_clients_soak",
        concurrent_clients_soak,
    ),
];
