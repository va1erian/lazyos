# Messenger core: handles, channels, shared buffers

**What it is.** The kernel transport of the Messenger fabric: capability
handles, duplex channels with synchronous transactions, and zero-copy shared
buffers. Spec: [messenger.md](../messenger.md) sections 4-10.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/ipc/handles.rs` | Per-process handle tables and rights (issue #64) |
| `kernel/src/ipc/channels.rs` (+ `channels/*.rs`) | Endpoints, inboxes, transactions (issue #66); the call half (`call.rs`), the indexed registry (`registry.rs`), types, helpers, close, recv, stats, txn timeouts in submodules |
| `kernel/src/ipc/endpointfd.rs`, `channels/pollstate.rs` | Pollable endpoints: a Linux descriptor `poll`/`epoll` can watch for a channel handle (issue #667, [endpoint-fd.md](endpoint-fd.md)) |
| `kernel/src/ipc/shared.rs` (+ `shared/{types,registry,dma,stats}.rs`) | Shared buffers and mappings (issue #67) |
| `libs/messenger/src/lib.rs` | Parcel codec shared by kernel and userspace (issue #65) |

**Handles** (`handles.rs`)

| Item | Value |
|---|---|
| `HandleKind` | `Endpoint`, `Channel`, `Object`, `Buffer`, `Device` (a device claim, issue #240; never transferable or duplicable) |
| Rights | `CALL`, `DUPLICATE`, `TRANSFER`, `MONITOR`, `CONTROL`, `ALL` |
| Cap | `MAX_HANDLES = 256` per process, plus a per-uid aggregate (#103) |

Tables are indexed by task slot; `open`, `duplicate` (may only *narrow* rights),
`close`, `get`, `rights`, `count`, `reset_for_task`. The kernel-only cross-task
paths are `get_for_task`/`open_for_task` (registry resolve, `messengerd` proxy);
userspace never names another task's handles.

**Channels** (`channels.rs`)

| Limit | Value |
|---|---|
| `MAX_CHANNELS` | 64 |
| `MAX_QUEUE_DEPTH` / `MAX_QUEUE_BYTES` per endpoint | 64 / 1 MiB |
| `MAX_OUTSTANDING` per channel / `MAX_PENDING_PER_SENDER` | 64 / 16 |

- One channel has two endpoints; an endpoint's `object_id` packs
  `(channel_id << 1) | side`. API: `create`, `send`, `begin_call`/`await_reply`/
  `call`, `reply`, `cancel`, `close_endpoint`, `try_recv`/`recv`, `stats`.
- **Registry and ids** (P6.3, `channels/registry.rs`): channels live in a
  fixed table of `MAX_CHANNELS` slots. A channel id is a never-repeating
  sequence number above the slot index (low 6 bits), and a transaction id
  carries its channel's slot the same way, so finding a channel or a
  transaction is one index and one comparison; a stale id never matches a
  reused slot. Endpoint waiter lists are slot bitsets (no allocation).
- **One copy per direction** (P6.3): the syscall layer copies a parcel in
  once (`read_parcel`), validates it in place (`libmessenger::ParcelView`, the
  same checks as `Parcel::decode`, which is built on it) and hands the buffer
  to `call_owned`/`begin_call_owned`/`send_owned`/`reply_owned`, which queue it
  as is; the receiver's `recv` and the caller's `await_reply` copy it out of
  that same buffer. The slice entry points (`call`, `send`, `reply`, ...) copy
  once for kernel callers. The 64-byte argument block is read onto the stack,
  `copy_in` walks each page once and `copy_out` keeps the translations of its
  range check for up to 16 pages.
- **Handoff** (P6.2): a call that wakes a callee parked in `recv`, and a reply
  that wakes its caller, hand the CPU to that task when the current one parks
  (`task::hand_off`; see [tasks.md](tasks.md)).
- Transactions carry a global `txn_id`, a deadline and the state machine
  `Pending`/`Replied`/`TimedOut`/`Canceled`/`PeerDied`; replies match by id and
  may arrive out of order. A synchronous call on the same pair is `Deadlock`
  unless the parcel sets `ALLOW_NESTED` when it would nest (the caller already
  has a call open there) or call back (a call toward the caller's side is
  open). Concurrent calls from *different* tasks in the same direction are
  allowed: resolved names alias one endpoint, so a service's independent
  clients share its channel. One shared `MESSENGER` wait
  queue with advisory wakeups handles all blocking.
- **Wait sets** (`channels/recv/waitset.rs`, native op `wait` = 19): park on
  up to 8 items at once, endpoints or the caller's own pending calls
  (`WAIT_ITEM_CALL`, `channels/recv/waitcall.rs`, ready when the transaction
  ended; issue #309, [wait-any.md](wait-any.md)), and optionally on the caller's raw input ring
  (`WAIT_RAW_INPUT`, `inputd` only), until one is ready; nothing is received,
  the op returns a ready mask and the caller takes the message with
  `try_recv`. Registration is `recv`'s (each endpoint's waiter list under the
  lock that saw it empty, the bus's doorbell under the bus lock), so a
  delivery, a peer close or an input publication wakes it through
  `MESSENGER`. `inputd` and `xuid` use it instead of short-deadline polling
  (docs/performance-plan.md P1.3, P1.4). The other doorbells are the display
  owner's key queue (`WAIT_DISPLAY_KEYS`), the `AF_INET` pump's bell
  (`WAIT_INET`, `netd`) and the child-exit bell (`WAIT_CHILD`, any task;
  `task/childbell.rs`, P7.1): ready while the caller has a finished child it
  has not reaped, rung by every exit path next to the `CHILD_EXIT`
  notification. `init` parks on its endpoint and its children's exits with
  it, so a request is served at once and an idle supervisor does not wake.
  The flags also take `WAIT_DEADLINE_NS` (bit 24: the deadline is monotonic
  nanoseconds, not ticks; `xui-app` timers and frames) and `WAIT_FD` (16) with
  one of the caller's Linux descriptors in bits 32..63, ready (`FD_READY`,
  bit 59) when `poll` would report `POLLIN` or a hang-up. Descriptors have no
  waiter list, so a watcher is flagged and `task::notify_poll` and
  `notify_poll_key` (P6.5), the wakes every pipe, pty and socket already rings
  for `poll`, also wake the flagged tasks parked in a wait set
  (`wake_fd_watchers`); the woken wait rescans. The desktop Terminal parks on
  its pty master this way.
- **Poll calls.** A call with deadline `POLL_DEADLINE` (1, the user library's
  `EXPIRED_DEADLINE`) is not dead on arrival. `begin_call` gives its
  transaction a short real deadline (`POLL_GRACE_TICKS` = 3 ticks, so a callee
  that never receives it cannot stall the caller), and when the callee receives
  the request `take_locked` records the txn in the endpoint's `serving_polls`
  and replaces the grace deadline with `POLL_SERVICE_TICKS` (100 ticks: a slow
  service turn may still reply, a wedged callee is still bounded). The parked
  caller wakes on the old deadline, finds it not due and re-parks
  (`expire_transaction` only expires a deadline that has actually passed).
  The next `recv`/`try_recv` on that endpoint runs `expire_served_polls`, which
  ends every still-`Pending` poll as `TimedOut` and wakes its caller. The
  callee therefore gets its whole service turn to reply (the reply is accepted
  and returned), while a request it deferred (a parked long-poll) is reported
  as "nothing ready" the moment it is done, not after a timeout. Before this, a
  literal expired deadline expired the call inside `await_reply` before the
  callee ran, so every reply was refused and the broker's event was never
  committed. Plain `recv` with `EXPIRED_DEADLINE` (no transaction) is unchanged.
  Tests: `ipc_channel_poll_*` in `tests/ipc_channel_suite/poll.rs`, including a
  20,000-round soak.
- **Objects** (`channels/support.rs`, `recv.rs`; core plan 2.3): a parcel's
  object list (`libmessenger::Object`, one entry per `Channel<I>` or
  `Buffer` field) is resolved into one `Vec<Resolved { kind, rights,
  object_id }>` by `resolve_objects`: the declared gate first, then each
  entry's handle must be of its slot's kind (`WrongObjectKind`) and hold
  `TRANSFER`, and a channel end may appear once (`BadTransfer`). A channel
  entry **moves** (the sender's handle closes once the message is queued;
  delivery opens a receiver-local one); a buffer entry **shares** (the
  message takes one reference with `shared::retain`; delivery converts it
  into the receiver's handle with `attach`). `deliver` installs the list in
  one loop and `rollback_delivery` undoes a partial one (installed handles
  closed, pending references released, orphaned ends closed). The kernel
  knows nothing about byte ranges: a `Buffer` field's offset and length are
  data the receiving library checks against the mapped size. `Message.objects`
  carries the installed numbers in list order. Replies refuse objects
  (`UnsupportedTransfer`); non-buffer objects have no refcount yet. Tests:
  `ipc_buffer_handle_transfer_rights`, `ipc_object_*`
  (`tests/ipc_shared_suite/{transfer,objects}.rs`, with a 100,000-round
  move/share soak).
- **Buffer syscalls** are `messenger` ops (`syscalls/bufop.rs`):
  `OP_BUFFER_CREATE = 22` (`parcel_len` is the size; `value` the handle, `aux`
  the address, `bytes` the size), `OP_BUFFER_MAP = 23` (`value` the address,
  `aux` the size) and `OP_BUFFER_CLOSE = 24` (`EBUSY` for the bound
  compositor's screen buffer). The display syscall's former ops 4 to 6 are
  gone. Userspace: `lazyos_sys::msg::{buffer_create, buffer_map,
  buffer_close}`. Tests: `display_close_buffer_*`, `tests/bufops.rs`.
- **Declared objects** (issue #516, `channels/declared.rs`): a request
  carries exactly what its `.midl` method declares. `send`/`begin_call`
  compare the parcel's object kinds with `midlc`'s generated
  `declared_objects(interface_id, method)` (same length, kinds and order)
  before resolving anything and refuse any other list with
  `UndeclaredObject` (`EINVAL`), so the sender's table is untouched and
  nothing reaches the receiver. An interface no `.midl` declares may carry
  none. Test builds exempt the suite's fixture interface (`0x0bad_cafe`).
  Tests: `transfer_gate_*` (`tests/transfer_gate_suite.rs`, with a
  20,000-request soak).

**Shared buffers** (`shared.rs`)

| Item | Value |
|---|---|
| Largest buffer / registry | `max_bytes_per_process()` / `MAX_BUFFERS = 256` |
| Per-process quota / mapping base | `limit.shared_buffer_max` bytes (3 screens, at least 16 MiB; [limits.md](limits.md)) or 64 buffers / `SHARED_WINDOW_BASE = 0x0000_7f80_0000_0000` (PML4 entry 255) |

- `create(size)` zero-fills frames, maps the creator read/write and opens a
  handle (no flags: every mapping is read/write); `map` is idempotent per task
  and refuses a driver's share-only DMA buffer (`dma_alloc(SHARE_ONLY)`,
  [devices.md](devices.md)) for anyone but the creator; `close` unmaps and drops a
  reference; `info` reports state.
- Lifetime is refcounted: handles + in-flight messages + mappings. `retain`
  takes the message reference, `attach` converts it into the receiver's
  handle, `release` drops it when a queue is discarded.
- Every handoff is zero-copy (`Stats::handoffs`): mappings alias the same
  frames. Ordering belongs to the protocol (a `Present` reply, audio's
  `Commit`, a ring's armed flag), never to the kernel.

**Invariants.** `CHANNELS`/`REGISTRY` locks are released before wait-queue
notifications; waiter parks run with interrupts disabled, so no reply can slip
in between registration and the first wait.

- A task that dies parked on `MESSENGER` (in `recv` or `await_reply`) is
  dropped from the queue by task teardown (`forget_task`, `WaitQueue::forget`).

**Performance** (WHPX, dev profile; `tools/perf/run.py`, `msgbench`): a
cross-process `Ping` round trip between two user processes is 4 to 5 µs at
the median (5.6 µs before P6) and one channel carries about 0.8 to 1 million
one-way messages per second (523k before). The in-kernel echo (`ipc_rt`, no
switch) is 0.3 to 0.5 µs; the rest of a round trip is two syscalls and two
context switches. Tests: `ipc_channel_suite::indexed` (slot reuse, a million
calls with exact quota and heap accounting, callers ended mid-call).

**Per-connection channels** (issue #483, `channels/connect.rs`,
`ipc/connect.rs`, native op `OP_CONNECT` = 20). `Connect(name)`
(`idl/registry.midl`) mints a fresh channel per client instead of aliasing the
registered endpoint: the client gets side 0, and side 1 moves to the service
in a kernel-queued `Connected` message on its registered endpoint, stamped with
the client's identity (`SenderId`) and charged to the client's queue quota.
Closing either end ends that connection only; other clients of the name are
untouched. A moved channel end that nobody receives (its message dropped when
the receiver closed, or its delivery failed) is closed by the orphan pass
(`close::close_orphans`), so its peer sees `PeerDied` instead of waiting. The
name policy is `Resolve`'s. User side: `messenger::registry::connect`. Tests:
`connect_*` (`tests/connect_suite.rs`, two clients in two tasks, orphans, a
5,000-round connect/call/close soak) and `ipc_registry_syscall_connect`.

**Status.** Working: handle rights, transactions with deadlines/cancel,
objects as fields (channel move and buffer share), per-connection channels.
Open: reply-borne objects, services serving their connections (they still
serve the shared endpoint), non-buffer object refcounts.
