# Messenger fabric: registry, topics, stats, syscalls

**What it is.** Discoverability and observability over the core: the kernel name
registry, the topic-policy hook, the stats snapshot, and the native `messenger`
syscall surface (including the bootstrap channel).

**Key files**

| Path | Role |
|---|---|
| `kernel/src/ipc/registry.rs` | Name table with owners, leases, pruning (issue #89) |
| `kernel/src/ipc/topics.rs` | Per-segment publish/subscribe ACL hook (issue #92) |
| `kernel/src/ipc/stats.rs` | `FabricStats` v3 snapshot (issue #70, #204) |
| `kernel/src/ipc/syscalls.rs` | Native op dispatch, `MsgArgs`/`MsgResult`, bootstrap |

**Name registry** (`registry.rs`)

| Limits | Methods |
|---|---|
| `MAX_ENTRIES=64`, `MAX_NAME_BYTES=128`, `MAX_INTERFACES=16` | `REGISTER=1`, `RESOLVE=2`, `UNREGISTER=3`, `LIST=4` |

- An entry records the owner slot (from the caller, never a user field), the
  endpoint kind/rights/object id, interface ids and an optional lease.
  Access-time pruning drops dead-owner and expired-lease entries; `release_owner`
  is the teardown hook.
- `resolve` performs the capability transfer: it opens the endpoint in the
  *target task's* table (`REGISTRY_TARGET_SELF` = caller, or the `messengerd`
  proxy path gated by `CAP_IPC_CONTROL`). Re-registering a name by the same owner
  replaces it; another owner gets `NameTaken`. All resolvers alias one endpoint,
  so closing it is peer death for everyone; per-connection channels are the
  documented follow-up. TLV ids (`NAME=1`, `ENDPOINT=4`, ...) are mirrored in
  `user/src/messenger/`.

**Topic policy** (`topics.rs`)

- `PUBLISH_INTERFACE`/`SUBSCRIBE_INTERFACE` are `fnv1a64` hashes of the
  `os.lazy.messenger.topics.{publish,subscribe}.v1` names; the method id per
  segment is `fnv1a32(segment)`, and `+`/`#` hash like literals so policy can
  deny a wildcard explicitly.
- `validate` enforces the segment alphabet, `MAX_SEGMENTS = 8`, literal publish
  topics, and `#` only as the last filter segment. `authorize(actor, mode, name,
  txn)` checks every segment through `ipc::authorize`; the first denial
  short-circuits. Topics live in the userspace `messengerd` broker
  ([userland.md](userland.md)); the kernel owns policy only.

**Fabric stats** (`stats.rs`)

- `FABRIC_STATS_VERSION = 3`; `FabricStats::SIZE` is 22 scalar words, a
  `MAX_TASKS` (64) slot handle table, 8 ACL/audit words, then 64 four-word
  per-slot rows (version 2 had 16 of each). It aggregates
  channels/endpoints/queues, message counters, buffers/fences, handles per slot,
  ACL state, and audit counters and chain head. `snapshot()` takes each subsystem
  lock in turn (never two at once); fields are little-endian `u64` in order.

**Native syscall surface** (`syscalls.rs`, syscall 5)

Ops: 1-7 `CALL`, `REPLY`, `SEND`, `RECV`, `CANCEL`, `CLOSE_ENDPOINT`,
`CREATE_PAIR`; 8-12 `STATS` (v3/v1), `BOOTSTRAP`, `CALL_BEGIN`, `CALL_AWAIT`,
`TOTALS` (v1); 13-17 `REGISTER`, `RESOLVE`, `UNREGISTER`, `LIST`,
`AUTHORIZE_TOPIC`.

- ABI blocks are fixed 64-byte `MsgArgs`/`MsgResult` little-endian word arrays,
  mirrored byte-for-byte in `user/src/messenger/`; sizes are compile-time
  asserted at the bottom of `syscalls.rs`.
- Before touching a channel, `op_call`/`op_send` derive `(interface_id, method)`
  from the parcel header and pass `ipc::authorize`; the handle path uses
  `access_range`/`copy_in`/`copy_out`, which validate ranges against page tables
  (materializing demand-zero pages) instead of trusting pointers. `OP_STATS`
  serves v3 for a big buffer, else the compact 64-byte v1 `MsgStats`.
- `kernel_main` calls `bootstrap::create()` and publishes
  `os.lazy.messenger.registry`; the first userspace task to call `OP_BOOTSTRAP`
  claims the client end (`EBUSY` on a second claim; the kernel task is refused).
  `service_handle()` is the kernel-held service end, `stub_serve()` the echo
  stub used before `messengerd` claims it.

**Status.** Working: register/resolve/list, leases and pruning, topic ACL,
v1/v3 stats, bootstrap. Open: userspace audit stream (`os.lazy.audit.v1`) and
slot generation counters.
