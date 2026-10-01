//! The real [`Bus`]: LazyOS's native Messenger gate (`int 0x80`, syscall 5).
//!
//! The gate is dispatched by task, not by binary kind, so a static-musl
//! program (the `rhai` command, the LazyRAD player) reaches the same code a
//! native `user` program does. This is the same call sequence as
//! `xui-app/src/sys/messenger.rs`; bodies come from the `msg` codec, and the
//! registry's `Resolve`/`List` from the compiled `messenger-generated` stubs,
//! so no wire field is written by hand here.
//!
//! Only meaningful inside LazyOS: on another kernel `int 0x80` is a different
//! ABI, so a program gets a [`Gate`] only from [`Gate::detect`], which asks the
//! kernel's name through the Linux `uname` syscall (valid on both) first.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;

use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::os_lazy_messenger_registry_v1 as registry;

use super::bus::{Bus, BusError};

/// `messenger(op, args, result)`.
const SYS_MESSENGER: u64 = 5;
/// `clock()`: the PIT tick counter, 100 Hz.
const SYS_CLOCK: u64 = 8;
const TICK_MS: u64 = 10;

/// Messenger op codes (`user/src/messenger/mod.rs` `op`).
mod op {
    pub const CALL: u64 = 1;
    pub const SEND: u64 = 3;
    pub const RESOLVE: u64 = 14;
    pub const LIST: u64 = 16;
}

/// `MsgArgs::txn_id` for registry ops: act on the calling task.
const TARGET_SELF: u64 = u64::MAX;
/// Largest reply a call accepts (services cap inline payloads well below).
const REPLY_BUFFER: usize = 64 * 1024;
/// Largest name-table snapshot (the registry's own client uses 32 KiB).
const LIST_BUFFER: usize = 32 * 1024;
const EINVAL: i64 = 22;
const E2BIG: i64 = 7;

/// The kernel's request block (`MsgArgs`).
#[repr(C)]
#[derive(Default)]
struct MsgArgs {
    handle: u64,
    txn_id: u64,
    parcel_ptr: u64,
    parcel_len: u64,
    buf_ptr: u64,
    buf_cap: u64,
    deadline: u64,
    flags: u64,
}

/// The kernel's response block (`MsgResult`).
#[repr(C)]
#[derive(Default)]
struct MsgResult {
    status: i64,
    value: u64,
    aux: u64,
    bytes: u64,
    reserved: [u64; 4],
}

fn native(nr: u64, a1: u64, a2: u64, a3: u64) -> i64 {
    let code: u64;
    // SAFETY: the native `int 0x80` gate with its register convention (number
    // in rax, arguments in rdi/rsi/rdx, result in rax; rcx/r11 clobbered). The
    // kernel validates every pointer argument against this task's address
    // space, and the blocks it reads/writes live on this stack frame.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") nr,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
}

fn messenger(op: u64, args: &MsgArgs, result: &mut MsgResult) -> Result<(), BusError> {
    let code = native(
        SYS_MESSENGER,
        op,
        args as *const MsgArgs as u64,
        result as *mut MsgResult as u64,
    );
    if code < 0 {
        Err(BusError::errno(code))
    } else {
        Ok(())
    }
}

fn invalid(error: libmessenger::Error) -> BusError {
    BusError {
        code: -EINVAL,
        message: String::from(error.message()),
    }
}

fn encode(interface: u64, method: u32, flag_bits: u16, body: &[u8]) -> Result<Vec<u8>, BusError> {
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flag_bits,
            interface_id: interface,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.to_vec(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(invalid)?;
    Ok(bytes)
}

/// The reply parcel in `buf[..len]`.
fn decode(buf: &[u8], len: u64) -> Result<Parcel, BusError> {
    let len = usize::try_from(len).map_err(|_| BusError::errno(-E2BIG))?;
    let bytes = buf.get(..len).ok_or_else(|| BusError::errno(-E2BIG))?;
    Parcel::decode(bytes).map_err(invalid)
}

/// The fabric as seen from this process.
#[derive(Debug, Default, Clone, Copy)]
pub struct Gate;

/// Linux `uname(2)`; LazyOS answers it with sysname `LazyOS`.
const SYS_UNAME: u64 = 63;
/// `struct utsname`: six 65-byte fields, sysname first.
const UTS_FIELD: usize = 65;

/// The running kernel's `sysname`, through the Linux syscall ABI.
fn sysname() -> Option<String> {
    let mut uts = [0u8; UTS_FIELD * 6];
    let code: i64;
    // SAFETY: Linux `uname` (63) via `syscall`: rdi points at a writable
    // `utsname`-sized buffer on this frame; rcx/r11 are clobbered by the
    // instruction. LazyOS and Linux both implement it.
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") SYS_UNAME as i64 => code,
            in("rdi") uts.as_mut_ptr(),
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    if code != 0 {
        return None;
    }
    let end = uts[..UTS_FIELD]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(UTS_FIELD);
    core::str::from_utf8(&uts[..end]).ok().map(String::from)
}

impl Gate {
    /// The gate, when this process runs on LazyOS; `None` elsewhere, so a
    /// host build of a LazyOS program never issues `int 0x80`.
    pub fn detect() -> Option<Gate> {
        (sysname().as_deref() == Some("LazyOS")).then_some(Gate)
    }

    /// An absolute PIT deadline `ms` from now; `0` waits forever.
    fn deadline(ms: u64) -> u64 {
        if ms == 0 {
            return 0;
        }
        let now = native(SYS_CLOCK, 0, 0, 0) as u64;
        now.saturating_add(ms.div_ceil(TICK_MS).max(1))
    }
}

impl Bus for Gate {
    fn resolve(&self, name: &str) -> Result<u64, BusError> {
        let body = registry::encode_resolve_args(&registry::ResolveArgs { name: name.into() })
            .map_err(invalid)?;
        let request = encode(registry::INTERFACE_ID, registry::METHOD_RESOLVE, 0, &body)?;
        let args = MsgArgs {
            txn_id: TARGET_SELF,
            parcel_ptr: request.as_ptr() as u64,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        messenger(op::RESOLVE, &args, &mut result)?;
        Ok(result.value)
    }

    fn call(
        &self,
        endpoint: u64,
        interface: u64,
        method: u32,
        body: &[u8],
        timeout_ms: u64,
    ) -> Result<Vec<u8>, BusError> {
        // `ALLOW_NESTED`: a script may be parked on another transaction of
        // the same channel (a topic pull) when it calls again.
        let request = encode(interface, method, flags::SYNC | flags::ALLOW_NESTED, body)?;
        let mut buf = vec![0u8; REPLY_BUFFER];
        let args = MsgArgs {
            handle: endpoint,
            parcel_ptr: request.as_ptr() as u64,
            parcel_len: request.len() as u64,
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            deadline: Self::deadline(timeout_ms),
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        messenger(op::CALL, &args, &mut result)?;
        Ok(decode(&buf, result.bytes)?.body)
    }

    fn send(
        &self,
        endpoint: u64,
        interface: u64,
        method: u32,
        body: &[u8],
    ) -> Result<(), BusError> {
        let request = encode(interface, method, flags::ONE_WAY, body)?;
        let args = MsgArgs {
            handle: endpoint,
            parcel_ptr: request.as_ptr() as u64,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        messenger(op::SEND, &args, &mut MsgResult::default())
    }

    fn names(&self) -> Result<Vec<String>, BusError> {
        let mut buf = vec![0u8; LIST_BUFFER];
        let args = MsgArgs {
            txn_id: TARGET_SELF,
            buf_ptr: buf.as_mut_ptr() as u64,
            buf_cap: buf.len() as u64,
            ..MsgArgs::default()
        };
        let mut result = MsgResult::default();
        messenger(op::LIST, &args, &mut result)?;
        let reply = decode(&buf, result.bytes)?;
        let list = registry::decode_list_reply(&reply.body).map_err(invalid)?;
        Ok(list.entries.into_iter().map(|entry| entry.name).collect())
    }
}
