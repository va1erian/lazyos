//! Name registry (issue #89).

use super::*;
use crate::ipc::registry::{self, Error as RegistryError};
use crate::ipc::syscalls::{
    errno, MsgArgs, MsgResult, OP_LIST, OP_REGISTER, OP_RESOLVE, OP_UNREGISTER,
};
use crate::ipc::{acl, audit, channels, credentials, handles};
use crate::task::TaskState;
use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

/// Friendly-message adapter for each error type the suite plumb through
/// `Result<_, String>`; a trait keeps `map_err` call sites terse and typed.
trait Friendly {
    fn friendly(self) -> String;
}

impl Friendly for libmessenger::Error {
    fn friendly(self) -> String {
        self.message().into()
    }
}

impl Friendly for channels::Error {
    fn friendly(self) -> String {
        self.message().into()
    }
}

impl Friendly for handles::Error {
    fn friendly(self) -> String {
        self.message().into()
    }
}

impl Friendly for RegistryError {
    fn friendly(self) -> String {
        self.message().into()
    }
}

fn friendly<E: Friendly>(error: E) -> String {
    error.friendly()
}

/// Scratch user address space for the syscall-level test, private per test
/// because `in_space` installs and frees a fresh table around it.
const SPACE: u64 = 0x0050_0000;

const SPACE_PAGES: u64 = 8;

const ARGS: u64 = SPACE;

const RESULT: u64 = SPACE + 0x100;

const REQUEST: u64 = SPACE + 0x1000;

const LIST_BUF: u64 = SPACE + 0x2000;

/// Every registry test starts from an empty fabric with the kernel task
/// current and runnable, so counts are deterministic.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in 0..task::MAX_TASKS {
        handles::reset_for_task(slot);
    }
    channels::reset();
    registry::reset();
    credentials::reset_for_task(task::KERNEL_TASK);
    acl::load(&[]);
    audit::reset();
    audit::set_trace(false);
    task::wake_task(task::KERNEL_TASK);
    let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
    Ok(())
}

/// Friendly-message adapter for registry plumbing.
fn reason(error: RegistryError) -> String {
    error.message().into()
}

/// Encode a registry request parcel whose body is already built.
fn encode_parcel(method: u32, body: Encoder) -> Result<Vec<u8>, String> {
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: registry::INTERFACE,
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
    parcel.encode(&mut bytes).map_err(friendly)?;
    Ok(bytes)
}

/// A request body carrying one name field.
fn string_parcel(method: u32, text: &str) -> Result<Vec<u8>, String> {
    let mut body = Encoder::new();
    body.string(registry::field::NAME, text).map_err(friendly)?;
    encode_parcel(method, body)
}

/// A register request: name, endpoint handle, interface array and lease.
fn register_parcel(
    name: &str,
    endpoint: u64,
    interfaces: &[u64],
    lease: u64,
) -> Result<Vec<u8>, String> {
    let mut body = Encoder::new();
    body.string(registry::field::NAME, name).map_err(friendly)?;
    body.u64(registry::field::ENDPOINT, endpoint)
        .map_err(friendly)?;
    let mut array = Encoder::new();
    for interface in interfaces {
        array
            .u64(registry::field::INTERFACES, *interface)
            .map_err(friendly)?;
    }
    body.array(registry::field::INTERFACES, &array)
        .map_err(friendly)?;
    body.u64(registry::field::LEASE_TICKS, lease)
        .map_err(friendly)?;
    encode_parcel(registry::method::REGISTER, body)
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

/// Two's-complement `-errno` as the syscall returns it in `rax`.
fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
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

/// One op through the native gate with the args block at [`ARGS`].
fn dispatch(op: u64, args: &MsgArgs) -> (u64, MsgResult) {
    write_bytes(ARGS, &args.to_bytes());
    let code = process::dispatch_for_test(5, op, ARGS, RESULT);
    let result = MsgResult::from_bytes(&read_bytes(RESULT, 64))
        .expect("the kernel wrote a malformed result block");
    (code, result)
}

mod acl_and_proxy;
mod register_and_resolve;

pub(super) use acl_and_proxy::*;
pub(super) use register_and_resolve::*;

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "ipc_registry_register_resolve_roundtrip",
        register_resolve_roundtrip,
    ),
    ("ipc_registry_unknown_name_friendly", unknown_name_friendly),
    ("ipc_registry_lease_expiry_prunes", lease_expiry_prunes),
    ("ipc_registry_owner_death_releases", owner_death_releases),
    ("ipc_registry_acl_denies_register", acl_denies_register),
    ("ipc_registry_list_reflects_state", list_reflects_state),
    (
        "ipc_registry_proxy_registers_for_client",
        proxy_registers_for_client,
    ),
    ("ipc_registry_syscall_roundtrip", syscall_roundtrip),
];
