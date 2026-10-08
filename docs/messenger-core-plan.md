# Messenger core: handles and buffers are fields

Status: plan (accepted). No compatibility with parcel v1 is kept. Owner: kernel IPC. Related: [`messenger.md`](messenger.md),
[`midl.md`](midl.md), [`architecture/ipc-core.md`](architecture/ipc-core.md).

## 1. Why

Messenger works, but its transfer story is harder to explain than it needs to
be. A request today carries two parallel vectors beside its body: `handles`
*moves* kernel objects (the sender loses its number) and `buffers` *shares*
memory (the sender keeps its mapping, the receiver gets a descriptor). A
shared buffer may travel on either path, so the kernel special-cases
`HandleKind::Buffer` in queue, deliver and rollback. MIDL declares them in a
`transfers (...)` clause apart from the parameters, narrows both vectors to
one slot each, and the receive syscall reports only the first of each
(`first_handle`, `first_buffer`). The wire, the kernel and the docs still
describe N of each.

Around that sit features nothing uses. Each was a forward bet a later design
made unnecessary:

| Feature | Intended for | What happened |
|---|---|---|
| Fences (`fence_submit`, `fence_wait`) | GPU-style ordering of a producer's writes against the compositor and drivers (`messenger.md` 10, the ebook's "client presents a frame" example) | `Present` is a call whose reply is the fence; audio orders with `Commit`; net rings use the armed flag in the ring header. `networking-plan.md`: "Fences stay unused." Two stats viewers print the counters. |
| `SHARE_ONLY` | `keyd` key tables no client can map; a driver client handing a DMA buffer "without mapping it" | Never passed. `input-plan.md` found the semantics backwards for the key-state page (only the *creator* may map). `keyd` is served by never sending the buffer. |
| `PINNED` | DMA | Superseded by `dma_alloc` / `create_from_frames`. Only recorded. |
| `EXECUTABLE` | A W^X story the kernel does not have | Always refused. |
| `BufferDesc.flags`, `BufferDesc.offset` | Descriptor metadata | Flags read by nobody; offset always 0 from every sender. |
| TLV kinds `HANDLE`, `BUFFER` | Objects as body fields | Defined by the codec, refused by MIDL "because the number would mean nothing in the receiver's table". |
| Buffer-by-move (a buffer handle in `handles`) | | Kernel tests only. |
| `msg_buffer_create` (as documented) | | Does not exist: buffers are `display_create_buffer`, `display_map_buffer`, `display_close_buffer` in the *display* syscall family. |

The goal of this plan is a Messenger whose whole model fits on one page
(section 2), implemented with less code than today, where a handle or a
buffer is a field like any other.

## 2. The model (the documentation)

This section is written to become the opening of `docs/messenger.md` and the
text an agent reads before touching IPC. Everything else in that document is
detail under these six concepts.

### 2.1 Six concepts

1. **Channel.** A pair of endpoints. What one end sends, the other receives, in
   order. A channel is the only thing two processes share by default. You get
   one by connecting to a registered name, by `create_pair`, or by receiving
   an end in a message.

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
   channel (a transaction with a deadline). A `send` is one-way. Both are the
   same message shape.

5. **Identity.** The kernel stamps every message with the sender's uid, gid,
   label, session and capabilities. A server authorizes on the stamp, never
   on anything in the body.

6. **Name and topic.** `messengerd` maps well-known names to endpoints and
   fans out topics to subscribers. Both are ordinary services speaking
   ordinary messages (`idl/registry.midl`, `idl/topics.midl`).

### 2.2 What a shared buffer is

A buffer is pages. `buffer_create(size)` gives the creator a handle and a
mapping; `buffer_map(handle)` maps a received handle and reports its size;
`buffer_close(handle)` unmaps and drops the reference. The pages live while
any handle or in-flight message references them. There are no flags, no
fences and no kinds of buffer. Bulk data goes through a buffer and a *ring*
layout declared in MIDL (`docs/midl.md`, "Rings"); messages only move
positions or ring a doorbell.

### 2.3 Rules the kernel enforces

* An object needs the `TRANSFER` right on the sender's handle.
* A channel end appears at most once in a message (one handle, one move).
* The object list must equal the method's declared kinds, in order and in
  number. Object fields have fixed cardinality (never inside `Option<T>` or
  `Array<T>`), so the declared list is one static slice per method and the
  gate is one comparison; the generated decoder repeats it as defence in
  depth.
* Inside the body, an object field's index is its position in the declared
  order. The generated decoder refuses any other value, so two fields can
  never name the same installed handle, an index can never be out of range,
  and every installed object is claimed by exactly one field.
* A reply with objects is refused.
* On process exit every handle closes; peers see `PeerDied`.

The kernel knows nothing about byte ranges: a `Buffer` field's offset and
length are data, and the receiving library checks them against the size
`buffer_map` reports.

### 2.4 Cheat sheet for agents

| I want to | Do |
|---|---|
| Hand a service a window buffer | `.midl`: `method Attach(surface: U64, pixels: Buffer) -> ();`. Client: `attach(&AttachRequest { surface, pixels: Buffer::whole(handle, len) })`. Server: `request.pixels.map()?`. |
| Give a service a way to send me events | `method Open(events: Channel<os.lazy.x.events.v1>) -> ();` create a pair, pass one end, keep the other. |
| Send bytes larger than a parcel | A buffer plus a `ring` declaration. Never a `Bytes` field over 64 KiB. |
| Put an object inside a struct | Allowed; it is a field with a fixed place. |
| Put an object inside an option or an array | Refused by `midlc`: the kernel gate needs a fixed object count per method. Use a separate method, or a `U32` count beside fixed fields. |
| Check what a request carried | The generated decoder refuses a request whose objects do not match; nothing to do by hand. |
| See why a call was refused | `LAZYOS_LABEL_TRACE=1` for policy; `msg_stats` `undeclared_refused` for the object gate. |
| Add a third object kind | Do not, unless it is a new kernel object. Then: one enum variant, one wire tag, one `match` arm in `resolve` and one in `deliver`. |

## 3. Target design

### 3.1 Wire (parcel version 2)

```
Header (40 bytes)
  u16 version (=2)   u16 flags       u32 object_count
  u64 interface_id   u32 method      u32 body_len
  u64 txn_id         u64 reply_to
  u64 deadline_ns
Body: TLV fields. An object field is
  HANDLE  (len 4): u32 index into the object list
  BUFFER  (len 20): u32 index, u64 offset, u64 len
Object list: object_count x 16 bytes
  u32 kind (1 = Channel, 2 = Buffer)   u32 reserved (0)
  u64 handle (sender's number; the receiver ignores it)
```

`body_crc32c`, `handle_count`, `buffer_count` and the buffer descriptor go.
`MAX_OBJECTS = 8` replaces `MAX_HANDLES = 64` and `MAX_BUFFERS = 64`. This
is the Fuchsia FIDL shape: handles in the body are presence markers, the
kernel moves a flat list it never has to find.

### 3.2 Rust types (`libs/messenger`)

```rust
pub enum Object { Channel(u64), Buffer(u64) }          // sender's handles
pub struct Parcel { pub header: Header, pub body: Vec<u8>, pub objects: Vec<Object> }
pub struct Buffer { pub handle: u64, pub offset: u64, pub len: u64 }  // a decoded field
```

The encoder pushes the handle onto `objects` and writes the index into the
field; the decoder takes the installed handle at that index, and refuses a
field whose index is not its declared position (a repeated index, a skipped
one, or one past the list are all `Error::BadObjectIndex`). The kernel
resolves each list entry to `Resolved { kind, rights, object_id }` and
delivery installs them with one loop and one rollback. `Transfer`,
`BufferTransfer`, `BufferDesc`, `retain_descriptor` and the three
`kind == Buffer` branches go.

### 3.3 Receive ABI

`MsgResult.reserved[0..4]` (`first_handle`, `handles`, `first_buffer`,
`buffers`) become `object_count` plus the installed numbers, written to a
small user block beside the sender-id block. `Message` keeps them until the
generated decoder claims each one; an object no decoder claimed is closed
when the `Message` drops, so a server that ignores one cannot leak it. Today
each server closes `first_buffer` by hand in its default arm (`audiod`,
`sndd`, `netdrv`, `inputd`, `xuid`).

### 3.4 Buffer syscalls

`buffer_create`, `buffer_map`, `buffer_close` join the `msg` op family
(`op::BUFFER_CREATE = 22`, `BUFFER_MAP = 23`, `BUFFER_CLOSE = 24`; 21 is `ENDPOINT_FD`); `map`
returns the address and the size. The display ops `CREATE_BUFFER`,
`MAP_BUFFER`, `CLOSE_BUFFER` are removed once every caller moves. `create`
takes a size and nothing else.

### 3.5 MIDL

The `transfers (...)` clause is removed. `Channel<I>` and `Buffer` are
parameter types, legal in a request's parameters and inside a `struct`
(nested structs included), illegal inside `Option<T>` or `Array<T>`, in a
reply and in a topic (`midlc` errors). The restriction keeps every method's
object count fixed, which is what lets the kernel gate compare a static slice
without reading the body. The rules on
`I` (declared somewhere, has a one-way method) and on rings (`Ring<A, B>`
is a `Buffer` parameter with a layout) stay. The fifteen clauses in `idl/`
become parameters:

```idl
method AttachBuffer(surface: U64, pixels: Buffer) -> () = 2;
method CreateSurface(width: U32, height: U32, title: String, role: U32,
                     events: Channel<os.lazy.display.v1>) -> (surface: U64) = 1;
method AttachRing(slots: U32, rings: Ring<Rx, Tx>, notify: Channel<os.lazy.net.nic.v1>) -> (ring: U32);
```

`midlc` walks each request's fields in order to produce the object kinds, and
emits per interface `DECLARED_OBJECTS` and `declared_objects(interface,
method) -> &[Kind]` for the kernel gate. The generated request struct has the
object as a normal field (`pixels: Buffer`, `events: Endpoint` on receive);
`<Method>Transfers`, `encode_<method>_transfers`, `request_transfers` and
`Message::carries` disappear. The manifest's `transfers` array becomes an
`objects` array (`name`, `kind`, `index`, `interface`, `path` for a nested
field). The `--schema` table keeps the kinds so the Rhai `msg` module still
refuses a method that carries objects.

## 4. Stages

Each stage lands alone, green, with its tests. M1 and M2 change no semantics
for any caller and can go first in either order.

### M1. Delete the dead machinery

* Remove fences: `kernel/src/ipc/shared/fences.rs`, `FENCES`, `park_wait`,
  the five fence fields of `Stats` and `ProcessStats`, their rows in
  `messengerctl` `render.rs`, `fabricmon` `view.rs`, `lazyos-sys` `fabric.rs`,
  and the test `ipc_buffer_fence_submit_wait`.
* Remove the buffer flags and `Error::{BadFlags, ExecutableDenied, ShareOnly,
  StaleSequence, TimedOut}`; `shared::create(size)`. Remove
  `ipc_buffer_share_only_not_mappable`. The `dma_alloc` `SHARE_ONLY` flag is a
  different mechanism (`user/src/dev.rs`) and stays.
* Verify: `cargo test -p messenger -p build-support-tests`,
  `LAZYOS_TEST_FILTER=ipc python tools/test/run.py --accel none`, the
  `core_apps.json` session under `LAZYOS_LABEL_TRACE=1` (no new `LABEL:DENY`),
  `python tools/sound/run.py --mix`, `python tools/net/run.py --netd`.

### M2. One path for a buffer

* Refuse a `Buffer` handle in the `handles` vector (`EINVAL`), delete the
  `kind == Buffer` branches in `recv.rs` and `support.rs`, and rewrite
  `ipc_buffer_handle_transfer_rights` and the `transfer.rs` cases to use the
  buffers vector.
* Move `create`, `map`, `close` to the `msg` ops (3.4) and delete the display
  ops and their `lazyos-sys` wrappers in the same change.
* Verify as M1 plus `python tools/usb/run.py` (DMA buffers untouched) and the
  `xui_writer.json` session.

### M3. Objects as fields (wire v2, ABI, MIDL, userspace)

This is the one stage where everything rebuilds together.

* `libs/messenger`: `Object`, `Buffer`, the parcel v2 codec, index-valued
  `HANDLE`/`BUFFER` fields, `MAX_OBJECTS`; the v1 decoder is deleted, not
  kept (no parcel is ever persisted).
* `tools/midlc`: `midlc_parser.py` (no clause; `Channel<I>` and `Buffer` as
  types), `midlc_model.py` (object walk, index, path), `midlc_transfers.py`
  becomes `midlc_objects.py`, `midlc_rings.py` (`Ring` as a `Buffer`
  parameter), `midlc_rust.py` (3.5), `midlc_docs.py`, `midlc_conformance.py`,
  `midlc_schema.py`, `midlc_rhai.py`. The fifteen `.midl` methods. Regenerate
  `libs/generated`, `libs/rhai-lazy/api`, `docs/idl/`, `idl/manifest.json`;
  update `test_midlc_transfers.py` and the conformance corpus (new fixtures:
  an object in a struct accepted; in an option, in an array, in a reply and
  in a topic refused).
* Kernel: `channels/types.rs` (`Resolved`, `Message.objects`), `support.rs`
  (`resolve_objects`), `recv.rs` (one loop), `declared.rs` (exact kind-list check),
  `syscalls.rs` (3.3), `channels_kernel.rs`.
* Userspace: `user/src/messenger/message.rs` and `endpoint.rs`, then every
  sender and receiver that `grep -rn "encode_.*_transfers\|first_buffer\|first_handle"`
  finds (today: the display client, input, net, logind, xuid, inputd, netdrv,
  sndd, audiod, xui-app display and input, nicctl). Each gets shorter: the
  object is a field of the request struct, and the default arm closes nothing.
* Tests: a `channels_suite` case per rule in 2.3 (kind-list gate, duplicate
  channel, reply refused, rollback on a full table, an index out of range,
  repeated or out of order refused by the decoder with the message's objects
  then closed on drop, an unclaimed object closed on drop); a soak that
  moves a channel end back and forth 100k times and shares a buffer 100k
  times with handle-count and frame-count checks before and after; a
  `libs/messenger` seeded fuzz of the v2 decoder (`fuzz/`, `gen_corpus.py`).
* Verify: the M1 list, `python tools/abi/run.py` (unchanged matrix),
  `python tools/rhai/run.py --desktop`, `python tools/lazyrad/modplayer_run.py`,
  `python tools/accounts/run.py`, and the perf harness
  (`python tools/perf/run.py --label objects`): the in-kernel IPC round trip
  must not regress.

### M4. Documentation

* `docs/messenger.md`: replace sections 1 to 4, 9, 10 and 14 with section 2 of
  this plan and the wire of 3.1; queues, topics, ACLs and credentials stay.
  The document should get shorter.
* `docs/midl.md`: delete "Transfers: handles and buffers"; add `Channel<I>`
  and `Buffer` to "Types" with the reply and topic restrictions; rings
  reworded as a `Buffer` parameter with a layout.
* `docs/architecture/ipc-core.md`: the resolve and deliver path as one loop;
  the fences paragraph goes.
* `AGENTS.md`: one paragraph pointing at the six concepts and the cheat sheet.
* Delete this plan's section 1 once the work has landed; keep 2 and 3 as the
  reference until `messenger.md` carries them.

## 5. What this does not change

* Rings, doorbells and the ring header protocol (`libs/framering`).
* DMA buffers (`create_from_frames`, `dma_alloc`) and device claims.
* Handle rights (`CALL`, `MONITOR`, `DUPLICATE`, `TRANSFER`, `CONTROL`) and
  the `Object` handle kind. `MONITOR` and `CONTROL` have no user today and are
  a candidate for a later plan; they are not in this one.
* The ACL hook, labels, audit, credentials, topics, the registry, `wait_any`.
* Objects in topics: a published event carries none, as today.

## 6. Expected size

Rough counts from today's tree, to be checked at each stage:

| Area | Removed | Added |
|---|---|---|
| `kernel/src/ipc/shared` (fences, flags, errors, descriptors) | about 280 lines | 0 |
| `kernel/src/ipc/channels` (two vectors, Buffer branches) | about 160 lines | about 50 (one loop) |
| `libs/messenger` (descriptors, two counts) | about 60 lines | about 40 (`Object`, index fields) |
| `midlc` (transfers clause, `<Method>Transfers`, `carries`) | about 200 lines | about 80 (type walk) |
| Userspace senders and receivers (pair returns, hand closing, `carries`) | about 200 lines | 0 |
| Stats viewers | about 40 lines | 0 |

## 7. Acceptance

* The fifteen `.midl` methods lose their clause and gain the same names as
  parameters; nothing else in `idl/` changes.
* `grep -rn "first_handle\|first_buffer\|BufferDesc\|handle_count\|buffer_count\|fence_\|_transfers\|transfers (" --include=*.rs --include=*.midl`
  finds nothing outside git history.
* A new reader can answer "what can a message carry, and who owns it
  afterwards" from section 2.1 alone.
* The full kernel suite, the ABI matrix, the sound, net, USB, accounts, rhai
  and core-apps sessions pass, and `docs/perf/history.md` shows no IPC
  regression.
