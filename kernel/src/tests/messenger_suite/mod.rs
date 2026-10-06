//! Native Messenger syscalls and bootstrap (issue #69).

use super::*;
use crate::ipc::stats::{FabricStats, FABRIC_STATS_VERSION};
use crate::ipc::syscalls::{
    self, errno, MsgArgs, MsgResult, MsgStats, OP_CALL, OP_CALL_AWAIT, OP_CALL_BEGIN, OP_CANCEL,
    OP_CLOSE_ENDPOINT, OP_CREATE_PAIR, OP_RECV, OP_REPLY, OP_SEND, OP_STATS, OP_TOTALS,
};
use crate::ipc::{acl, audit, channels, credentials, handles};
use crate::task::TaskState;
use libmessenger::{flags, Decoder, Encoder, Header, Kind, Parcel, VERSION};

const IFACE: u64 = 0x6969_6969_6969_6969;

/// Scratch user address space for the syscall tests. `dispatch` validates
/// pointers against the active CR3, so each test installs a fresh table and
/// restores the kernel's afterwards.
const SPACE: u64 = 0x0040_0000;

const SPACE_PAGES: u64 = 8;

/// Blocks inside the scratch space, one per page so page-crossing copies
/// are not a factor in these tests.
const ARGS: u64 = SPACE;

const RESULT: u64 = SPACE + 0x100;

const REQUEST: u64 = SPACE + 0x1000;

const RECV_BUF: u64 = SPACE + 0x2000;

const REPLY_BUF: u64 = SPACE + 0x3000;

const STATS_BUF: u64 = SPACE + 0x4000;

/// Each test starts from the bring-up state: kernel task current and
/// runnable, no handles, channels, policy, audit events, or bootstrap.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in 0..task::MAX_TASKS {
        handles::reset_for_task(slot);
    }
    channels::reset();
    syscalls::bootstrap::reset();
    credentials::reset_for_task(task::KERNEL_TASK);
    acl::load(&[]);
    audit::reset();
    audit::set_trace(false);
    // A failed earlier test can leave the kernel task parked; a stale wake
    // reason must not leak into this one.
    task::wake_task(task::KERNEL_TASK);
    let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
    Ok(())
}

/// Friendly-message adapter for `Result` plumbing.
fn reason(error: channels::Error) -> String {
    error.message().into()
}

/// Two's-complement `-errno` as the syscall returns it in `rax`.
fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Run `f` with [`SPACE`] mapped into a fresh address space installed as
/// CR3, exactly as a real syscall from a user task would find it.
fn in_space<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
    let kernel = mem::kernel_table();
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    process::map_range(table, SPACE, SPACE + SPACE_PAGES * 4096).map_err(to_string)?;
    mem::switch_to(table);
    let outcome = f();
    mem::switch_to(kernel);
    mem::free_user_table(table);
    outcome
}

/// Encode a parcel whose body carries one string field.
fn parcel(method: u32, parcel_flags: u16, text: &str) -> Result<Vec<u8>, String> {
    let mut body = Encoder::new();
    body.string(1, text).map_err(|error| error.message())?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: parcel_flags,
            interface_id: IFACE,
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
fn string_field(bytes: &[u8]) -> Result<String, String> {
    let parcel = Parcel::decode(bytes).map_err(|error| error.message())?;
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(|error| error.message())? {
        if field.kind == Kind::String {
            return Ok(field.as_str().map_err(|error| error.message())?.into());
        }
    }
    Err("parcel body has no string field".into())
}

/// Write bytes into the installed scratch space.
fn write_bytes(va: u64, bytes: &[u8]) {
    // Safety: the scratch pages are mapped writable while installed.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), va as *mut u8, bytes.len()) };
}

/// Read bytes from the installed scratch space.
fn read_bytes(va: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.resize(len, 0);
    // Safety: the scratch pages are mapped readable while installed.
    unsafe { core::ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), len) };
    out
}

/// Run one op through the native gate with the args block at [`ARGS`] and
/// decode the result block.
fn syscall(op: u64, args: &MsgArgs) -> (u64, MsgResult) {
    write_bytes(ARGS, &args.to_bytes());
    let code = process::dispatch_for_test(5, op, ARGS, RESULT);
    let result = MsgResult::from_bytes(&read_bytes(RESULT, 64))
        .expect("the kernel wrote a malformed result block");
    (code, result)
}

mod bootstrap_and_stats;
mod release_flag;
mod sender_id;
mod syscall_core;

pub(super) use bootstrap_and_stats::*;
pub(super) use release_flag::*;
pub(super) use sender_id::*;
pub(super) use syscall_core::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_messenger_syscall_echo", syscall_echo),
    ("ipc_messenger_syscall_timeout", syscall_timeout),
    ("ipc_messenger_syscall_denied", syscall_denied),
    ("ipc_messenger_syscall_bad_pointer", syscall_bad_pointer),
    ("ipc_messenger_late_reply_is_enoent", late_reply_is_enoent),
    ("ipc_messenger_bootstrap_claim", bootstrap_claim),
    (
        "ipc_messenger_release_flag_is_accepted_only_on_close",
        release_flag_is_accepted_only_on_close,
    ),
    ("ipc_messenger_fabric_stats_abi", syscall_fabric_stats),
    (
        "ipc_messenger_recv_reports_queue_time_sender",
        recv_reports_queue_time_sender,
    ),
    (
        "ipc_messenger_recv_sender_id_refuses_bad_requests",
        recv_sender_id_refuses_bad_requests,
    ),
    (
        "ipc_messenger_sender_id_carries_labels_and_the_kernel",
        sender_id_carries_labels_and_the_kernel,
    ),
    ("ipc_messenger_sender_id_soak", sender_id_soak),
    (
        "ipc_messenger_sender_id_survives_slot_reuse",
        sender_id_survives_slot_reuse,
    ),
];
