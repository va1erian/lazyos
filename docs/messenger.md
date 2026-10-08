# Messenger — LazyOS RPC and pub/sub fabric

Messenger is the LazyOS system fabric: **every** inter-process communication that
is not a minimal memory/thread syscall goes through it. It provides a synchronous
**request/response** path (Binder-style transactions) and an asynchronous
**publish/subscribe** path (DBus-style topics), with one uniform security,
observability, and documentation model.

This document is the specification. The platform roadmap is in
[`platform-plan.md`](platform-plan.md); the trust rules in
[`security-model.md`](security-model.md).

---

## 1. Design stance

| Concern | Choice | Why |
|---|---|---|
| Mediation point | **Kernel transport + userspace broker** | The kernel stamps unforgeable identity, moves handles and shared buffers, enforces per-call ACLs, and is always reachable from a sandbox. `messengerd` does naming, pub/sub fanout, policy reload, and introspection so it can evolve without kernel changes. |
| Addressing | **Handles for calls, names/topics for discovery** | Calls go to capabilities (no ambient authority); names and topics are resolved by `messengerd` and can be policy-controlled. |
| Serialization | **Self-describing TLV parcels** | Version-tolerant, fuzzable, introspectable; IDL adds types on top. |
| Delivery | **Transactions (sync) + topics (async)** | One kernel queueing core serves both; synchronous is sugar over request/reply transactions. |
| Identity | **Kernel-stamped credentials** | A sender cannot forge pid/uid/gid/session/label. |
| Policy | **Default-deny allowlists** | Kernel enforces; userspace compiles and hot-reloads policy. |
| Observability | **Protocol-level introspection** | The fabric describes itself; no privileged side channel needed. |

### 1.1 Why not plain Binder or plain DBus

- **Pure Binder:** fast and secure, but a kernel object is a poor place for
  naming policy, documentation, pub/sub QoS, and hot-reloadable rules.
- **Pure DBus:** flexible naming/fanout, but a socket daemon is bypassable by a
  sandboxed app that can open the socket, and identity is self-declared unless the
  kernel provides it.
- **Hybrid (chosen):** kernel for what must be trusted and fast (identity, handles,
  copy, ACL enforcement, shared memory); userspace for what must evolve
  (registry, topics, policy, docs).

---

## 2. Concepts

| Term | Meaning |
|---|---|
| **Interface** | A named, versioned set of methods, e.g. `os.lazy.fs.reader.v1`. Identified by a 64-bit hash of the name. |
| **Object** | An instance of an interface inside a service. Referenced by a handle. |
| **Handle** | An unforgeable per-process reference to an object. Carries rights. Holding it *is* the capability. |
| **Channel** | A duplex connection between two endpoints; carries transactions and one-way messages. |
| **Parcel** | A serialized message body: TLV-encoded fields plus an array of transferred handles and shared buffers. |
| **Transaction** | `call` + matching `reply`, identified by `txn_id`, with a deadline. |
| **Topic** | A hierarchical pub/sub name, e.g. `system/events/network/up`. |
| **Subscription** | A durable or transient interest in topics, with QoS and an optional filter. |
| **Endpoint** | A service's listening handle that accepts new connections. |
| **Capability set** | The union of handles + granted names/topics + syscall rights a process holds. |

---

## 3. Names and identifiers

- **Interface name:** reverse-DNS, lowercase, `.vN` suffix. Interface id =
  `fnv1a64(name)` (stable across builds; collisions checked at codegen).
- **Method id:** a `u32` that is *stable forever* once published; append-only.
- **Object handle:** `u64` local to a process; a global `HandleId` in the kernel.
- **Service name:** well-known human name (`os.lazy.fs`, `os.lazy.notify`),
  owned by one process at a time and resolved by `messengerd`.
- **Topic name:** slash-separated segments; wildcards for subscriptions:
  `+` = one segment, `#` = zero or more trailing segments.
- **Transaction id:** `u64` scoped to a channel.
- **Labels:** a short sandbox/profile label attached to a process, used in policy.

---

## 4. Message format (wire)

All integers little-endian. Parcels are version-tagged and length-delimited so
old readers can **skip** unknown fields.

```
+--------------------------------------------------------------+
| Parcel header (fixed 48 bytes)                               |
|  u16 version (=1)          u16 flags                         |
|  u64 interface_id          u32 method                        |
|  u64 txn_id                u64 reply_to (0 if a call)        |
|  u64 deadline_ns (0 = none) u32 body_len  u16 handle_count   |
|  u16 buffer_count          u32 body_crc32c (optional)        |
+--------------------------------------------------------------+
| Credentials (kernel-written, read-only to userspace)         |
|  u32 pid u32 tid u32 uid u32 gid u32 caps u32 label_id       |
|  u64 session_id            u8[16] signature_or_zero          |
+--------------------------------------------------------------+
| Announcement block (per method: which TLVs the reply carries)|
+--------------------------------------------------------------+
| Body: sequence of TLV fields                                 |
|   u32 type (domain,id,kind)   u32 len   u8[len] value        |
+--------------------------------------------------------------+
| Handles:   u64 handle[]  (transferred/duplicated references) |
| Buffers:   { u64 handle, u64 offset, u64 len, u32 flags }[]  |
+--------------------------------------------------------------+
```

### 4.1 Flags

| Bit | Name | Meaning |
|---|---|---|
| 0 | `SYNC` | A reply is expected (transaction). |
| 1 | `ONE_WAY` | Fire-and-forget. |
| 2 | `NO_REPLY_IF_DEAD` | Do not error if the callee dies before replying. |
| 3 | `ALLOW_NESTED` | Nested transactions permitted (default: flattened). |
| 4 | `CRED_REQUIRED` | Callee insists on kernel credentials (always true for system services). |
| 5 | `TRACE` | Emit trace events for this transaction. |

### 4.2 TLV value kinds

`BOOL`, `I32`, `I64`, `U32`, `U64`, `F64`, `STRING` (UTF-8, length-prefixed),
`BYTES`, `ARRAY<T>` (element TD), `STRUCT` (nested TLVs), `HANDLE`, `BUFFER`,
`MAP<K,V>`, `OPTION<T>`, `ERROR` (see section 12). The runtime codec defines
`MAP`, but MIDL has no `Map` type yet: `midlc` rejects it and an IDL models a
map as an `ARRAY` of key/value structs ([`docs/midl.md`](midl.md#types)).

Unknown kinds and fields are skipped; required fields are declared per method in
the IDL. MIDL never puts a `HANDLE` or `BUFFER` in a body (the number would
mean nothing in the receiver's table): a request's handles and buffers travel
in the parcel's vectors and are declared by the method's `transfers (...)`
clause ([`docs/midl.md`](midl.md#transfers-handles-and-buffers)). Sizes are bounded per process (configurable quota) to prevent
amplification attacks.

---

## 5. Object model and dispatch

- A service registers an **endpoint** with `messengerd` under a name and
  advertises the interfaces it implements.
- A client resolves the name once and receives a **handle** with the requested
  rights (policy-checked). Subsequent calls go straight through the kernel to the
  service — the broker is not in the call path.
- Services dispatch by `(interface_id, method)`; unknown method → `ERR_NO_METHOD`.
- Interfaces are versioned; a service may implement v1 and v2 of an interface
  simultaneously during migration.

---

## 6. Synchronous transactions

1. Client `msg_call(handle, method, parcel, deadline)`.
2. Kernel validates the parcel, stamps credentials, checks policy
   `(sender label/uid -> interface.method)`, enqueues to the target's queue.
3. Client blocks (or registers an async completion) until:
   - a reply arrives (`msg_reply(txn, parcel)`),
   - the deadline expires (`ERR_TIMEOUT`),
   - the client cancels (`msg_cancel(txn)`),
   - the callee dies (`ERR_PEER_DIED`).
4. Replies are matched by `txn_id` on the channel; out-of-order replies are
   allowed and cheap.

**Nested calls:** if a callee calls back into the caller while a transaction is
open, the kernel detects the cycle. Default policy is `ERR_DEADLOCK` with a
friendly hint unless `ALLOW_NESTED` is set; services that need callbacks use
separate channels or async topics. Only nesting (the caller already has a call
open on the channel) and callbacks (a call toward the caller's side is open)
count as cycles: independent clients calling one service concurrently over its
shared, resolved endpoint are not refused.

**Reentrancy:** each service should declare a concurrency model in its IDL
(single-threaded mailbox, thread pool, or actor). The generated server runtime
enforces it.

**Timeouts:** every call should carry a deadline. The kernel drops/returns
expired transactions and meters queue depth per sender.

**Poll calls:** a call whose deadline is `EXPIRED_DEADLINE` (absolute tick 1,
`POLL_DEADLINE` in the kernel) is a *poll*: "answer now if you can, otherwise
tell me nothing is ready". It is not treated as already expired, because the
caller would then time out inside `msg_call` before the callee ever ran and the
callee's reply would be refused as "caller gone" (this is what made
`messengerd` log `topic reply dropped` once a second and never deliver the
event a poll found queued). Instead the transaction stays `Pending` through the
callee's **service turn**:

1. The callee receives the request (`recv`/`try_recv` on that endpoint).
2. It either replies (the poll returns the reply) or defers it, for example by
   parking a long-poll such as `NextEvent` for later.
3. When the callee returns to `recv` on the endpoint, any received poll it has
   not answered ends with `ERR_TIMEOUT`, so a poll never waits for a deferred
   answer. A reply to it is then refused like any late reply.

A callee that never receives the poll (busy, wedged) cannot stall the caller:
the poll also expires `POLL_GRACE_TICKS` (3 ticks, 30 ms) after it was sent.
Once the callee has received it that grace no longer applies, so a slow service
turn can still reply; it is replaced by `POLL_SERVICE_TICKS` (100 ticks, 1 s),
which only bounds a callee that wedges mid-request.
Nothing else about the transaction changes: cancel and peer death end it as
usual. One endpoint shared by several server threads can end a poll early (any
thread returning to `recv` ends polls another is still serving); that degrades
to the old behaviour, a spurious "nothing ready", never to a lost message,
because a service commits a delivery only after its reply is accepted.

---

## 7. Asynchronous: one-way and pub/sub

### 7.1 One-way

`msg_send(handle, method, parcel, flags=ONE_WAY)` enqueues and returns
immediately. Guarantees: at-most-once per boot; ordering preserved per sender→
receiver channel; no reply.

### 7.2 Topics

The wire contract of the broker (`publish`, `subscribe`, `unsubscribe`,
`next_event`, `ack`, `list_topics`, `stats`, `ping`, plus the kernel ACL scope
interfaces) is defined in [`idl/topics.midl`](../idl/topics.midl); `midlc`
generates the stubs every client and `messengerd` link (`os_lazy_messenger_topics_v1`
in `libs/generated`) and the reference in
[`docs/idl/os.lazy.messenger.topics.v1.md`](idl/os.lazy.messenger.topics.v1.md).
No client hand-copies interface ids, method ids or field numbers; the static-musl
`xui-app` fabric panel consumes the same generated crate. The sketch below is
the conceptual shape.

```
publish(topic, parcel, qos)          -> topic_handle? (retained only)
subscribe(topic_filter, qos, filter) -> subscription_handle
unsubscribe(subscription_handle)
```

**QoS** (chosen at subscribe time, enforced by `messengerd`):

| QoS | Delivery |
|---|---|
| `latest` | only the most recent message is kept; a new subscriber gets the retained value if present |
| `buffered(N)` | up to N messages queued per subscriber; overflow drops oldest |
| `reliable` | per-subscriber ack + bounded retry; still at-most-once after peer death |
| `conflate` | coalesce: only the latest message per publisher within a window |

**Ordering:** per publisher→topic→subscriber. **Filters:** a small predicate
language over TLV fields (`level == ERROR && unit == "netd"`), compiled by
`messengerd` — subscribers can also filter in-process for volatile data.

**Retained values:** publishers may mark a message retained; new subscribers get
it immediately (state-like topics such as `system/health/netd`).

**Backpressure:** if a subscriber's queue is bounded and full, `messengerd`
tracks drops and exposes them in stats; system topics default to `conflate` so a
slow subscriber cannot stall publishers.

**Scope:** topics have owners (session topic, system topic). Policy decides who
may publish/subscribe per segment, including wildcards.

#### 7.2.1 Implementation note (issue #92)

The first slice puts topics entirely in userspace, per the epic decision in
section 20: `messengerd` owns the broker (parsing, filters, QoS queues,
retained values and per-subscriber drop counters) and the kernel adds exactly
one thing — a per-segment policy question. The broker registers itself as
`os.lazy.messenger.topics`; every publish and subscribe first asks the kernel
(`authorize_topic`, `kernel/src/ipc/topics.rs`) to evaluate `ipc::authorize`
for each segment of the name or filter, with publish and subscribe as separate
pseudo-interfaces and `+`/`#` hashed like literal segments, so policy can deny
a wildcard explicitly. Denials land in the audit ring with the broker
request's correlation id.

Delivery is **pull-based with deferred replies**: `next_event` is a
synchronous call to the broker; when a subscription's queue is empty the
broker parks the transaction and answers it later, when a matching publish
arrives. The caller sleeps in the kernel wait queue with a real deadline, so
timeouts work and a slow subscriber cannot stall a publisher. `reliable` is
ack-gated with pull-driven retry and bounded queues (no userspace timers yet)
— best-effort after peer death, as section 7.2 allows. The `latest` /
`conflate` replacement and `buffered(N)` overflow paths count drops, which
`messengerctl topics` / `Subscription::stats` expose.

A service with other work cannot sit in `next_event`, so a subscription can
have a **doorbell** (`Bell`, docs/performance-plan.md P7.2): the subscriber
transfers one end of a fresh channel, and the broker sends one one-way
`os.lazy.messenger.topics.bell.v1` `Ready` on it when the subscription has an
event nobody is pulling, then no more until a `next_event` from its owner
finds the queue empty. The subscriber parks on the other end beside its own
endpoints (`wait_any`), and on a ring drains with expired-deadline
`next_event`s until one comes back empty, which re-arms the bell
(`central::Subscription::bell`/`take_ring`). `logd`'s central feed and
`timed`'s `confd` watch use it instead of polling every few ticks.

Headless verification: boot with `LAZYOS_MESSENGERD=1 LAZYOS_MESSENGERCTL=1`;
`messengerctl` runs a topic conformance self-test when the broker is reachable
and prints `TOPIC:FANOUT:PASS`, `TOPIC:WILDCARD:PASS`, `TOPIC:RETAINED:PASS`,
`TOPIC:DROP:PASS`, `TOPIC:QOS:PASS` and `TOPIC:UNSUB:PASS` on the serial log.
`messengerctl topics` lists known topics and `tail <filter> [count]` streams
(issue #92). Under `LAZYOS_SERVICES=1`, `messengerd` runs a boot-time
`soak=4096` request/reply self-test through its serve loop and prints
`MSGRD:SOAK:PASS cycles=... bytes=...` (heap growth across the cycles) plus
`MSGRD:TOPICS:PASS topics=... subs=...` (the broker's live counts), which
keeps the receive-buffer reuse honest (issue #169).

**Central routing (issue #169).** The platform services publish and subscribe
through this broker, not per-service brokers: `sysmond` republishes
`system/stats/*`, `clipboardd` publishes the per-session changed topic and its
audit trail, and `mimed` publishes `system/events/open/<app>`, all through the
`user::central` client; `logd` subscribes to `system/events/#` centrally next
to its supervision/health feeds. `messengerctl topics` and the fabric viewers
therefore show the real topics and subscriber counts. `init` and `healthd`
still serve their local topic brokers for the supervision and health paths,
and their consumers connect to those names unchanged.

**Supervision services (S2, issue #93).** The supervisor services run over the
topic path above (with an interim userspace router while it was landing): the
names stay the spec's — `system/events/service/<name>` carries a service's
state and detail, `system/health/<name>` and `system/health/summary` are
retained health rows, and `system/events/security/denial` is the denial signal
from the fabric audit counters. `init` (the supervisor) publishes service
state, `healthd` aggregates heartbeats and dependency health into the retained
health rows, `logd` appends hash-chained records and serves queries, and
`messengerctl services|health|log` reads them back.

---

## 8. Naming, discovery, and activation

- **Bootstrap:** the kernel hands `init` a well-known bootstrap handle. `init`
  starts `messengerd` and passes it the registry endpoint.
- **Registry:** `messengerd` owns the name table: `Register(name, endpoint,
  interfaces)`, `Resolve(name) -> handle`, `List(filter)`, plus ownership and
  lease semantics (a dead owner's names are released).
- **Activation:** `Resolve` can start a service on demand via `init` (like DBus
  activation). Services declare names in their manifest.
- **Leases:** handles and names are reference-counted; process death releases all
  handles and, optionally, marks the service unhealthy for supervision.
- **Namespaces** (`kernel/src/ipc/policy.rs`, `docs/architecture/ipc-security.md`):
  `os.lazy.*` service names belong to the platform (a `system:*` task or a
  privileged unlabelled one registers them); an app labelled `app:<id>` may
  register only `app.<id>.<name>` (`<name>` is one dot-free segment, so a name
  maps to exactly one id) and publish or subscribe topics at or under
  `app/<id>/`. A development run (`dev:<id>`) owns the same names and topics.
  Registering anything else is refused with `EACCES` and audited; resolving
  another name needs an allow rule loaded for the label.
- **Per-uid topics** (#407, `kernel/src/ipc/topics/private.rs`): topics at or
  under `user/<uid>/` belong to that uid. Another non-root task cannot
  publish or subscribe there, even with a wildcard (`user/+/...`, a leading
  `#`), whatever policy is loaded; root can. `confd` announces a user's own
  key changes on `user/<uid>/confd/changed/<path>`.
- **Interface domains** (#495): an app's registration may only advertise
  interfaces of its own domain, `<id>.<name>.v<N>` (`<name>` one or more
  segments). Interface ids are `fnv1a64` hashes, so `Register` carries
  `interface_names` beside `interfaces` (`idl/registry.midl`; each generated
  module has an `INTERFACE_NAME`): the kernel checks every name hashes to its
  id and lies in the label's domain, and refuses anything else with `EACCES`,
  audited as `UNNAMED_INTERFACE` or `FOREIGN_INTERFACE` with the offending id
  as the record's `txn_id`. Platform services (unlabelled or `system:*`) may
  leave the names empty; names they send must still be true.

---

## 9. Handles and capability rights

Rights accompany a handle at creation/duplication time:

| Right | Grants |
|---|---|
| `CALL` | invoke methods |
| `MONITOR` | receive its health/state events and stats |
| `DUPLICATE` | create more handles to it |
| `TRANSFER` | move the handle to another process (ownership moves) |
| `CONTROL` | administrative operations (revoke, reconfigure) |

- Transferring a handle requires `TRANSFER` on the sender and the sender's
  process policy must allow sending that object to that specific peer.
- Receiving a handle requires policy to accept it; the kernel rewrites the
  handle into the receiver's table.
- Handles are kernel objects; allocating them is metered per process (quota) so
  a malicious sender cannot exhaust kernel memory.
- On process exit all handles are closed and endpoints withdrawn; peers get
  `ERR_PEER_DIED` for outstanding transactions.

---

## 10. Shared buffers and zero-copy

- `msg_buffer_create(size, flags)` returns a buffer handle plus a mapping in the
  creating process. Flags: `READ`, `WRITE`, `SHARE_ONLY` (no direct map),
  `EXECUTABLE` (denied by default), `PINNED` (DMA for drivers).
- Buffers are sent as `BUFFER` fields (metadata: handle, offset, length, flags),
  which the kernel duplicates into the receiver mapping.
- Writes are synchronized with **fences** (`fence_submit`/`fence_wait`) so the
  compositor and drivers can order DMA and software access without extra copies.
- The compositor uses this to composite app surfaces with no copy; `netd` uses it
  for DMA rings; `keyd` uses `SHARE_ONLY` buffers so key material never maps into
  clients.

---

## 11. IDL, code generation, versioning

Interfaces are authored in `.midl` files; `midlc` generates Rust stubs, server
dispatch, documentation, and a conformance manifest. The real definitions live
in [`idl/`](../idl) (for example `topics.midl` and `registry.midl`, the two
interfaces the fabric viewers speak) with generated references in
[`docs/idl/`](idl); the example below is illustrative only. Any interface
published on Messenger is defined there and consumed through the generated
`messenger-generated` crate (kernel, native `user` programs and the static-musl
`xui-app` alike) rather than by hand-copied constants or encoders.

```idl
// interfaces/os.lazy.notify.midl
interface os.lazy.notify.v1 {
    /// Deliver an informational notification to the session.
    method Notify(level: Level, title: String, body: String) -> (id: U64);
    /// Publish a typed event on a topic owned by this service.
    method Publish(topic: String, event: Event) -> ();
    /// Deliver `Event`s as one-way messages on the transferred channel.
    method Watch(topic_filter: String, qos: Qos) -> ()
        transfers (events: Channel<os.lazy.notify.v1>);
    method Delivered(event: Event) -> () oneway;
    enum Level { Info, Warn, Error, Critical }
    struct Event { topic: String, payload: Bytes, at: U64 }
    enum Qos { Latest, Buffered(u32), Reliable, Conflate }
}
```

**Versioning rules**

- Adding a method, field, enum value, or optional struct member is compatible.
- Removing/renaming/retyping is not; publish a new `.vN`.
- The interface hash is checked at bind time; a service advertises which versions
  it implements.
- Generated docs (signature, arguments, errors, permissions, examples) are
  emitted next to the code and aggregated into the docs portal.

---

## 12. Errors (friendly by construction)

Every reply may carry a structured error. On the wire it is the standard
error field (id 15, generated as `messenger_generated::errors`); its encoding
and which parts are implemented today (`code`, `message`, `domain`, `hint`,
`docs`; not yet `detail`) are in [`midl.md`](midl.md), "Errors":

```
Error {
  domain: &'static str,   // "os.lazy.fs", "os.lazy.messenger", ...
  code: u32,              // stable within domain
  message: String,        // what happened, in plain language
  hint: Option<String>,   // how to fix it
  detail: Option<Parcel>, // structured context (path, pid, interface...)
  docs: Option<String>,   // documentation id, e.g. "err.fs.eacces"
}
```

Standard fabric errors (domain `os.lazy.messenger`): `ERR_DENIED`,
`ERR_NO_METHOD`, `ERR_NO_HANDLE`, `ERR_TIMEOUT`, `ERR_PEER_DIED`,
`ERR_DEADLOCK`, `ERR_QUOTA`, `ERR_PARSE`, `ERR_VERSION`. POSIX-ish domains map
`EACCES`, `ENOENT`, `EBUSY` for compatibility.

Example denial:

```
ERR_DENIED
  you cannot call os.lazy.fs.reader.Read because app "com.example.editor"
  was not granted the "files.read" permission
  hint: approve it in Settings > Apps > Editor > Permissions, or run:
        lazyosctl grant com.example.editor files.read /home/user/docs
  docs: err.messenger.denied
```

`explain <code>` and `doctor` render these system-wide; the GUI shows the same
text with a button that opens the docs page.

---

## 13. Observability

All introspection is *itself* Messenger interfaces, subject to policy:

| Interface | Purpose |
|---|---|
| `os.lazy.messenger.registry.v1` | ListServices, GetService, ListInterfaces, GetInterface (methods, types, docs), WhoOwns |
| `os.lazy.messenger.topics.v1` | ListTopics, ListSubscribers, GetRetained, Tail(filter) |
| `os.lazy.messenger.stats.v1` | per-service and global call counts, error rates, p50/p99 latency, queue depth, drops, handle counts |
| `os.lazy.messenger.trace.v1` | subscribe to transaction start/finish with ids and durations |
| `os.lazy.health.v1` | service heartbeats, dependency health, last-error, restart count (also a retained topic) |
| `os.lazy.audit.v1` | read the audit stream (policy-gated) |

The first kernel-side slice of this surface is live: the native `messenger`
syscall's `stats` op serves a versioned `FabricStats` snapshot (ABI v2)
aggregating services/endpoints/channels, message counters, shared buffers and
fences, handles, ACL/audit state and per-slot usage; the `totals` op keeps the
compact v1 counters. The `messengerctl` tool (`/system/bin/messengerctl`, on the OS
image) renders the snapshot as a table (boot the demo with
`LAZYOS_MESSENGERCTL=1`).

`messengerctl` commands: `services`, `iface <name>`, `topics`, `subs`,
`tail <topic>`, `trace <service>`, `stat`, `policy check`, `why <txn>`, `graph`.

Tracing correlates: `txn_id` propagates to nested calls; events carry
pid/uid/interface/method/duration/outcome. Sampling is configurable so tracing
stays cheap under load.

---

## 14. Kernel implementation sketch

- **Handle table** per process: `Vec<HandleEntry>` indexed by local handle, with
  rights, refcount, and type tag (object/channel/endpoint/buffer/subscription).
- **Channels:** pairs of bounded queues; each entry is a `ParcelRef` (buffer
  handle + offset) to avoid copying; small parcels are inlined.
- **Queues:** per-channel ring buffers with per-sender metering; block/wake via
  the scheduler's wait queues (no spinning).
- **Topics** live in `messengerd`; the kernel only moves messages.
- **Credentials:** stamped on entry from the task struct; never writable by
  userspace.
- **ACL hook:** called once per call with
  `(sender creds/label, target object's owner, interface.method, rights)`; the
  compiled policy is a compact trie/bitmap loaded by `messengerd` via a privileged
  interface. Decisions are O(1)-ish and audited on denial.
- **Limits:** max parcel size, max handles per message, max queue depth per
  process, max outstanding transactions; all metered and reported.
- **Syscalls** (native ABI), each taking a small op structure validated on entry:
  `msg_endpoint`, `msg_connect`, `msg_register`, `msg_resolve`, `msg_call`,
  `msg_reply`, `msg_send`, `msg_cancel`, `msg_publish`, `msg_subscribe`,
  `msg_recv`, `msg_wait` (`wait_any`: park on up to 8 items, each an endpoint or one of the caller's pending calls (`WAIT_ITEM_CALL`, ready when the transaction ended), plus doorbells; return a ready mask without receiving or awaiting; a subscription joins through its doorbell endpoint or its outstanding `NextEvent` call; `user::messenger::wait`, design note [docs/architecture/wait-any.md](architecture/wait-any.md)), `msg_buffer_create`, `msg_fence`, `msg_stats`, `msg_acl_load`.
- **Service supervision calls:** ahead of the Messenger family, the S2
  supervisor adds four small native calls — `spawn` (start a program as the
  caller's child), `wait` (reap a child exit against a deadline), `clock` (PIT
  ticks, for backoff and polls) and `args` (the service's manifest argument
  string). They are mechanism only: restart policy, dependencies and health
  live in `init`. Since filesystem F3 (#507) programs live in `/system/bin` on
  the ext2 OS volume, `spawnv` (syscall 31) passes an `argv` vector and `envp`,
  and syscall 9 reads both back (`user/src/sys/args.rs`).

---

## 15. Userspace library

- `libmessenger` (Rust): blocking API (`Connection::call`, `Server::serve`) and an
  async API (futures/`async`/`await`) over the same kernel calls; a `select` loop
  for multiplexing.
- Generated stubs from IDL provide typed clients/servers and doc comments.
- A `service!` macro wires dispatch, concurrency model, health heartbeats,
  metrics, and graceful shutdown from the interface manifest.
- A testing harness spins a mock channel so interface tests run without a kernel.

An ordinary event loop (`epoll`, `mio`, tokio, calloop) includes Messenger
through the `ENDPOINT_FD` op (issue #667): it turns an endpoint handle into a
Linux descriptor that reads ready while a message is queued or the peer
closed, and hangs up once the handle is gone (`lazyos_sys::msg::Pollable`
implements `AsRawFd`). The readiness, edge, lifetime and security rules are
[architecture/endpoint-fd.md](architecture/endpoint-fd.md).

---

## 16. Network transport (future)

Remote messaging frames the same parcels over TCP, authenticated with mTLS or
`keyd`-issued tokens; `messengerd` federates topics across hosts (publish/subscribe
bridging with per-topic policy). Identity becomes `(host, uid, label)` with the
same ACL model. No protocol change at the parcel level.

---

## 17. Performance targets

| Metric | Target (single CPU, QEMU/KVM) |
|---|---|
| Small sync call, same host, no contention | < 10 us median round trip |
| Throughput, small messages, one channel | > 200k msg/s |
| Shared-buffer 1080p surface handoff | 0 copies; < 50 us metadata |
| Topic fanout, 100 subscribers, small msg | < 1 ms p99 |
| Trace overhead when disabled | < 1% |
| Handle create/destroy | > 1M/s |

The first two rows are measured by `/system/bin/msgbench` (two user
processes, `PERF:msg_rt` and `PERF:msg_tput`) through `tools/perf/run.py`:
4 to 5 us median and 0.8 to 1 million one-way messages per second under WHPX
on the dev profile (docs/performance-plan.md P6). No CI workflow runs the
benchmark or gates on it yet; the other rows have no benchmark.

---

## 18. Testing

- **Conformance suite:** every interface's generated test (round-trip, unknown
  field skip, version negotiation, error paths).
- **Fuzzing:** parcel/TLV parser, handle transfer, filter language; run in CI.
- **Fault injection:** dropped replies, peer death mid-transaction, deadline
  races, queue overflow, policy change mid-flight.
- **Policy tests:** allow/deny matrices per interface/topic, including wildcard
  topics and cross-user attempts.
- **Perf suite:** the metrics above, tracked over time.

---

## 19. Worked examples

**Echo (hello, fabric).** Service registers `os.lazy.echo.v1`; client calls
`Echo("hi")`; reply `("hi", txn_id)`. The first CI artifact proving S1.

**Clipboard.** `os.lazy.clipboard.v1`: `Offer(mime_types) -> token`,
`Request(token, mime) -> (handle: Buffer)`; a selection is a handle plus a
scoped topic `session/<id>/clipboard/changed`. Cross-app transfers go through
policy (an app must be granted `clipboard.write`, and pastes are logged).

**Files (shell integration).** `os.lazy.fs.reader.v1`: `Open(path) -> (file)`,
`Read(file, buffer) -> n`, `Stat(path) -> Stat`; the file manager publishes
`session/<id>/selection` with file references; the editor resolves and opens
them, using `open-with` from `os.lazy.mime.v1`.

**System events.** `system/events/{boot,service,network,security}` topics with
`latest`/`conflate` QoS; the desktop tray subscribes; `auditd` taps
`system/events/security` to file denials.

---

## 20. Open questions

1. **Topics: userspace broker first — decided.** Start with `messengerd` owning
   topics while the kernel only moves messages (simplest correct thing). Then
   **instrument** fanout (latency, queue depth, drops, broker CPU) and
   **assert the threat model** for the broker (single point of failure, its own
   privilege, policy-bypass attempts) before considering a kernel fast-path for
   hot topics. Revisit only with data.
2. Parcel schema evolution beyond append-only (renames) — do we need adapters?
3. Handle revocation semantics for long-lived handles (generation counters?).
4. Whether `ALLOW_NESTED` should ever be on by default for session services.
5. Filter language scope: full expressions or a fixed field/operator set?
