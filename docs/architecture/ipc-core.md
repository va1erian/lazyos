# Messenger core: handles, channels, shared buffers

**What it is.** The kernel transport of the Messenger fabric: capability
handles, duplex channels with synchronous transactions, and zero-copy shared
buffers with fences. Spec: [messenger.md](../messenger.md) sections 4-10.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/ipc/handles.rs` | Per-process handle tables and rights (issue #64) |
| `kernel/src/ipc/channels.rs` (+ `channels/*.rs`) | Endpoints, inboxes, transactions (issue #66); types, helpers, close, recv, stats, txn timeouts in submodules |
| `kernel/src/ipc/shared.rs` (+ `shared/{types,registry,fences}.rs`) | Shared buffers, mappings, fences (issue #67) |
| `libs/messenger/src/lib.rs` | Parcel codec shared by kernel and userspace (issue #65) |

**Handles** (`handles.rs`)

| Item | Value |
|---|---|
| `HandleKind` | `Endpoint`, `Channel`, `Object`, `Buffer` |
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
- Transactions carry a global `txn_id`, a deadline and the state machine
  `Pending`/`Replied`/`TimedOut`/`Canceled`/`PeerDied`; replies match by id and
  may arrive out of order. A synchronous call on the same pair is `Deadlock`
  unless the parcel sets `ALLOW_NESTED` when it would nest (the caller already
  has a call open there) or call back (a call toward the caller's side is
  open). Concurrent calls from *different* tasks in the same direction are
  allowed: resolved names alias one endpoint, so a service's independent
  clients share its channel. One shared `MESSENGER` wait
  queue with advisory wakeups handles all blocking.
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
- A parcel's handles **move** (sender holds `TRANSFER`; its handle closes once
  queued; delivery opens a receiver-local one); buffers **share** (the message
  takes one reference). Replies refuse transfers (`UnsupportedTransfer`);
  non-buffer objects have no refcount yet.

**Shared buffers & fences** (`shared.rs`)

| Item | Value |
|---|---|
| `MAX_BUFFER_SIZE` / registry | 64 MiB / `MAX_BUFFERS = 256` |
| Per-process quota / mapping base | 8 MiB or 64 buffers / `0x0000_5000_0000_0000`, never reused |
| Flags | `READ`, `WRITE`, `SHARE_ONLY`, `EXECUTABLE` (denied), `PINNED` (recorded) |

- `create` zero-fills frames, maps the creator and opens a handle; `map` is
  idempotent per task and refuses `SHARE_ONLY` for anyone but the creator;
  `close` unmaps and drops a reference; `info` reports state.
- Lifetime is refcounted: handles + in-flight messages + mappings. `retain` /
  `retain_descriptor` take the message reference, `attach` converts it into the
  receiver's handle, `release` drops it when a queue is discarded.
- Fences: `fence_submit` is monotonic (stale -> error) and wakes `FENCES`;
  `fence_wait` parks until the sequence passes or the deadline expires. Every
  handoff is zero-copy (`Stats::handoffs`): mappings alias the same frames.

**Invariants.** `CHANNELS`/`REGISTRY` locks are released before wait-queue
notifications; waiter parks run with interrupts disabled, so no reply can slip
in between registration and the first wait.

**Status.** Working: handle rights, transactions with deadlines/cancel, handle
move and buffer share, fences. Open: reply-borne transfers, per-connection
channels on one endpoint, non-buffer object refcounts.
