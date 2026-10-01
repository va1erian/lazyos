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
| `kernel/src/ipc/syscalls.rs` (+ `syscalls/{abi,regops,usermem,bootstrap}.rs`) | Native op dispatch; `MsgArgs`/`MsgResult` ABI, registry ops, user-pointer copies, bootstrap in submodules |

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
  documented follow-up. The wire (interface id, method ids, TLV fields of
  requests and the `list` reply) is defined in `idl/registry.midl` and consumed
  through the generated `os_lazy_messenger_registry_v1` stubs by the kernel, the
  native client in `user/src/messenger/` and `xui-app`'s fabric panel; nothing
  is hand-mirrored. (The `REGISTER=1`..`LIST=4` numbers above are the native op
  codes, not the parcel method ids.)

**Topic policy** (`topics.rs`)

- `PUBLISH_INTERFACE`/`SUBSCRIBE_INTERFACE` are `fnv1a64` hashes of the
  `os.lazy.messenger.topics.{publish,subscribe}.v1` names; the method id per
  segment is `fnv1a32(segment)`, and `+`/`#` hash like literals so policy can
  deny a wildcard explicitly.
- `validate` enforces the segment alphabet, `MAX_SEGMENTS = 8`, literal publish
  topics, and `#` only as the last filter segment. `authorize(actor, mode, name,
  txn)` checks every segment through `ipc::authorize`; the first denial
  short-circuits. Topics live in the userspace `messengerd` broker
  ([userland.md](userland.md)); the kernel owns policy only. The broker and
  both ACL scope interfaces are defined in `idl/topics.midl`.

**Fabric stats** (`stats.rs`)

- `FABRIC_STATS_VERSION = 4`; `FabricStats::SIZE` is 22 scalar words, a
  `MAX_TASKS` (256) slot handle table, 8 ACL/audit words, then 256 four-word
  per-slot rows (version 3 had 64 of each, version 2 had 16). `snapshot()`
  returns a `Box` filled in place: the block is ~10 KiB, too much to pass by
  value through a 32 KiB kernel stack. It aggregates
  channels/endpoints/queues, message counters, buffers/fences, handles per slot,
  ACL state, and audit counters and chain head. `snapshot()` takes each subsystem
  lock in turn (never two at once); fields are little-endian `u64` in order.

**Native syscall surface** (`syscalls.rs`, syscall 5)

Ops: 1-7 `CALL`, `REPLY`, `SEND`, `RECV`, `CANCEL`, `CLOSE_ENDPOINT`,
`CREATE_PAIR`; 8-12 `STATS` (v3/v1), `BOOTSTRAP`, `CALL_BEGIN`, `CALL_AWAIT`,
`TOTALS` (v1); 13-17 `REGISTER`, `RESOLVE`, `UNREGISTER`, `LIST`,
`AUTHORIZE_TOPIC`.

- `CLOSE_ENDPOINT` takes a flags word. `CLOSE_RELEASE` (1) makes it a *release*:
  the handle is dropped and the side closes only if no other handle names it
  (teardown semantics), instead of ending the side for every holder. A receiver
  that was handed someone else's endpoint must release, not close (networking
  plan N2: `netdrv` was closing `netd`'s service endpoint).
- ABI blocks are fixed 64-byte `MsgArgs`/`MsgResult` little-endian word arrays,
  mirrored byte-for-byte in `user/src/messenger/`; sizes are compile-time
  asserted at the bottom of `syscalls/abi.rs`.
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
