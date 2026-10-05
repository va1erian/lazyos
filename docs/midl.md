# MIDL: interface and topic declarations

`.midl` files are the single source of truth for every Messenger interface. The
grammar, the versioning rules and the code generator are described in
[`docs/messenger.md`](messenger.md) section 11 and
[`tools/midlc/README.md`](../tools/midlc/README.md). This page adds the
**topic** declaration introduced for issue #307.

A topic used to live only in prose: its name pattern, payload type, QoS default,
retained flag and the `publish:`/`subscribe:` permissions were restated by hand
at every call site, and some payloads were hand-encoded. A `topic` declaration
puts all of that in the interface that owns the topic.

## Grammar

```idl
interface os.lazy.clipboard.v1 {
    struct OfferMeta { /* ... */ }

    topic "session/{session}/clipboard/changed" : OfferMeta retained qos=latest;
}
```

The declaration is `topic "<pattern>" : <Payload> [retained] [qos=<qos>];`.

* **Pattern** — a quoted topic filter: `/`-separated segments where `+` matches
  exactly one segment and a trailing `#` matches zero or more. The pattern is
  normalized before it is documented or generated.
* **Payload** — a `struct` or `enum` declared in the *same* interface. A struct
  payload gets the struct's `encode_*`/`decode_*` codec; an enum payload travels
  as a `U32`.
* **`retained`** — optional flag: publishers mark the topic retained and a late
  subscriber receives the value immediately (state-like topics).
* **`qos`** — one of the `Qos` values from [`idl/topics.midl`](../idl/topics.midl):
  `latest` (the default), `buffered`, `conflate` or `reliable`.

### Named placeholders

`{name}` is sugar for a `+` segment, and `{name...}` for a trailing `#`. The
name becomes the generated helper's Rust parameter, which is why it is worth
writing:

```idl
topic "session/{session}/clipboard/changed" : OfferMeta retained qos=latest;
topic "system/confd/changed/{path...}" : Change qos=latest;
```

A pattern may mix literals and bare `+`/`#`; a bare wildcard gets an automatic
parameter name (`wildcard<index>`).

## Validation

Patterns are validated at parse time, against the same rules as the broker
(`user/src/bin/messengerd/filter.rs`) and the kernel ACL gate
(`kernel/src/ipc/topics.rs`), so a declaration can never name a topic the
broker would refuse:

* non-empty, at most 128 bytes, at most 8 segments;
* no empty segment (no leading/trailing `/` or `//`);
* literal segments use the broker's byte set (`[A-Za-z0-9_.-]`) and may not mix
  a wildcard into a literal (`a+b` is an error; write `a/+` or `{name}`);
* `#` (and `{name...}`) may only be the last segment;
* a placeholder name is used at most once;
* the payload must be a struct or enum of the same interface;
* two topics may not normalize to the same pattern or share a generated name,
  and a topic's suffix may not collide with a struct/method codec name.

The pattern must contain at least one literal segment, so the generated name
has a stable identifier.

## Generated code

`midlc` emits, per topic, into `libs/generated`:

* `TOPIC_<NAME>`, `TOPIC_<NAME>_QOS` and `TOPIC_<NAME>_RETAINED` constants;
* `name_<name>(...)` — builds a concrete topic name, rejecting `+`, `#`, `/`
  and empty segments (publishing to a wildcard is impossible);
* `encode_<name>` / `decode_<name>` — the typed payload codec; `decode_*`
  rejects malformed bytes;
* `publish_<name>(publisher, <segments>, value)` — encodes and publishes
  through a `topics::Publish` transport;
* `subscribe_<name>(subscriber, <segments>)` — builds a filter (`+` and a
  trailing `#` are allowed) and subscribes through a `topics::Subscribe`
  transport.

The transport traits keep the generated crate transport-agnostic (`libs/generated`
depends only on `libmessenger`); `user` implements them for
`user::central::Bus`. The crate-level `DECLARED_TOPICS` table and
`declared_topic()` let `messengerctl topics` annotate each live topic with its
declared payload type.

The manifest (`idl/manifest.json`) records every topic's pattern, payload, QoS,
retained flag and the derived `publish:<pattern>` / `subscribe:<pattern>`
permission strings (security-model section 6); the permissions are never
hand-typed. The generated Markdown (`docs/idl/*.md`) lists the same table.

## Transfers: handles and buffers

A parcel carries kernel objects in two vectors beside its TLV body, `handles`
and `buffers` (`docs/messenger.md` section 4). The kernel rewrites each one into
the receiver's handle table, so a handle number written into the *body* would
mean nothing to the receiver. That is why `Handle`, `Buffer` and `Channel` are
refused as body types. Every object a request carries is declared instead in
a `transfers (...)` clause after the method:

```idl
interface os.lazy.display.v1 {
    method CreateSurface(width: U32, height: U32, title: String, role: U32) -> (surface: U64) = 1
        transfers (events: Channel<os.lazy.display.v1>);
    method AttachBuffer(surface: U64) -> () = 2
        transfers (pixels: Buffer);
}
```

* **`Channel<I>`**: one end of a channel pair (`user::messenger::create_pair`).
  The receiver keeps it and sends `I`'s `oneway` methods on it: this is how
  input reaches a window (the event-endpoint pattern). `I` may be declared in
  another file, and it must have at least one `oneway` method.
* **`Buffer`**: a shared buffer (`BufferDesc`), mapped by the receiver.

Each kind fills its vector in declaration order (`handles[0]`, `buffers[0]`).
A request carries at most one of each kind, because the kernel tells the
receiver only the first handle and the first buffer of a delivery
(`Message::first_handle`, `first_buffer`). The kernel refuses transfers in a
reply, so the clause belongs to the request.

### Generated code

For each method with a clause, in the interface's module:

* `<METHOD>_TRANSFERS: transfers::Transfers`: the declared counts;
* `<Method>Transfers`: the objects by name (`u64` for a channel,
  `BufferDesc` for a buffer), and `encode_<method>_transfers(&value)`, which
  returns the parcel's `(handles, buffers)`.

Every module also gets `request_transfers(method)`, the declared counts of any
method id (`Transfers::NONE` when it declares none). A server checks a delivery
with `Message::carries(wire::OPEN_TRANSFERS)` (or
`carries(wire::request_transfers(method))`): the check is exact, so an
undeclared object is refused rather than adopted. The Markdown reference lists
each method's slots under **Transfers**, and the manifest gives each method a
`transfers` array (`name`, `kind`, `slot`, `interface`). The `--schema` table
carries them too, and the Rhai `msg` module refuses to call a method that
declares transfers, because a script cannot create a channel or a buffer.

## Rings: bulk data through shared memory

Bulk data (frames, samples) never travels in a message body. It goes through
a single-producer/single-consumer ring in a shared buffer, and messages only
wake the consumer or move a position. A `ring` declaration states that
contract in the interface, and a `Ring<...>` transfer attaches the rings:

```idl
interface os.lazy.net.nic.v1 {
    method AttachRing(slots: U32) -> (ring: U32)
        transfers (rings: Ring<Rx, Tx>, notify: Channel<os.lazy.net.nic.v1>);
    method Kick(ring: U32) -> () oneway;
    method Notify(ring: U32, events: U32) -> () oneway;
    /// The receive ring: frames the card received, driver to client.
    ring Rx : frames producer=server doorbell=Notify;
    /// The transmit ring: frames to send, client to driver.
    ring Tx : frames producer=client doorbell=Kick;
}
```

The declaration is `ring <Name> : <layout> producer=<side> doorbell=<Method>`
or `... advance=<Method>`:

* **Layout `frames`**: fixed slots, with the indices and an `armed` flag in a
  header page (`libs/framering`). The producer sends the `oneway` **doorbell**
  method only when the consumer armed the ring, so a burst costs one message.
* **Layout `stream`**: a byte ring whose position travels in calls. The
  producer reports how far it wrote with the **advance** method, whose reply
  says how far the consumer read (audio's `Commit`).
* **`producer`**: `client` (the side that transfers the buffer) or `server`.

`Ring<A, B>` is one shared buffer that holds the listed rings back to back,
all the same size. It takes the request's buffer slot, because a request
carries at most one buffer. The compiler checks that:

* the doorbell is a `oneway` method of the interface;
* the advance method exists;
* every declared ring is transferred by some method;
* a `frames` ring the **server** produces comes with a `Channel` of the same
  interface in the same request, the path its doorbell travels back to the
  client.

### Generated code

The crate-level `rings` module has the `RingDecl`, `Layout` and `Side` types.
Each interface module gets:

* `RING_<NAME>: rings::RingDecl` per ring (layout, producer, and the method id
  of its doorbell or advance method);
* for the method that transfers them, `<METHOD>_RINGS` (the rings in buffer
  order) and `<method>_rings(ring_bytes) -> Option<<Method>Rings>`, the
  offset of each ring plus the `total` buffer size, with overflow checked.

`user::messenger::net` builds its receive and transmit rings from
`attach_ring_rings`. The Markdown reference lists the rings under **Rings**,
and the manifest gives each interface a `rings` array.

## Language reference

This section states what `midlc` accepts today (`tools/midlc/midlc_lexer.py`,
`midlc_parser.py`, `midlc_model.py`, `midlc_rust.py`). Where the compiler is
narrower than a tool might expect, the rule says so (issue #306 tracks the
remaining work: explicit field ids, a standard error field, a conformance
corpus).

### Lexical structure

* `//` starts a comment to the end of the line; `///` starts a doc comment,
  attached to the next declaration (consecutive lines join with `\n`).
* Tokens: identifiers `[A-Za-z_][A-Za-z0-9_.]*` (dots allowed, so interface
  names are one token), decimal numbers, double-quoted strings (used only by
  `topic`), `->`, and the punctuation `{ } ( ) < > : , ; =`.
* Whitespace is insignificant. Commas between struct fields and enum variants
  are optional.

### Grammar (EBNF)

```ebnf
file       = interface { interface } ;
interface  = "interface" NAME "{" { member } "}" ;
member     = method | struct | enum | topic | ring ;
method     = "method" IDENT params "->" params { attr } ";" ;
params     = "(" [ IDENT ":" type { "," IDENT ":" type } ] ")" ;
attr       = "=" NUMBER | "oneway" | "sync" | "transfers" params ;
struct     = "struct" IDENT "{" { IDENT ":" type [ "," ] } "}" ;
enum       = "enum" IDENT "{" { IDENT [ "," ] } "}" ;
topic      = "topic" STRING ":" IDENT { "retained" | "qos" "=" IDENT } ";" ;
ring       = "ring" IDENT ":" IDENT { IDENT "=" IDENT } ";" ;
type       = IDENT [ "<" type { "," type } ">" ] ;
```

`NAME` is reverse-DNS ending in `.vN` and must match `[a-z0-9_.]+\.v\d+`
(`os.lazy.confd.v1`). Several interfaces may share a file. `sync` is the
default and only cancels an earlier `oneway`. A method has at most one
`transfers` clause.

### Types

Built-ins: `Bool`, `I32`, `I64`, `U32`, `U64`, `F64`, `String` (UTF-8),
`Bytes`, `Array<T>` and `Option<T>` (exactly one parameter each). A name
declared as a `struct` or `enum` of the same interface is also a type. Enums
travel as `U32` (the variant's index, in declaration order). `Channel`,
`Buffer` and `Ring` (and the `Handle` kind) appear only inside `transfers (...)`
and are rejected in a body. There is **no `Map`**: the runtime's TLV layer has
a `Map` kind, but `midlc` does not accept `Map<K, V>`, so model a map as
`Array<Entry>` of a struct with `key` and `value` fields.

### Identifiers

* **Interface id**: FNV-1a 64-bit of the interface name (`fnv1a64`).
* **Method id**: an explicit `= N` wins; otherwise FNV-1a 32-bit of the
  method name, masked to 31 bits (`fnv1a32`). Two methods of one interface may
  not share an id; the compiler rejects the file and the author renumbers one
  with `= N`. Renaming a method that relies on the hash changes its id, so pin
  the id first.
* **Field id**: implicit, **1..n in declaration order** within each struct,
  method argument list and method reply list (and 1 for the element of an
  `Array` or `Option`). Explicit `name: T = 3` is *not* supported yet, so
  reordering or inserting a field before the end silently changes the wire.
* **Generated names**: `encode_<snake>`/`decode_<snake>` for structs,
  `encode_<method>_args`/`_reply` for methods, `{ENUM}_{VARIANT}` constants.
  Two declarations that fold to the same Rust identifier are an error.

### Wire encoding

A body is a sequence of TLV fields. Each field is an 8-byte header followed by
the payload: a little-endian `u32` tag, `kind | (id << 8)` (the id is 16 bits),
then a little-endian `u32` payload length. Kinds: `Bool`=1 (one byte), `I32`=2,
`I64`=3, `U32`=4, `U64`=5, `F64`=6 (little-endian), `String`=7, `Bytes`=8,
`Array`=9, `Struct`=10, `Map`=11 (unused by MIDL), `Option`=12, `Error`=13,
`Handle`=14, `Buffer`=15 (the last two never in a MIDL body).

* `Array<T>`: one `Array` field whose payload is the elements, each a TLV with
  id 1.
* `Option<T>`: one `Option` field; an empty payload is `None`, otherwise the
  payload holds the value as a TLV with id 1.
* A struct field is a `Struct` field whose payload is the struct's own body.
* A decoder skips unknown field ids, and a field that is absent keeps its
  `Default`. So appending fields is compatible; removing, reordering or
  retyping is not (publish `.v2`).
* `oneway` methods have no reply (`returns` must be empty) and set the
  `ONE_WAY` call flag; the caller does not wait.
* A request's channels and buffers are not in the body: they ride in the
  parcel's `handles`/`buffers` vectors, in the order `transfers (...)` lists
  (see above).

### Compatibility rules

Append-only within a `.vN`: add methods (explicit ids stay put), add
trailing struct fields, add trailing method args and reply fields, add enum
variants at the end. Anything else needs a new interface version. The
interface hash is checked at bind time (`docs/messenger.md` section 11).
