//! The `messenger` syscall ABI: op numbers, flags and the fixed-size blocks.
//! Mirrors `kernel/src/ipc/syscalls/abi.rs` and
//! `kernel/src/ipc/channels/recv/waitset.rs` (checked by
//! `tests/kernel_tables.rs`; the kernel pins the block sizes).

/// Native op numbers, the kernel's `ipc::syscalls::OP_*`.
pub mod op {
    /// Call a method and block until the reply arrives.
    pub const CALL: u64 = 1;
    /// Answer a pending transaction with a reply parcel.
    pub const REPLY: u64 = 2;
    /// Send a one-way message; never blocks.
    pub const SEND: u64 = 3;
    /// Receive the next message, blocking until one is queued.
    pub const RECV: u64 = 4;
    /// Cancel a pending transaction.
    pub const CANCEL: u64 = 5;
    /// Close an endpoint handle.
    pub const CLOSE_ENDPOINT: u64 = 6;
    /// `flags` of [`CLOSE_ENDPOINT`]: release this task's handle, and close
    /// the side only if no other handle names it.
    pub const CLOSE_RELEASE: u64 = 1;
    /// Create a fresh channel pair; both handles open in this task.
    pub const CREATE_PAIR: u64 = 7;
    /// Read channel counters (`handle = 0` means every live channel), or the
    /// versioned fabric snapshot into a larger buffer.
    pub const STATS: u64 = 8;
    /// Claim the boot-time client endpoint (first userspace task only).
    pub const BOOTSTRAP: u64 = 9;
    /// Register a call and park, returning the transaction id.
    pub const CALL_BEGIN: u64 = 10;
    /// Wait for a `CALL_BEGIN` transaction and return its reply.
    pub const CALL_AWAIT: u64 = 11;
    /// Global message totals in the compact 64-byte [`super::Stats`] shape.
    pub const TOTALS: u64 = 12;
    /// Publish a service name in the kernel registry (issue #89).
    pub const REGISTER: u64 = 13;
    /// Resolve a service name to a new handle.
    pub const RESOLVE: u64 = 14;
    /// Withdraw a service name.
    pub const UNREGISTER: u64 = 15;
    /// Snapshot the name table into the caller's buffer.
    pub const LIST: u64 = 16;
    /// Ask the kernel policy engine about every segment of a topic (issue #92).
    pub const AUTHORIZE_TOPIC: u64 = 17;
    /// Replace every rule of one label (`CAP_IPC_CONTROL`).
    pub const ACL_LOAD: u64 = 18;
    /// Park until one of several endpoints (or a doorbell) is ready.
    pub const WAIT: u64 = 19;
    /// Open a private connection to a registered name (issue #483).
    pub const CONNECT: u64 = 20;
    /// A Linux descriptor `poll`/`epoll` can watch for an endpoint handle
    /// (issue #667, docs/architecture/endpoint-fd.md); `value` is the
    /// descriptor.
    pub const ENDPOINT_FD: u64 = 21;
    /// `flags` of [`ENDPOINT_FD`]: open the descriptor close-on-exec.
    pub const ENDPOINT_FD_CLOEXEC: u64 = 1;
    /// Create a shared buffer of `parcel_len` bytes, mapped into the caller
    /// (`docs/messenger-core-plan.md` 3.4): `value` is the handle, `aux`
    /// the address, `bytes` the size.
    pub const BUFFER_CREATE: u64 = 22;
    /// Map the buffer `handle` names: `value` is the address, `aux` the
    /// size.
    pub const BUFFER_MAP: u64 = 23;
    /// Unmap and drop the caller's reference to the buffer `handle` names.
    pub const BUFFER_CLOSE: u64 = 24;
    /// `flags` of [`RECV`]: also write the sender's kernel-stamped
    /// [`super::SenderId`] to `parcel_ptr`.
    pub const RECV_SENDER_ID: u64 = 1;
}

/// `MsgArgs::txn_id` for registry ops: act on the calling task. Another
/// slot is the `messengerd` proxy path (`CAP_IPC_CONTROL`).
pub const REGISTRY_TARGET_SELF: u64 = u64::MAX;

/// Absolute PIT tick used to ask for an immediate answer (issue #91).
///
/// The native surface has no "peek" op, but a deadline at or below the
/// current tick is already expired when `recv` parks: the timer gate sweeps
/// it on the spot and reports `-ETIMEDOUT` instead of blocking. Tick 1 is in
/// the past after the first 10 ms of boot; before the first tick it waits at
/// most one tick.
///
/// As the deadline of a *call* it means "poll": the kernel keeps the
/// transaction open while the callee serves it, so the callee's reply is
/// accepted, and ends it with `-ETIMEDOUT` when the callee returns to `recv`
/// without answering (`kernel::ipc::channels::POLL_DEADLINE`).
pub const EXPIRED_DEADLINE: u64 = 1;

/// Most items (endpoints and pending calls) one wait may name.
pub const WAIT_MAX_ENDPOINTS: usize = 8;
/// Marks a word of the wait set as a call's transaction id.
pub const WAIT_ITEM_CALL: u64 = 1 << 63;
/// Doorbell: the caller's raw input ring has records (`inputd` only).
pub const WAIT_RAW_INPUT: u64 = 1;
/// Doorbell: a key reached the display input queue (the display owner only).
pub const WAIT_DISPLAY_KEYS: u64 = 2;
/// Doorbell: an application acted on an `AF_INET` socket (the attached
/// `netd` only).
pub const WAIT_INET: u64 = 4;
/// Doorbell: a child of the caller finished and waits to be reaped (any
/// task); it stays ready until every finished child is reaped.
pub const WAIT_CHILD: u64 = 8;
/// Doorbell: the Linux descriptor in bits 32..63 of the flags is readable
/// (or hung up).
pub const WAIT_FD: u64 = 16;
/// Where [`WAIT_FD`]'s descriptor sits in the flags.
pub const WAIT_FD_SHIFT: u32 = 32;
/// Flag: the deadline is absolute monotonic nanoseconds
/// ([`crate::time::monotonic_ns`]), not PIT ticks.
pub const WAIT_DEADLINE_NS: u64 = 1 << 24;
/// Ready-mask bit: the raw input ring holds records.
pub const RAW_INPUT_READY: u64 = 1 << 63;
/// Ready-mask bit: the display input queue has events.
pub const DISPLAY_INPUT_READY: u64 = 1 << 62;
/// Ready-mask bit: the `AF_INET` pump has work.
pub const INET_READY: u64 = 1 << 61;
/// Ready-mask bit: a child waits to be reaped.
pub const CHILD_READY: u64 = 1 << 60;
/// Ready-mask bit: the [`WAIT_FD`] descriptor is readable or hung up.
pub const FD_READY: u64 = 1 << 59;

/// The syscall request block; the kernel's `MsgArgs`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgArgs {
    /// Endpoint handle: call, begin, send, recv, cancel, close, stats; the
    /// buffer handle of [`op::BUFFER_MAP`] and [`op::BUFFER_CLOSE`].
    pub handle: u64,
    /// Transaction id (reply, cancel, await), or the registry target task.
    pub txn_id: u64,
    /// Request parcel bytes (call, begin, send, reply), the wait set, or the
    /// sender-id block of a [`op::RECV_SENDER_ID`] receive.
    pub parcel_ptr: u64,
    /// Length of `parcel_ptr` (bytes, or words for the wait set); the size
    /// of [`op::BUFFER_CREATE`].
    pub parcel_len: u64,
    /// Reply or receive buffer (call, recv, await, stats, list).
    pub buf_ptr: u64,
    /// Capacity of `buf_ptr` in bytes.
    pub buf_cap: u64,
    /// Absolute PIT deadline (or monotonic ns with [`WAIT_DEADLINE_NS`]);
    /// 0 waits forever.
    pub deadline: u64,
    /// Op flags ([`op::CLOSE_RELEASE`], [`op::RECV_SENDER_ID`], the wait
    /// doorbells); zero otherwise.
    pub flags: u64,
}

/// The most objects one message carries (`libmessenger::MAX_OBJECTS`).
pub const MAX_OBJECTS: usize = 8;

/// The syscall response block; the kernel's `MsgResult`
/// (`docs/messenger.md` section 10, `docs/messenger-core-plan.md` 3.3).
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct MsgResult {
    /// 0 on success, or a negative errno.
    pub status: i64,
    /// New handle (create_pair, resolve, bootstrap, buffer_create),
    /// transaction id (begin, recv), the wait's ready mask, or a buffer's
    /// address (buffer_map).
    pub value: u64,
    /// Second handle (create_pair), sender task slot (recv), a buffer's
    /// address (buffer_create) or size (buffer_map).
    pub aux: u64,
    /// Bytes written to `buf_ptr`, or a new buffer's size (buffer_create).
    pub bytes: u64,
    /// `recv`: how many objects the message carried (its parcel's object
    /// list length), each installed in this task's table; zero otherwise.
    pub object_count: u64,
    /// `recv`: the installed handle numbers of the first `object_count`
    /// objects, in object-list order (a channel end to receive on, a buffer
    /// to `buffer_map`).
    pub objects: [u64; MAX_OBJECTS],
}

impl MsgResult {
    /// The installed objects a `recv` delivered.
    pub fn delivered(&self) -> &[u64] {
        let count = (self.object_count as usize).min(MAX_OBJECTS);
        &self.objects[..count]
    }
}

/// Channel counters in the compact 64-byte shape; the kernel's `MsgStats`.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Stats {
    pub calls: u64,
    pub replies: u64,
    pub timeouts: u64,
    pub cancels: u64,
    pub drops: u64,
    pub queued: u64,
    pub queued_bytes: u64,
    pub outstanding: u64,
}

impl Stats {
    /// Bytes the compact shape occupies.
    pub const SIZE: usize = 64;
}

/// A message sender's identity as the kernel stamped it at queue time; the
/// kernel's `channels::SenderId` (five little-endian `u64` words: uid, gid,
/// label id, session, capability bits). Authorize from this, never from the
/// sender's task slot, which may have been reused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SenderId {
    pub uid: u32,
    pub gid: u32,
    pub label_id: u32,
    pub session: u64,
    pub caps: u32,
}

impl SenderId {
    /// Bytes of the block a [`op::RECV_SENDER_ID`] receive asks for.
    pub const SIZE: usize = 40;

    /// Decode the kernel's block; a word that does not fit its field means
    /// the block is not one, and is refused.
    pub fn from_bytes(bytes: &[u8; Self::SIZE]) -> Option<SenderId> {
        let word = |index: usize| {
            let mut le = [0u8; 8];
            le.copy_from_slice(&bytes[index * 8..index * 8 + 8]);
            u64::from_le_bytes(le)
        };
        Some(SenderId {
            uid: u32::try_from(word(0)).ok()?,
            gid: u32::try_from(word(1)).ok()?,
            label_id: u32::try_from(word(2)).ok()?,
            session: word(3),
            caps: u32::try_from(word(4)).ok()?,
        })
    }

    /// The same identity as a credential block.
    pub const fn cred(self) -> crate::cred::Cred {
        crate::cred::Cred::new(self.uid, self.gid, self.caps, self.label_id, self.session)
    }
}

/// The kernel pins the blocks at 64 and 104 bytes (`ARGS_SIZE`, `RESULT_SIZE`).
const _: () = assert!(core::mem::size_of::<MsgArgs>() == 64);
const _: () = assert!(core::mem::size_of::<MsgResult>() == 104);
const _: () = assert!(core::mem::size_of::<Stats>() == Stats::SIZE);

#[cfg(test)]
mod tests {
    use super::*;

    fn block(words: [u64; 5]) -> [u8; SenderId::SIZE] {
        let mut bytes = [0u8; SenderId::SIZE];
        for (chunk, word) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(words) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn a_sender_block_decodes_in_kernel_order() {
        let id = SenderId::from_bytes(&block([1000, 100, 7, 42, 1 << 6])).unwrap();
        assert_eq!(
            id,
            SenderId {
                uid: 1000,
                gid: 100,
                label_id: 7,
                session: 42,
                caps: 1 << 6,
            }
        );
        assert_eq!(id.cred(), crate::cred::Cred::new(1000, 100, 1 << 6, 7, 42));
    }

    #[test]
    fn a_sender_block_with_oversized_ids_is_refused() {
        assert!(SenderId::from_bytes(&block([1 << 32, 0, 0, 0, 0])).is_none());
        assert!(SenderId::from_bytes(&block([0, u64::MAX, 0, 0, 0])).is_none());
        assert!(SenderId::from_bytes(&block([0, 0, 1 << 40, 0, 0])).is_none());
        assert!(SenderId::from_bytes(&block([0, 0, 0, 0, 1 << 33])).is_none());
    }
}
