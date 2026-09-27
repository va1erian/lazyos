//! Native Messenger syscall surface (issue #69).
//!
//! `docs/messenger.md` section 14 sketches a family of native syscalls
//! (`msg_call`, `msg_reply`, ...) over the kernel channel fabric. This module
//! implements the first coherent slice of that surface behind a single native
//! syscall (`messenger = 5`): one gate with an op code keeps the register ABI
//! tiny and lets the surface grow without renumbering anything.
//!
//! ```text
//!   rax = 5              rdi = op
//!   rsi -> MsgArgs       rdx -> MsgResult
//! ```
//!
//! Every pointer is validated against the calling task's address space before a
//! single byte is copied. [`access_range`] walks the active page tables (via the
//! kernel's physical map, which gives a safe alias for the actual copy) and
//! returns `-EFAULT` for an unmapped, supervisor-only, or read-only-for-write
//! range; it never dereferences an untrusted pointer directly, so a malformed
//! call cannot fault the kernel path. Not-present pages inside an `Anon`/`Heap`
//! VMA are materialized first, exactly as a real fault would: `sbrk` heaps are
//! demand-zero, and a `write` into a fresh `Vec` must not be refused merely
//! because the user atomically grew its break.
//!
//! Policy: parcel-bearing operations derive `(interface_id, method)` from the
//! [`libmessenger`] header and pass through [`crate::ipc::authorize`] before the
//! channel is touched, so a denied call never enqueues anything. Denials are
//! recorded by `authorize` (audit ring, with the reason code); this module maps
//! the verdict to `-EACCES`.
//!
//! The bootstrap channel lives in [`bootstrap`]: `kernel_main` creates one
//! endpoint pair at boot, keeps the service end kernel-side (the `messengerd`
//! stub), and the first userspace task claims the client end with
//! `OP_BOOTSTRAP`.

use alloc::vec::Vec;
use x86_64::structures::idt::PageFaultErrorCode;
use x86_64::PhysAddr;

use libmessenger::{Parcel, MAX_PARCEL_BYTES};

use crate::ipc::{channels, handles};
use crate::mem;
use crate::task;

/// Negative errno-style values returned in `rax` (x86_64 Linux numbering, so
/// userspace error handling is the same as the Linux ABI shim).
pub mod errno {
    /// The caller may not perform this operation.
    pub const EPERM: i64 = 1;
    /// No such object (unused or invalid handle/transaction).
    pub const ENOENT: i64 = 2;
    /// The argument list is too long (parcel over the wire limit).
    pub const E2BIG: i64 = 7;
    /// Operation would block (queue full, quota exhausted).
    pub const EAGAIN: i64 = 11;
    /// The kernel could not allocate (frame, handle, registry slot).
    pub const ENOMEM: i64 = 12;
    /// Policy refused the call.
    pub const EACCES: i64 = 13;
    /// The pointer is not a writable/readable user mapping.
    pub const EFAULT: i64 = 14;
    /// The bootstrap client end has already been claimed.
    pub const EBUSY: i64 = 16;
    /// A malformed argument, parcel, or op code.
    pub const EINVAL: i64 = 22;
    /// The peer endpoint is gone.
    pub const EPIPE: i64 = 32;
    /// Refused because a call would form a synchronous cycle.
    pub const EDEADLK: i64 = 35;
    /// The deadline passed before a reply arrived.
    pub const ETIMEDOUT: i64 = 110;
    /// The caller canceled the transaction.
    pub const ECANCELED: i64 = 125;
}

/// Call a method and block until the reply arrives.
pub const OP_CALL: u64 = 1;
/// Answer a pending transaction with a reply parcel.
pub const OP_REPLY: u64 = 2;
/// Send a one-way message; never blocks.
pub const OP_SEND: u64 = 3;
/// Receive the next message, blocking until one is queued.
pub const OP_RECV: u64 = 4;
/// Cancel a pending transaction.
pub const OP_CANCEL: u64 = 5;
/// Close an endpoint handle.
pub const OP_CLOSE_ENDPOINT: u64 = 6;
/// Create a fresh channel pair; both handles open in the caller.
pub const OP_CREATE_PAIR: u64 = 7;
/// Read channel counters (`handle = 0` means every live channel).
pub const OP_STATS: u64 = 8;
/// Claim the boot-time client endpoint (first userspace task only).
pub const OP_BOOTSTRAP: u64 = 9;
/// Register a call and park, but return the transaction id instead of waiting:
/// the asynchronous completion `channels` split `begin_call` for. Finish it
/// with [`OP_CALL_AWAIT`].
pub const OP_CALL_BEGIN: u64 = 10;
/// Wait for a [`OP_CALL_BEGIN`] transaction and return its reply.
pub const OP_CALL_AWAIT: u64 = 11;

/// Number of bytes in [`MsgArgs`], the first range the syscall validates.
pub const ARGS_SIZE: usize = 64;
/// Number of bytes in [`MsgResult`].
pub const RESULT_SIZE: usize = 64;

/// The syscall request block. The layout is shared with `user/src/messenger.rs`
/// and must stay in lockstep; every field is a little-endian `u64`.
///
/// Only the fields an op documents as input are read; the rest are ignored (and
/// `flags` must be zero, so an ABI addition is rejected loudly rather than
/// silently misread).
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgArgs {
    /// Endpoint handle: call, begin, send, recv, close, stats.
    pub handle: u64,
    /// Transaction id: reply, cancel, await.
    pub txn_id: u64,
    /// Request parcel bytes (call, begin, send, reply).
    pub parcel_ptr: u64,
    /// Request parcel length in bytes.
    pub parcel_len: u64,
    /// Reply or receive buffer (call, recv, await, stats).
    pub buf_ptr: u64,
    /// Capacity of `buf_ptr` in bytes.
    pub buf_cap: u64,
    /// Absolute PIT deadline in ticks (100 Hz); `0` waits forever.
    pub deadline: u64,
    /// Reserved for future flags; must be zero today.
    pub flags: u64,
}

impl MsgArgs {
    /// Decode a little-endian block of exactly [`ARGS_SIZE`] bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<MsgArgs> {
        if bytes.len() != ARGS_SIZE {
            return None;
        }
        let mut words = [0u64; 8];
        for (index, word) in words.iter_mut().enumerate() {
            let at = index * 8;
            *word = u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?);
        }
        Some(MsgArgs {
            handle: words[0],
            txn_id: words[1],
            parcel_ptr: words[2],
            parcel_len: words[3],
            buf_ptr: words[4],
            buf_cap: words[5],
            deadline: words[6],
            flags: words[7],
        })
    }

    /// The absolute deadline, or `None` for "wait forever".
    fn deadline_ticks(&self) -> Option<u64> {
        (self.deadline != 0).then_some(self.deadline)
    }

    /// Encode little-endian; the user library's mirror keeps the same order.
    pub fn to_bytes(&self) -> [u8; ARGS_SIZE] {
        let words = [
            self.handle,
            self.txn_id,
            self.parcel_ptr,
            self.parcel_len,
            self.buf_ptr,
            self.buf_cap,
            self.deadline,
            self.flags,
        ];
        encode_words(&words)
    }
}

/// The syscall response block: `status`/`value`/`aux`/`bytes` plus reserved
/// space. Shared with `user/src/messenger.rs`, little-endian `u64` fields.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgResult {
    /// `0` on success, or a negative errno (the same value as `rax`).
    pub status: i64,
    /// Primary output: new handle (create_pair/bootstrap), transaction id
    /// (call_begin/recv), or 0.
    pub value: u64,
    /// Secondary output: second handle (create_pair), sender task slot (recv).
    pub aux: u64,
    /// Bytes written to `buf_ptr` (call, call_await, recv, stats).
    pub bytes: u64,
    /// Reserved; always zero today.
    pub reserved: [u64; 4],
}

impl MsgResult {
    /// Decode a little-endian block of exactly [`RESULT_SIZE`] bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<MsgResult> {
        if bytes.len() != RESULT_SIZE {
            return None;
        }
        let mut words = [0u64; 8];
        for (index, word) in words.iter_mut().enumerate() {
            let at = index * 8;
            *word = u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?);
        }
        Some(MsgResult {
            status: words[0] as i64,
            value: words[1],
            aux: words[2],
            bytes: words[3],
            reserved: [words[4], words[5], words[6], words[7]],
        })
    }

    /// Encode little-endian; the user library's mirror keeps the same order.
    pub fn to_bytes(&self) -> [u8; RESULT_SIZE] {
        let words = [
            self.status as u64,
            self.value,
            self.aux,
            self.bytes,
            self.reserved[0],
            self.reserved[1],
            self.reserved[2],
            self.reserved[3],
        ];
        encode_words(&words)
    }
}

/// Fixed-size little-endian encoding of a word block.
fn encode_words(words: &[u64; 8]) -> [u8; 64] {
    let mut bytes = [0u8; 64];
    for (index, word) in words.iter().enumerate() {
        bytes[index * 8..index * 8 + 8].copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// Channel counters, exactly the byte order [`OP_STATS`] writes into the
/// caller's buffer. The layout is shared with `user/src/messenger.rs`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgStats {
    pub calls: u64,
    pub replies: u64,
    pub timeouts: u64,
    pub cancels: u64,
    pub drops: u64,
    pub queued: u64,
    pub queued_bytes: u64,
    pub outstanding: u64,
}

impl MsgStats {
    /// Number of bytes [`OP_STATS`] writes.
    pub const SIZE: usize = 64;

    /// Decode the block written by [`OP_STATS`].
    pub fn from_bytes(bytes: &[u8]) -> Option<MsgStats> {
        if bytes.len() != Self::SIZE {
            return None;
        }
        let word = |index: usize| -> Option<u64> {
            let at = index * 8;
            Some(u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?))
        };
        Some(MsgStats {
            calls: word(0)?,
            replies: word(1)?,
            timeouts: word(2)?,
            cancels: word(3)?,
            drops: word(4)?,
            queued: word(5)?,
            queued_bytes: word(6)?,
            outstanding: word(7)?,
        })
    }
}

impl From<channels::Stats> for MsgStats {
    fn from(stats: channels::Stats) -> MsgStats {
        MsgStats {
            calls: stats.calls,
            replies: stats.replies,
            timeouts: stats.timeouts,
            cancels: stats.cancels,
            drops: stats.drops,
            queued: stats.queued,
            queued_bytes: stats.queued_bytes,
            outstanding: stats.outstanding,
        }
    }
}

impl MsgStats {
    /// Encode for `copy_out` into the caller's buffer.
    fn to_bytes(self) -> [u8; Self::SIZE] {
        let words = [
            self.calls,
            self.replies,
            self.timeouts,
            self.cancels,
            self.drops,
            self.queued,
            self.queued_bytes,
            self.outstanding,
        ];
        encode_words(&words)
    }
}

/// The `messenger` syscall entry point: validate, execute, report.
///
/// Returns 0 on success or `-errno`; every output also travels in the caller's
/// `MsgResult` block, whose writability is established before the operation
/// runs so a successful op can always report its results.
pub fn dispatch(op: u64, args_ptr: u64, result_ptr: u64) -> u64 {
    if access_range(result_ptr, RESULT_SIZE, true).is_err() {
        // Nothing can be reported through the block; the return register still
        // carries the failure.
        return negative(errno::EFAULT);
    }
    let args = match copy_in(args_ptr, ARGS_SIZE)
        .and_then(|bytes| MsgArgs::from_bytes(&bytes).ok_or(errno::EINVAL))
    {
        Ok(args) => args,
        Err(code) => return report(result_ptr, code),
    };
    if args.flags != 0 {
        return report(result_ptr, errno::EINVAL);
    }
    match handle_op(op, &args) {
        Ok(result) => {
            if copy_out(result_ptr, &result.to_bytes()).is_err() {
                return negative(errno::EFAULT);
            }
            0
        }
        Err(code) => report(result_ptr, code),
    }
}

/// Write an error status into the (already validated) result block and return
/// the same error in `rax`.
fn report(result_ptr: u64, code: i64) -> u64 {
    let failed = MsgResult {
        status: -code,
        ..MsgResult::default()
    };
    let _ = copy_out(result_ptr, &failed.to_bytes());
    negative(code)
}

/// Two's-complement `-errno` in the syscall return register.
fn negative(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Execute one op. Errors are positive errno values; the caller negates them.
fn handle_op(op: u64, args: &MsgArgs) -> Result<MsgResult, i64> {
    match op {
        OP_CALL => op_call(args),
        OP_REPLY => op_reply(args),
        OP_SEND => op_send(args),
        OP_RECV => op_recv(args),
        OP_CANCEL => op_cancel(args),
        OP_CLOSE_ENDPOINT => op_close(args),
        OP_CREATE_PAIR => op_create_pair(args),
        OP_STATS => op_stats(args),
        OP_BOOTSTRAP => op_bootstrap(args),
        OP_CALL_BEGIN => op_call_begin(args),
        OP_CALL_AWAIT => op_call_await(args),
        _ => Err(errno::EINVAL),
    }
}

/// Read a request parcel, bounded by the wire limit before any copying.
fn read_parcel(args: &MsgArgs) -> Result<Vec<u8>, i64> {
    if args.parcel_len == 0 || args.parcel_len > MAX_PARCEL_BYTES as u64 {
        return Err(errno::E2BIG);
    }
    copy_in(args.parcel_ptr, args.parcel_len as usize)
}

/// Decode a parcel at the kernel boundary; malformed bytes are `EINVAL`.
fn decode_parcel(bytes: &[u8]) -> Result<Parcel, i64> {
    Parcel::decode(bytes).map_err(|_| errno::EINVAL)
}

/// Enforce the ACL for an outbound call or one-way send: the header is the
/// only source of `(interface_id, method)`, and `authorize` audits the verdict.
fn authorize_parcel(parcel: &Parcel) -> Result<(), i64> {
    let decision = crate::ipc::authorize(
        task::current(),
        parcel.header.interface_id,
        parcel.header.method,
        parcel.header.txn_id,
    );
    if decision.denied() {
        // `authorize` already recorded the denial and its reason code; the
        // friendly text is in the decision for `messengerctl why <txn>`.
        return Err(errno::EACCES);
    }
    Ok(())
}

/// Copy a reply into the caller's buffer, refusing one that does not fit.
/// The transaction has already completed at this point, so an over-large
/// reply is dropped; a streaming/shared-buffer path is a follow-up.
fn write_reply(args: &MsgArgs, reply: &[u8]) -> Result<MsgResult, i64> {
    if reply.len() > args.buf_cap as usize {
        return Err(errno::E2BIG);
    }
    copy_out(args.buf_ptr, reply)?;
    Ok(MsgResult {
        bytes: reply.len() as u64,
        ..MsgResult::default()
    })
}

fn op_call(args: &MsgArgs) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    authorize_parcel(&parcel)?;
    let reply = channels::call(
        args.handle,
        parcel.header.method,
        &bytes,
        args.deadline_ticks(),
    )
    .map_err(channel_errno)?;
    write_reply(args, &reply)
}

fn op_call_begin(args: &MsgArgs) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    authorize_parcel(&parcel)?;
    let txn_id = channels::begin_call(
        args.handle,
        parcel.header.method,
        &bytes,
        args.deadline_ticks(),
    )
    .map_err(channel_errno)?;
    Ok(MsgResult {
        value: txn_id,
        ..MsgResult::default()
    })
}

fn op_call_await(args: &MsgArgs) -> Result<MsgResult, i64> {
    let reply = channels::await_reply(args.txn_id).map_err(channel_errno)?;
    write_reply(args, &reply)
}

fn op_reply(args: &MsgArgs) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    channels::reply(args.txn_id, &bytes).map_err(channel_errno)?;
    Ok(MsgResult::default())
}

fn op_send(args: &MsgArgs) -> Result<MsgResult, i64> {
    let bytes = read_parcel(args)?;
    let parcel = decode_parcel(&bytes)?;
    authorize_parcel(&parcel)?;
    channels::send(args.handle, &bytes).map_err(channel_errno)?;
    Ok(MsgResult::default())
}

fn op_recv(args: &MsgArgs) -> Result<MsgResult, i64> {
    let message = channels::recv(args.handle, args.deadline_ticks()).map_err(channel_errno)?;
    if message.bytes.len() > args.buf_cap as usize {
        return Err(errno::E2BIG);
    }
    copy_out(args.buf_ptr, &message.bytes)?;
    Ok(MsgResult {
        // The kernel transaction id (0 for one-way messages), not the
        // sender's header field, is what `reply` expects.
        value: message.txn.unwrap_or(0),
        aux: message.sender as u64,
        bytes: message.bytes.len() as u64,
        ..MsgResult::default()
    })
}

fn op_cancel(args: &MsgArgs) -> Result<MsgResult, i64> {
    channels::cancel(args.txn_id).map_err(channel_errno)?;
    Ok(MsgResult::default())
}

fn op_close(args: &MsgArgs) -> Result<MsgResult, i64> {
    channels::close_endpoint(args.handle).map_err(channel_errno)?;
    Ok(MsgResult::default())
}

fn op_create_pair(_args: &MsgArgs) -> Result<MsgResult, i64> {
    let (first, second) = channels::create().map_err(channel_errno)?;
    Ok(MsgResult {
        value: first,
        aux: second,
        ..MsgResult::default()
    })
}

fn op_stats(args: &MsgArgs) -> Result<MsgResult, i64> {
    let stats = if args.handle == 0 {
        channels::stats()
    } else {
        channels::channel_stats(args.handle).map_err(channel_errno)?
    };
    let encoded = MsgStats::from(stats).to_bytes();
    if (args.buf_cap as usize) < encoded.len() {
        return Err(errno::E2BIG);
    }
    copy_out(args.buf_ptr, &encoded)?;
    Ok(MsgResult {
        bytes: encoded.len() as u64,
        ..MsgResult::default()
    })
}

fn op_bootstrap(_args: &MsgArgs) -> Result<MsgResult, i64> {
    let handle = bootstrap::claim_client()?;
    Ok(MsgResult {
        value: handle,
        ..MsgResult::default()
    })
}

/// Channel errors to errno values.
fn channel_errno(error: channels::Error) -> i64 {
    use channels::Error::*;
    match error {
        InvalidHandle | WrongKind | NoTransaction => errno::ENOENT,
        MissingRight => errno::EACCES,
        NoFreeHandle | RegistryFull => errno::ENOMEM,
        BadTask | BadParcel => errno::EINVAL,
        QueueFull | TooManyOutstanding | Quota => errno::EAGAIN,
        Deadlock => errno::EDEADLK,
        NotCaller => errno::EPERM,
        TimedOut => errno::ETIMEDOUT,
        Canceled => errno::ECANCELED,
        PeerDied => errno::EPIPE,
        BadTransfer | UnsupportedTransfer => errno::EINVAL,
    }
}

/// Handle-table errors to errno values.
fn handles_errno(error: handles::Error) -> i64 {
    use handles::Error::*;
    match error {
        NoFreeHandle => errno::ENOMEM,
        InvalidHandle => errno::ENOENT,
        MissingRight => errno::EACCES,
        BadTask => errno::EINVAL,
    }
}

// ---------------------------------------------------------------------------
// User pointer validation and copying
// ---------------------------------------------------------------------------

/// CPU-visible page-table entry bits (the same layout `mem` uses internally;
/// duplicated here because this module only ever reads foreign tables).
const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_USER: u64 = 1 << 2;
const PTE_HUGE: u64 = 1 << 7;
const PTE_ADDR: u64 = 0x000F_FFFF_FFFF_F000;

/// Highest address a user pointer may name: the canonical lower half. Anything
/// above is kernel memory and never a valid syscall buffer.
const USER_MAX: u64 = 0x0000_8000_0000_0000;

/// Read one 64-bit page-table entry at `phys`.
fn entry_at(phys: u64, index: usize) -> u64 {
    let ptr = mem::phys_to_virt(PhysAddr::new(phys)).as_ptr::<u64>();
    // Safety: `phys` names a live page table reachable through the physical
    // memory map, and `index` is masked to 9 bits by every caller.
    unsafe { ptr.add(index).read_volatile() }
}

/// Validate that `[ptr, ptr + len)` is a user range the caller may access.
///
/// `write` also requires writability (privatizing a COW page when needed).
/// Not-present pages inside an `Anon`/`Heap` VMA are materialized exactly as a
/// page fault would. Fails fast with `-EFAULT`; allocates nothing for a range
/// that is not the caller's.
fn access_range(ptr: u64, len: usize, write: bool) -> Result<(), i64> {
    if len == 0 {
        return Ok(());
    }
    let end = ptr.checked_add(len as u64).ok_or(errno::EFAULT)?;
    let table = mem::kernel_table();
    let mut va = ptr & !0xfff;
    while va < end {
        translate(table, va, write)?;
        va += 4096;
    }
    Ok(())
}

/// Translate one user virtual address in `table` to a kernel-accessible
/// physical address (page base plus the in-page offset). `-EFAULT` when the
/// range is not present/accessible to user mode.
fn translate(table: PhysAddr, va: u64, write: bool) -> Result<u64, i64> {
    if va >= USER_MAX {
        return Err(errno::EFAULT);
    }
    let l4 = entry_at(table.as_u64(), ((va >> 39) & 0x1ff) as usize);
    if l4 & PTE_PRESENT == 0 {
        return materialize(table, va, write);
    }
    if l4 & PTE_USER == 0 {
        return Err(errno::EFAULT);
    }
    let l3 = entry_at(l4 & PTE_ADDR, ((va >> 30) & 0x1ff) as usize);
    if l3 & PTE_PRESENT == 0 {
        return materialize(table, va, write);
    }
    if l3 & PTE_USER == 0 {
        return Err(errno::EFAULT);
    }
    if l3 & PTE_HUGE != 0 {
        if write && l3 & PTE_WRITABLE == 0 {
            return Err(errno::EFAULT);
        }
        return Ok((l3 & PTE_ADDR) + (va & ((1 << 30) - 1)));
    }
    let l2 = entry_at(l3 & PTE_ADDR, ((va >> 21) & 0x1ff) as usize);
    if l2 & PTE_PRESENT == 0 {
        return materialize(table, va, write);
    }
    if l2 & PTE_USER == 0 {
        return Err(errno::EFAULT);
    }
    if l2 & PTE_HUGE != 0 {
        if write && l2 & PTE_WRITABLE == 0 {
            return Err(errno::EFAULT);
        }
        return Ok((l2 & PTE_ADDR) + (va & ((1 << 21) - 1)));
    }
    let l1 = entry_at(l2 & PTE_ADDR, ((va >> 12) & 0x1ff) as usize);
    if l1 & PTE_PRESENT == 0 {
        return materialize(table, va, write);
    }
    if l1 & PTE_USER == 0 {
        return Err(errno::EFAULT);
    }
    if write && l1 & PTE_WRITABLE == 0 {
        // A shared COW page: privatize it, then re-walk for the new frame.
        if !mem::cow_fault(table, va & !0xfff) {
            return Err(errno::EFAULT);
        }
        return translate(table, va, write);
    }
    Ok((l1 & PTE_ADDR) + (va & 0xfff))
}

/// Resolve a not-present page through the demand-zero path, then translate.
///
/// Only `Anon`/`Heap` VMAs materialize (see `mem::demand_fault`); anything else
/// is `-EFAULT`. The kernel already resolved faults through this path for user
/// tasks, so the copy helper uses the same rule instead of relying on a fault.
fn materialize(table: PhysAddr, va: u64, write: bool) -> Result<u64, i64> {
    let mut error = PageFaultErrorCode::empty();
    if write {
        error.insert(PageFaultErrorCode::CAUSED_BY_WRITE);
    }
    if !mem::demand_fault(table, va, error) {
        return Err(errno::EFAULT);
    }
    translate(table, va, write)
}

/// Copy `len` bytes from a validated user range into a fresh `Vec`.
fn copy_in(ptr: u64, len: usize) -> Result<Vec<u8>, i64> {
    access_range(ptr, len, false)?;
    let table = mem::kernel_table();
    let mut out: Vec<u8> = Vec::with_capacity(len);
    let mut done = 0usize;
    while done < len {
        // `access_range` proved `ptr + len` does not overflow, so this add is
        // safe for every `done < len`.
        let va = ptr + done as u64;
        let phys = translate(table, va, false)?;
        let chunk = (4096 - (va & 0xfff) as usize).min(len - done);
        let src = mem::phys_to_virt(PhysAddr::new(phys)).as_ptr::<u8>();
        // Safety: `phys` is a live user frame mapped through the kernel's
        // physical map, and `chunk` stays inside the page it points into.
        unsafe {
            core::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(done), chunk);
        }
        done += chunk;
    }
    // Safety: the loop wrote exactly `len` initialized bytes.
    unsafe { out.set_len(len) };
    Ok(out)
}

/// Copy `bytes` into a validated, writable user range.
fn copy_out(ptr: u64, bytes: &[u8]) -> Result<(), i64> {
    access_range(ptr, bytes.len(), true)?;
    let table = mem::kernel_table();
    let mut done = 0usize;
    while done < bytes.len() {
        let va = ptr + done as u64;
        let phys = translate(table, va, true)?;
        let chunk = (4096 - (va & 0xfff) as usize).min(bytes.len() - done);
        let dst = mem::phys_to_virt(PhysAddr::new(phys)).as_mut_ptr::<u8>();
        // Safety: `phys` is a live writable user frame mapped through the
        // kernel's physical map, and `chunk` stays inside the page.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr().add(done), dst, chunk);
        }
        done += chunk;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bootstrap channel
// ---------------------------------------------------------------------------

/// The boot-time Messenger channel (issue #69).
///
/// `kernel_main` calls [`create`] once: it opens an endpoint pair in the
/// kernel's handle table, keeps the service end for the `messengerd` stub, and
/// records the client end's object id. The first userspace task to call the
/// `bootstrap` op gets a fresh handle to that client end opened in its own
/// table, so the capability transfer happens inside the kernel and the task
/// never needs to name another process's handle.
pub mod bootstrap {
    //! Boot-time channel plumbing; see the module docs for the flow.

    use spin::Mutex;

    use super::{channels, errno, handles, handles_errno, task};
    use crate::ipc::handles::HandleKind;

    /// Registry entry: the kernel-owned handles and the claim state.
    struct Channel {
        /// Kernel handle for the client end, kept so the channel survives
        /// until a task claims it.
        client: u64,
        /// Kernel handle for the service end; the stub serves on this.
        server: u64,
        /// Object id (channel id + side) `client` names; opening a handle with
        /// this id in another task's table aliases the same endpoint.
        client_object: u64,
        /// Whether a task has already taken the client end.
        claimed: bool,
    }

    static BOOTSTRAP: Mutex<Option<Channel>> = Mutex::new(None);

    /// Create the bootstrap pair. Called once from `kernel_main`, in kernel
    /// context (the handles open in the kernel task's table).
    pub fn create() -> Result<(), &'static str> {
        let (client, server) = channels::create().map_err(|error| error.message())?;
        let client_object = handles::get(client)
            .map_err(|error| error.message())?
            .object_id;
        *BOOTSTRAP.lock() = Some(Channel {
            client,
            server,
            client_object,
            claimed: false,
        });
        Ok(())
    }

    /// The kernel-held service endpoint, for the `messengerd` stub.
    pub fn service_handle() -> Option<u64> {
        BOOTSTRAP.lock().as_ref().map(|channel| channel.server)
    }

    /// Open the client end in the calling task's handle table.
    ///
    /// Exactly one userspace task may claim it; the kernel task is refused (it
    /// already owns the pair). A second claim fails with `-EBUSY` rather than
    /// silently handing out a second capability.
    pub fn claim_client() -> Result<u64, i64> {
        if task::current() == task::KERNEL_TASK {
            return Err(errno::EPERM);
        }
        let mut guard = BOOTSTRAP.lock();
        let channel = guard.as_mut().ok_or(errno::ENOENT)?;
        if channel.claimed {
            return Err(errno::EBUSY);
        }
        let handle = handles::open(
            HandleKind::Channel,
            handles::rights::ALL,
            channel.client_object,
        )
        .map_err(handles_errno)?;
        channel.claimed = true;
        Ok(handle)
    }

    /// Serve one queued request by echoing its parcel back.
    ///
    /// A synchronous request is answered with `reply`; a one-way message has
    /// no transaction, so the stub sends the same bytes back to the peer — the
    /// same echo either way. This is the `messengerd` stub for the #69 slice:
    /// enough to prove the bootstrap path end to end, not a name registry.
    /// Returns whether a request was handled.
    pub fn stub_serve() -> Result<bool, channels::Error> {
        let Some(server) = service_handle() else {
            return Ok(false);
        };
        let Some(message) = channels::try_recv(server)? else {
            return Ok(false);
        };
        match message.txn {
            Some(txn) => channels::reply(txn, &message.bytes)?,
            None => channels::send(server, &message.bytes)?,
        }
        Ok(true)
    }

    /// Drop the registry and the kernel-held endpoints. Kernel context only
    /// (tests and reboot); a running system never tears the bootstrap down.
    pub fn reset() {
        if let Some(channel) = BOOTSTRAP.lock().take() {
            handles::close(channel.client).ok();
            handles::close(channel.server).ok();
        }
    }
}

const _: () = {
    // The user library mirrors these blocks; keep the sizes pinned so an
    // accidental field addition is a compile error, not an ABI mismatch.
    assert!(core::mem::size_of::<MsgArgs>() == ARGS_SIZE);
    assert!(core::mem::size_of::<MsgResult>() == RESULT_SIZE);
    assert!(core::mem::size_of::<MsgStats>() == MsgStats::SIZE);
};
