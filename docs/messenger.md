# Messenger — LazyOS RPC and pub/sub fabric

Messenger is the LazyOS system fabric: **every** inter-process communication
that is not a minimal memory/thread syscall goes through it, as a
**request/response** transaction on a channel or a **publish/subscribe**
topic, under one security, observability and documentation model.

This document is the specification. Sections 1 to 5 are the whole model;
the rest is detail under them. See also [`midl.md`](midl.md) (the interface
language), [`architecture/ipc-core.md`](architecture/ipc-core.md) (the kernel
side), [`security-model.md`](security-model.md) (the trust rules) and
[`messenger-core-plan.md`](messenger-core-plan.md) (the plan behind
sections 1 to 4 and 10).

---

## 1. Six concepts

1. **Channel.** A pair of endpoints. What one end sends, the other receives,
   in order. A channel is the only thing two processes share by default. You
   get one by connecting to a registered name (`Resolve`, `Connect`), by
   `create_pair`, or by receiving an end in a message.

2. **Message.** A parcel sent on a channel: a fixed header, a TLV body the
   kernel copies and never reads, and the kernel objects the body refers to.

3. **Field.** A body is fields. Most are values (integers, strings, bytes,
   structs, arrays, options). Two kinds are *objects*:
   * `Channel<I>`: one end of a channel. It **moves**. The sender's handle is
     closed when the message is queued; the receiver gets a fresh handle and
     sends `I`'s one-way methods on it.
   * `Buffer`: shared memory with a byte range. It is **shared**. The sender
     keeps its handle and mapping; the receiver gets a new handle to the same
     pages and maps it when it wants.

   An object field holds no handle number. It holds an index into the
   message's *object list*, which the kernel resolves, checks and installs in
   the receiver's table. A method's parameters say which objects it carries;
   the kernel refuses a message whose object list does not match them. A
   reply carries no objects.

4. **Call and send.** A `call` is a message that expects a reply on the same
   channel (a transaction with a deadline, section 6). A `send` is one-way
   (section 7). Both are the same message shape.

5. **Identity.** The kernel stamps every message with the sender's uid, gid,
   label, session and capabilities (section 9). A server authorizes on the
   stamp, never on anything in the body.

6. **Name and topic.** `messengerd` maps well-known names to endpoints
   (section 8) and fans out topics to subscribers (section 7.2). Both are
   ordinary services speaking ordinary messages (`idl/registry.midl`,
   `idl/topics.midl`).

Vocabulary: an **interface** is a named, versioned set of methods
(`os.lazy.confd.v1`, identified by the FNV-1a 64-bit hash of its name) whose
**method ids** are `u32`s stable forever once published; a **handle** is a
`u64` local to a process that carries rights, and holding it *is* the
capability; a **transaction** is a `call` plus its `reply`, matched by
`txn_id` on the channel; a **topic** is a slash-separated name (`+` matches
one segment, a trailing `#` the rest); a **label** is the sandbox name
policy keys on (`app:<id>`, `system:*`, `dev:<id>`).

---

## 2. What a shared buffer is

A buffer is pages. `buffer_create(size)` gives the creator a handle and a
read/write mapping; `buffer_map(handle)` maps a received handle and reports
its size; `buffer_close(handle)` unmaps and drops the reference
(`lazyos_sys::msg::{buffer_create, buffer_map, buffer_close}`, `msg` ops 22
to 24; section 14). The pages live while any handle or in-flight message
references them. There are no flags, no fences and no kinds of buffer.

A buffer travels as a `Buffer` field: the index of its object-list entry, an
offset and a length (`libmessenger::Buffer { handle, offset, len }`,
`Buffer::whole(handle, len)`). The kernel takes one reference for the message
and installs a new handle in the receiver's table. It knows nothing about
the byte range: the receiving library checks `offset + len` against the size
`buffer_map` reports (`Buffer::fits`).

Bulk data (frames, samples, packets) goes through a buffer and a *ring*
layout declared in MIDL ([`midl.md`](midl.md), "Rings"); messages only move
positions or ring a doorbell. Ordering is the protocol's business (`Present`
is a call whose reply says the frame was consumed, audio orders with
`Commit`, the NIC rings use the armed flag in their header), never the
kernel's. A driver's `dma_alloc(SHARE_ONLY)` buffer is the one buffer mapped
only into its creator ([architecture/devices.md](architecture/devices.md)).

---

## 3. Rules the kernel enforces

Rights accompany a handle at creation or duplication time (`duplicate` may
only narrow them):

| Right | Grants |
|---|---|
| `CALL` | invoke methods |
| `MONITOR` | receive its health/state events and stats |
| `DUPLICATE` | create more handles to it |
| `TRANSFER` | send it in a message (a channel end moves; a buffer is shared) |
| `CONTROL` | administrative operations (revoke, reconfigure) |

On every message with objects (`kernel/src/ipc/channels/support.rs`,
`resolve_objects`; `declared.rs`):

* An object needs the `TRANSFER` right on the sender's handle.
* A channel end appears at most once in a message (one handle, one move). A
  buffer may be shared twice; each entry takes its own reference. An entry
  whose handle is not of its slot's kind is refused (`WrongObjectKind`).
* The object list must equal the method's declared kinds, in order and in
  number (`messenger_generated::declared_objects(interface, method)`), or the
  request is refused with `EINVAL` before anything moves. Object fields have
  fixed cardinality (never inside `Option<T>` or `Array<T>`), so the declared
  list is one static slice per method and the gate is one comparison; the
  generated decoder repeats it as defence in depth. An interface no `.midl`
  declares may carry none.
* Inside the body, an object field's index is its position in the declared
  order. The generated decoder refuses any other value
  (`libmessenger::Error::BadObjectIndex`), so two fields can never name the
  same installed handle, an index can never be out of range, and every
  installed object is claimed by exactly one field.
* A reply with objects is refused.
* On process exit every handle closes and every endpoint is withdrawn; peers
  see `PeerDied` (`ERR_PEER_DIED`) for outstanding transactions.

The kernel knows nothing about byte ranges: a `Buffer` field's offset and
length are data (section 2). Handles are metered per process (section 14).

---

## 4. Message format (wire)

All integers little-endian. Parcels are version-tagged (version 2) and
length-delimited so old readers can **skip** unknown fields. The codec is
`libs/messenger` (`Parcel`, `Header`, `Object`, `Buffer`; the kernel
validates in place with `ParcelView`).

```
+--------------------------------------------------------------+
| Parcel header (fixed 48 bytes)                               |
|  u16 version (=2)          u16 flags      u32 object_count   |
|  u64 interface_id          u32 method     u32 body_len       |
|  u64 txn_id                u64 reply_to (0 if a call)        |
|  u64 deadline_ns (0 = none)                                  |
+--------------------------------------------------------------+
| Body: sequence of TLV fields                                 |
|   u32 type (kind | id << 8)   u32 len   u8[len] value        |
|   HANDLE (len 4):  u32 index into the object list            |
|   BUFFER (len 20): u32 index, u64 offset, u64 len            |
+--------------------------------------------------------------+
| Object list: object_count x 16 bytes                         |
|   u32 kind (1 = Channel, 2 = Buffer)   u32 reserved (0)      |
|   u64 handle (the sender's number; the receiver ignores it)  |
+--------------------------------------------------------------+
```

At most `MAX_OBJECTS = 8` objects travel in one parcel. The sender's
credentials are not in the parcel: the kernel stamps them on the queued
message and `recv` reports them beside it (section 10). Sizes are bounded
per process (configurable quota) to prevent amplification attacks.

### 4.1 Flags

| Bit | Name | Meaning |
|---|---|---|
| 0 | `SYNC` | A reply is expected (transaction). |
| 1 | `ONE_WAY` | Fire-and-forget. |
| 2 | reserved | Unused. |
| 3 | `ALLOW_NESTED` | A call that would close a wait cycle is allowed instead of refused with `Deadlock`. |
| 4 | reserved | Unused (the kernel always stamps credentials, section 9). |
| 5 | reserved | Unused (tracing is not per-message, section 13). |

Reserved bits are carried and ignored; the kernel acts on none of them.

### 4.2 TLV value kinds

`BOOL`, `I32`, `I64`, `U32`, `U64`, `F64`, `STRING` (UTF-8), `BYTES`,
`ARRAY<T>`, `STRUCT`, `MAP<K,V>` (codec only: MIDL has no `Map`, model one as
an `ARRAY` of key/value structs), `OPTION<T>`, `ERROR` (section 12), `HANDLE`
(a `Channel<I>` parameter) and `BUFFER` (a `Buffer` or `Ring<...>` parameter,
with its byte range); the tags are in [`midl.md`](midl.md#wire-encoding).
Unknown kinds and fields are skipped; required fields are declared per
method in the IDL.

---

## 5. Cheat sheet for agents

| I want to | Do |
|---|---|
| Hand a service a window buffer | `.midl`: `method AttachBuffer(surface: U64, pixels: Buffer) -> ();`. Client: `encode_attach_buffer_args(&AttachBufferArgs { surface, pixels: Buffer::whole(handle, len) })` gives `(body, objects)` for the call. Server: `let args = message.decode(wire::decode_attach_buffer_args)?;` then `let (va, size) = sys::buffer_map(args.pixels.handle)?;` and check `args.pixels.fits(size)`. |
| Give a service a way to send me events | `method CreateSurface(..., events: Channel<os.lazy.display.v1>) -> (surface: U64);`: `create_pair()`, pass one end, keep the other and `recv` on it. The server's decoded `events: u64` is its handle to send `oneway` methods on. |
| Send bytes larger than a parcel | A buffer plus a `ring` declaration. Never a `Bytes` field over 64 KiB. |
| Put an object inside a struct | Allowed; it is a field with a fixed place (nested structs too). |
| Put an object inside an option, an array, a reply or a topic | Refused by `midlc`: the kernel gate needs a fixed object count per method. Use a separate method, or a `U32` count beside fixed fields. |
| Check what a request carried | The generated decoder refuses a request whose objects do not match (`Message::decode`); nothing to do by hand. An object no decoder claimed is closed when the `Message` drops. |
| See why a call was refused | `LAZYOS_LABEL_TRACE=1` for policy (`LABEL:DENY` lines); `EINVAL` on `send`/`call` with objects is the declared gate or a wrong kind (`kernel/src/ipc/channels/declared.rs` counts the refusals). |
| Add a third object kind | Do not, unless it is a new kernel object. Then: one `ObjectKind` variant, one wire tag, one `match` arm in `resolve_objects` and one in `deliver`. |

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

**Nested calls:** a call that would nest (the caller already has a call open
on the channel) or call back (a call toward the caller's side is open) is
`ERR_DEADLOCK` unless `ALLOW_NESTED` is set; services that need callbacks
use a `Channel<I>` parameter or a topic. Independent clients calling one
service concurrently over its shared, resolved endpoint are not refused.

**Reentrancy:** a service serves one message at a time per endpoint
(`Server::serve`); threads share an endpoint only when their work is independent.

**Timeouts:** every call should carry a deadline. The kernel drops/returns
expired transactions and meters queue depth per sender.

**Poll calls:** a call whose deadline is `EXPIRED_DEADLINE` (absolute tick 1,
`POLL_DEADLINE` in the kernel) is a *poll*: "answer now if you can, otherwise
tell me nothing is ready". The transaction stays `Pending` through the
callee's **service turn**: the callee receives the request and either replies
(the poll returns the reply) or defers it (parks a long-poll such as
`NextEvent`); when it returns to `recv` on that endpoint, every received poll
it has not answered ends with `ERR_TIMEOUT`, and a late reply is refused. A
callee that never receives the poll cannot stall the caller: the poll expires
`POLL_GRACE_TICKS` (3 ticks) after it was sent, replaced once received by
`POLL_SERVICE_TICKS` (100 ticks), which only bounds a callee that wedges
mid-request. Cancel and peer death end it as usual. The mechanism and its
history are in [`architecture/ipc-core.md`](architecture/ipc-core.md),
"Poll calls".

---

## 7. Asynchronous: one-way and pub/sub

### 7.1 One-way

`msg_send(handle, method, parcel, flags=ONE_WAY)` enqueues and returns
immediately. Guarantees: at-most-once per boot; ordering preserved per sender→
receiver channel; no reply.

### 7.2 Topics

The broker's wire contract (`publish`, `subscribe`, `unsubscribe`,
`next_event`, `ack`, `list_topics`, `stats`, `ping`, plus the kernel ACL scope
interfaces) is [`idl/topics.midl`](../idl/topics.midl), generated as
`os_lazy_messenger_topics_v1` and documented in
[`docs/idl/os.lazy.messenger.topics.v1.md`](idl/os.lazy.messenger.topics.v1.md).
The sketch below is the conceptual shape.

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

**Backpressure and scope:** a full bounded queue drops (counted in stats);
system topics default to `conflate` so a slow subscriber cannot stall
publishers. Topics have owners (session, system, user); policy decides who
may publish/subscribe per segment, wildcards included (section 8).

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
passes one end of a fresh channel (a `Channel<os.lazy.messenger.topics.bell.v1>`
parameter), and the broker sends one one-way `Ready` on it when the
subscription has an event nobody is pulling, then no more until a
`next_event` from its owner finds the queue empty. The subscriber parks on
the other end beside its own endpoints (`wait_any`), and on a ring drains
with expired-deadline `next_event`s until one comes back empty, which re-arms
the bell (`central::Subscription::bell`/`take_ring`). `logd`'s central feed
and `timed`'s `confd` watch use it instead of polling every few ticks.

Headless verification: boot with `LAZYOS_MESSENGERD=1 LAZYOS_MESSENGERCTL=1`;
`messengerctl` runs a topic conformance self-test (`TOPIC:FANOUT|WILDCARD|
RETAINED|DROP|QOS|UNSUB:PASS` on the serial log), then `MESSAGE:OBJECTS:PASS`
once it has checked that a received message owns its objects until a decoder
claims them and closes the rest when it drops
(`user/src/bin/messengerctl/object_tests.rs`). `messengerctl topics` lists
known topics and `tail <filter> [count]` streams. Under `LAZYOS_SERVICES=1`,
`messengerd` runs a boot-time `soak=4096` request/reply self-test
(`MSGRD:SOAK:PASS`, `MSGRD:TOPICS:PASS`; issue #169).

**Central routing (issue #169).** The platform services publish and subscribe
through this broker, not per-service brokers (`sysmond`'s `system/stats/*`,
`clipboardd`'s per-session changed topic, `mimed`'s
`system/events/open/<app>`, all through `user::central`; `logd` subscribes to
`system/events/#`). `init` and `healthd` still serve their local topic
brokers for the supervision and health paths.

**Supervision services (S2, issue #93).** `system/events/service/<name>`
carries a service's state, `system/health/<name>` and `system/health/summary`
are retained health rows, and `system/events/security/denial` is the denial
signal from the fabric audit counters. `init` publishes service state,
`healthd` aggregates heartbeats and dependency health, `logd` appends
hash-chained records and serves queries, and `messengerctl
services|health|log` reads them back.

---

## 8. Naming, discovery, and activation

- **Bootstrap:** the kernel hands `init` a well-known bootstrap handle. `init`
  starts `messengerd` and passes it the registry endpoint.
- **Registry:** `messengerd` owns the name table: `Register(name, endpoint,
  interfaces)`, `Resolve(name) -> handle`, `List(filter)`, plus ownership and
  lease semantics (a dead owner's names are released). A client resolves a
  name once and gets a handle with the requested rights (policy-checked);
  calls then go straight through the kernel to the service, the broker is
  not in the call path. Services dispatch by `(interface_id, method)`;
  unknown method is `ERR_NO_METHOD`. A service may implement v1 and v2 of
  an interface at once during a migration.
- **Activation and leases:** `Resolve` can start a service on demand via
  `init`; handles and names are reference-counted, and process death
  releases them all (and may mark the service unhealthy for supervision).
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
## 9. Identity

The kernel stamps every queued message with the sender's `uid`, `gid`,
label id, session and capability bits from its own credential registry
(`kernel/src/ipc/credentials.rs`); no syscall or parcel field can write them
back. A receiver reads the stamp beside the message (a `RECV_SENDER_ID`
receive fills a 40-byte `SenderId` block, section 10; `user::messenger::Message::caller`)
and authorizes on it, never on the sender's task slot (which may have been
reused) and never on anything in the body. The same stamp is what the ACL
hook (section 14) and the audit ring read, so there is exactly one source of
truth. The label is what policy keys on ([`security-model.md`](security-model.md),
[`architecture/ipc-security.md`](architecture/ipc-security.md)).

---

## 10. Receive ABI

Every `messenger` op (section 14) takes a 64-byte `MsgArgs` block and writes
a 104-byte `MsgResult` block (`lazyos_sys::msg::{MsgArgs, MsgResult}`,
mirrored from `kernel/src/ipc/syscalls/abi.rs`; the sizes are pinned on both
sides). `MsgResult` is 13 words:

| Word | Field | On `recv` |
|---|---|---|
| 0 | `status` | 0, or a negative errno |
| 1 | `value` | the transaction id (0 for a one-way message) |
| 2 | `aux` | the sender's task slot (addressing only; authorize on the stamp) |
| 3 | `bytes` | parcel bytes written to `buf_ptr` |
| 4 | `object_count` | how many objects the message carried |
| 5..12 | `objects[8]` | the handles the delivery installed in the receiver's table, in object-list order |

The parcel arrives as it was sent, object list included, so the receiver
knows each installed handle's kind from its own entry. `RECV_SENDER_ID`
(`flags` bit 0) also writes the sender's `SenderId` (five `u64` words: uid,
gid, label id, session, caps) to `parcel_ptr`.

The user library (`user::messenger::Message`) keeps the installed objects
until a generated decoder claims them (`Message::decode(wire::decode_<m>_args)`;
a failed decode leaves them with the message). Whatever is unclaimed when
the `Message` drops is released (a channel end, so a shared service end
stays open for its other holders) or closed (a buffer): a server that
ignores an object cannot leak it, and no default arm closes anything by
hand (`MESSAGE:OBJECTS:PASS`, section 7.2.1).

---

## 11. IDL, code generation, versioning

Interfaces are authored in `.midl` files under [`idl/`](../idl); `midlc`
generates the Rust codecs (`messenger-generated`, used by the kernel, native
`user` programs and the static-musl `xui-app` alike), the references in
[`docs/idl/`](idl) and the manifest. Nothing hand-copies an id or an
encoder. The example below is illustrative only.

```idl
// interfaces/os.lazy.notify.midl
interface os.lazy.notify.v1 {
    /// Deliver an informational notification to the session.
    method Notify(level: Level, title: String, body: String) -> (id: U64);
    /// Publish a typed event on a topic owned by this service.
    method Publish(topic: String, event: Event) -> ();
    /// Deliver `Event`s as one-way messages on the carried channel.
    method Watch(topic_filter: String, qos: Qos, events: Channel<os.lazy.notify.v1>) -> ();
    method Delivered(event: Event) -> () oneway;
    enum Level { Info, Warn, Error, Critical }
    struct Event { topic: String, payload: Bytes, at: U64 }
    enum Qos { Latest, Buffered(u32), Reliable, Conflate }
}
```

**Versioning:** append-only within a `.vN` (methods, fields, enum values);
removing, renaming or retyping publishes a new `.vN`. The interface hash is
checked at bind time; a service advertises which versions it implements. The
rules, the object types and the generated names are in [`midl.md`](midl.md).

---

## 12. Errors (friendly by construction)

Every reply may carry a structured error instead of its declared fields: the
standard error field (id 15, generated as `messenger_generated::errors`)
holds a `code` (stable within a `domain`), a `message` in plain language, a
`hint` (how to fix it) and a `docs` id; the encoding, and that `detail` is
not implemented yet, are in [`midl.md`](midl.md), "Errors".

The fabric reports failures as errno values on the syscall (`EINVAL` for a
refused object list, `EPERM` for policy, `ETIMEDOUT`, `EPIPE` for a dead peer,
`EDEADLK` for a wait cycle). Named `ERR_*` codes, hint text, an `explain` or
`doctor` tool and the "docs" id are not implemented; a service that wants
friendly text puts it in the standard error field itself. To see why a call
was refused, use `LAZYOS_LABEL_TRACE=1` (section 5).

---

## 13. Observability

All introspection is *itself* Messenger interfaces, subject to policy:

| Interface | Purpose |
|---|---|
| `os.lazy.messenger.registry.v1` | ListServices, GetService, ListInterfaces, GetInterface, WhoOwns |
| `os.lazy.messenger.topics.v1` | ListTopics, ListSubscribers, GetRetained, Tail(filter) |

Both are in `idl/`. The kernel-side slice is live: the `messenger` syscall's
`STATS` op serves a versioned `FabricStats` snapshot (services, endpoints,
channels, message counters, shared buffers, handles, ACL/audit state,
per-slot usage) and `TOTALS` the compact counters. `messengerctl`
(`LAZYOS_MESSENGERCTL=1`) renders them: `list`, `services`, `health`,
`sessions`, `apps`, `topics`, `tail <topic>`, `stats`, `stats-json`,
`tasks-json`, `log`, `keys`, `clipboard`. Per-service `stats`, `trace`,
`health` and `audit` interfaces, per-transaction tracing and the
`iface`/`trace`/`why`/`graph`/`policy check` subcommands do not exist and are
not planned; heartbeats live in `healthd`. The declared-gate refusal count
(`channels/declared.rs::refused()`) is a kernel diagnostic read by kernel
tests only; it is not exported.

---|---|
| `os.lazy.messenger.registry.v1` | ListServices, GetService, ListInterfaces, GetInterface (methods, types, docs), WhoOwns |
| `os.lazy.messenger.topics.v1` | ListTopics, ListSubscribers, GetRetained, Tail(filter) |
| `os.lazy.messenger.stats.v1` | per-service and global call counts, error rates, p50/p99 latency, queue depth, drops, handle counts |
| `os.lazy.messenger.trace.v1` | subscribe to transaction start/finish with ids and durations |
| `os.lazy.health.v1` | service heartbeats, dependency health, last-error, restart count (also a retained topic) |
| `os.lazy.audit.v1` | read the audit stream (policy-gated) |

The kernel-side slice is live: the `messenger` syscall's `STATS` op serves a
versioned `FabricStats` snapshot (services, endpoints, channels, message
counters, shared buffers, handles, ACL/audit state, per-slot usage) and
`TOTALS` the compact counters; `messengerctl` (`LAZYOS_MESSENGERCTL=1`)
renders them (`services`, `iface <name>`, `topics`, `subs`, `tail <topic>`,
`trace <service>`, `stat`, `policy check`, `why <txn>`, `graph`). Tracing
correlates on `txn_id`; events carry pid/uid/interface/method/duration/outcome,
sampled so tracing stays cheap under load.

---

## 14. Kernel implementation sketch

The detail is [`architecture/ipc-core.md`](architecture/ipc-core.md); this
is the shape.

- **Handle table** per process (`kernel/src/ipc/handles.rs`): rights,
  refcount and a kind tag (endpoint, channel, object, buffer, device).
- **Channels** (`kernel/src/ipc/channels/`): pairs of bounded queues; each
  entry holds the parcel bytes as they arrived plus the resolved object list
  (`Resolved { kind, rights, object_id }` per entry). Queueing a message runs
  the declared gate (`declared.rs`), resolves the list against the sender's
  table (`support.rs`, `resolve_objects`), takes a reference per buffer and
  closes the sender's moved channel handles; delivery installs the list in
  the receiver's table with one loop and one rollback (`recv.rs`, `deliver`).
- **Queues:** per-channel ring buffers with per-sender metering; block/wake
  via the scheduler's wait queues (no spinning).
- **Topics** live in `messengerd`; the kernel only moves messages and
  answers the per-segment policy question (section 7.2.1).
- **Credentials:** stamped on entry from the task struct; never writable by
  userspace (section 9).
- **ACL hook:** called once per call with `(sender creds/label, target
  object's owner, interface.method, rights)`; the compiled policy is a compact
  trie/bitmap loaded by `messengerd` via a privileged interface. Decisions are
  O(1)-ish and audited on denial.
- **Limits:** max parcel size, max objects per message (8), max handles per
  process (256), max queue depth per process, max outstanding transactions;
  all metered and reported.
- **Syscalls:** one native `messenger` syscall with an op number
  (`lazyos_sys::msg::op`), each op taking the blocks of section 10:
  `CALL` 1, `REPLY` 2, `SEND` 3, `RECV` 4, `CANCEL` 5, `CLOSE_ENDPOINT` 6,
  `CREATE_PAIR` 7, `STATS` 8, `BOOTSTRAP` 9, `CALL_BEGIN` 10, `CALL_AWAIT` 11,
  `TOTALS` 12, `REGISTER` 13, `RESOLVE` 14, `UNREGISTER` 15, `LIST` 16,
  `AUTHORIZE_TOPIC` 17, `ACL_LOAD` 18, `WAIT` 19 (`wait_any`: park on up to
  8 endpoints, pending calls and doorbells and return a ready mask;
  [architecture/wait-any.md](architecture/wait-any.md)), `CONNECT` 20
  (section 8), `ENDPOINT_FD` 21 (section 15), `BUFFER_CREATE` 22,
  `BUFFER_MAP` 23 and `BUFFER_CLOSE` 24 (section 2).
- **Service supervision calls:** beside the Messenger family, `init` has
  `spawn`/`spawnv`, `wait`, `clock` and `args` (mechanism only: restart
  policy, dependencies and health live in `init`;
  [`architecture/processes.md`](architecture/processes.md)).

---

---

## 15. Userspace library

- `user::messenger` (native programs): `Endpoint` (`call`, `send`, `reply`,
  `recv`, `create_pair`), `Server::serve` (one handler per received
  `Message`), `wait` (`wait_any`), `registry` (`resolve`, `connect`,
  `register`) and `topics_client`; the static-musl `xui-app` has the same
  shape over `lazyos-sys`. Both encode with `libmessenger` and the generated
  `messenger-generated` codecs (section 11); nothing hand-rolls a wire.
- A received `Message` owns its objects until a decoder claims them
  (section 10).

An ordinary event loop (`epoll`, `mio`, tokio, calloop) includes Messenger
through the `ENDPOINT_FD` op (issue #667): it turns an endpoint handle into a
Linux descriptor that reads ready while a message is queued or the peer
closed, and hangs up once the handle is gone (`lazyos_sys::msg::Pollable`
implements `AsRawFd`). The readiness, edge, lifetime and security rules are
[architecture/endpoint-fd.md](architecture/endpoint-fd.md).

---

## 16. Network transport (not planned)

Nothing here is implemented, and no issue tracks it. Remote messaging would frame the same parcels over TLS (`keyd`-issued
identities, `(host, uid, label)` under the same ACL model) and let
`messengerd` bridge topics across hosts; no change at the parcel level.

---

## 17. Performance targets

| Metric | Target (single CPU, QEMU/KVM) |
|---|---|
| Small sync call, same host, no contention | < 10 us median round trip |
| Throughput, small messages, one channel | > 200k msg/s |
| Shared-buffer 1080p surface handoff | 0 copies; < 50 us metadata |
| Topic fanout, 100 subscribers, small msg | < 1 ms p99 |
| Handle create/destroy | > 1M/s |

The first two rows are measured by `/system/bin/msgbench` through
`tools/perf/run.py` (`PERF:msg_rt`, `PERF:msg_tput`): 4 to 5 us median and
0.8 to 1 million one-way messages per second under WHPX on the dev profile
(docs/performance-plan.md P6). The other rows have no benchmark yet.

---

## 18. Testing

- **Conformance:** the MIDL corpus (`idl/conformance/`, [`midl.md`](midl.md))
  and `cargo test -p messenger-generated`.
- **Fuzzing:** the parcel v2 decoder (`libs/messenger`, `fuzz::run`, seeded
  in CI) and the object list; the filter language.
- **Kernel suites** (`LAZYOS_TEST_FILTER=ipc python tools/test/run.py`):
  transactions, deadlines, peer death, queue overflow, the object rules of
  section 3 with soaks, the declared gate.
- **Policy tests:** allow/deny matrices per interface/topic, wildcard topics,
  cross-user attempts (`tools/accounts/run.py`).
- **Perf:** section 17, tracked in `docs/perf/history.md`.

---

## 19. Worked examples

**Echo (hello, fabric).** `os.lazy.echo.v1`: `Echo("hi")` replies `"hi"`;
`msgbench` times it (section 17).

**Clipboard.** `os.lazy.clipboard.v1` (`idl/clipboard.midl`): `Offer` a
selection, `Request(token, mime)` its bytes (a `Bytes` reply, since a reply
cannot carry a buffer); a selection is a token plus the retained topic
`session/<id>/clipboard/changed`. Cross-app pastes go through policy and
are announced on `system/events/clipboard/paste`.

**Files.** The file manager publishes `session/<id>/selection`; the editor
opens the files through `os.lazy.mimed.v1`. Bulk reads go through the VFS.

**System events.** `system/events/{boot,service,network,security}` topics
(`latest`/`conflate`); the desktop tray subscribes, `logd` files denials.

---

## 20. Open questions

1. **Topics: userspace broker first — decided.** `messengerd` owns topics
   while the kernel only moves messages; instrument fanout and assert the
   broker's threat model before considering a kernel fast path for hot
   topics. Revisit only with data.
2. Parcel schema evolution beyond append-only (renames) — do we need adapters?
3. Handle revocation semantics for long-lived handles (generation counters?).
4. Whether `ALLOW_NESTED` should ever be on by default for session services.
5. Filter language scope: full expressions or a fixed field/operator set?
