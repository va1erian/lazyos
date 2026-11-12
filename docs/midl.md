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

## Objects: channels and buffers

A parcel carries its kernel objects in an *object list* beside its TLV body
([`docs/messenger.md`](messenger.md) sections 1 and 4); an object field in
the body holds only an index into that list, which the kernel resolves,
checks and installs in the receiver's table. In MIDL an object is a
parameter type like any other:

```idl
interface os.lazy.display.v1 {
    method CreateSurface(width: U32, height: U32, title: String, role: U32,
                         events: Channel<os.lazy.display.v1>) -> (surface: U64) = 1;
    method AttachBuffer(surface: U64, pixels: Buffer) -> () = 2;
}
```

* **`Channel<I>`**: one end of a channel pair (`user::messenger::create_pair`).
  It **moves**: the sender's handle closes when the message is queued, and
  the receiver keeps the fresh one and sends `I`'s `oneway` methods on it
  (the event-endpoint pattern). `I` may be declared in another file, and it
  must have at least one `oneway` method.
* **`Buffer`**: a shared buffer with a byte range (`libmessenger::Buffer`:
  `handle`, `offset`, `len`). It is **shared**: the sender keeps its handle
  and mapping, the receiver gets a new handle to the same pages and maps it
  when it wants; the range is data it checks against the mapped size.
* **`Ring<A, B>`**: a `Buffer` with a ring layout (below).

An object may sit in a request's parameters or inside a `struct` (nested
structs too). It is refused in a reply, in a topic payload, and inside an
`Option<T>` or an `Array<T>`: every method's object count is fixed, so the
kernel gate compares one static kind list per method, and the generated
decoder demands that each object field's index be its position in the
declared order (the parameters walked depth-first), so a repeated, skipped,
out-of-range or wrong-kind index is refused and every installed object is
claimed by exactly one field. A request carries at most eight objects.

### Generated code

A `Channel<I>` field is a `u64` (the sender's handle when encoding, the
installed one when decoded); a `Buffer` or `Ring<...>` field is a
`libmessenger::Buffer`. Both sit in the request struct (`<Method>Args`,
e.g. `AttachBufferArgs { surface: u64, pixels: libmessenger::Buffer }`) as
plain fields. For a method whose request carries objects, in the
interface's module:

* `encode_<method>_args(&value)` returns `(body, objects)`, the parcel's body
  and object list, in the field order;
* `decode_<method>_args(body, objects)` takes the installed object list the
  kernel reported (`Message::objects`) and refuses one that is not exactly
  the declared slots each claimed once, in order (`Error::BadObjectIndex`);
* `<METHOD>_OBJECTS: &[objects::Kind]`, the declared kinds, and
  `DECLARED_OBJECTS`, every `(method id, kinds)` of the interface.

A struct that holds an object gets the same shape: `encode_<struct>(&value,
objects)` pushes onto the request's list and `decode_<struct>(body, objects,
next)` claims from it. A server decodes with `Message::decode(wire::decode_<method>_args)`:
an object no decoder claimed is closed when the `Message` drops. The Markdown
reference lists each method's objects under **Objects**, and the manifest
gives each method an `objects` array (`name`, `kind`, `index`, `interface`,
`rings`, and `path` for a nested field). The `--schema` table carries them
too, and the Rhai `msg` module refuses to call a method that carries objects,
because a script cannot create a channel or a buffer.

The crate also gets `DECLARED_OBJECTS`, one `objects::ObjectDecl` per method
that carries objects (interface id, method id, kinds), and
`declared_objects(interface, method)`. The kernel enforces it on every
request (`kernel/src/ipc/channels/declared.rs`): a parcel whose object list
differs from its header's method in length, kind or order is refused with
`EINVAL` before any handle moves, and so is any object sent to an interface no
`.midl` declares.

## Rings: bulk data through shared memory

Bulk data (frames, samples) never travels in a message body. It goes through
a single-producer/single-consumer ring in a shared buffer, and messages only
wake the consumer or move a position. A `ring` declaration states that
contract in the interface, and a `Ring<...>` parameter attaches the rings:

```idl
interface os.lazy.net.nic.v1 {
    method AttachRing(slots: U32, rings: Ring<Rx, Tx>, notify: Channel<os.lazy.net.nic.v1>) -> (ring: U32);
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
* **`producer`**: `client` (the side that sends the buffer) or `server`.

`Ring<A, B>` is one shared buffer (a `Buffer` object with a layout) that
holds the listed rings back to back, all the same size; a request carries
at most one. The compiler checks that:

* the doorbell is a `oneway` method of the interface;
* the advance method exists;
* every declared ring is carried by some method;
* a `frames` ring the **server** produces comes with a `Channel` of the same
  interface in the same request, the path its doorbell travels back to the
  client.

### Generated code

The crate-level `rings` module has the `RingDecl`, `Layout` and `Side` types.
Each interface module gets:

* `RING_<NAME>: rings::RingDecl` per ring (layout, producer, and the method id
  of its doorbell or advance method);
* for the method that carries them, `<METHOD>_RINGS` (the rings in buffer
  order) and `<method>_rings(ring_bytes) -> Option<<Method>Rings>`, the
  offset of each ring plus the `total` buffer size, with overflow checked.

`user::messenger::net` builds its receive and transmit rings from
`attach_ring_rings`. The Markdown reference lists the rings under **Rings**,
and the manifest gives each interface a `rings` array.

## Language reference

This section states what `midlc` accepts (`tools/midlc/midlc_lexer.py`,
`midlc_parser.py`, `midlc_model.py`, `midlc_rust.py`). Where the compiler is
narrower than a tool might expect, the rule says so. The conformance corpus
(below) pins every rule here with examples another implementation can test
itself against.

### Lexical structure

* `//` starts a comment to the end of the line; `///` starts a doc comment,
  attached to the next declaration (consecutive lines join with `\n`).
* Tokens: identifiers `[A-Za-z_][A-Za-z0-9_.]*` (dots allowed, so interface
  names are one token), decimal numbers, double-quoted strings (`topic`,
  `service` and `scope`), `->`, and the punctuation `{ } ( ) < > : , ; =`.
* Whitespace is insignificant. Commas between struct fields and enum variants
  are optional.

### Grammar (EBNF)

```ebnf
file       = { service | interface } ;
service    = "service" STRING "implements" NAME { "," NAME } ";" ;
interface  = "interface" NAME "{" { member } "}" ;
member     = method | struct | enum | topic | ring | errors | default ;
method     = "method" IDENT params "->" params { attr } ";" ;
params     = "(" [ field { "," field } ] ")" ;
field      = IDENT ":" type [ "=" NUMBER ] ;
attr       = "=" NUMBER | "oneway" | "sync"
           | "scope" STRING | deadline | retry ;
deadline   = "deadline" "=" ( NUMBER ( "ms" | "s" ) | "never" ) ;
retry      = "retry" "=" ( "safe" | "never" ) ;
default    = "default" { deadline | retry } ";" ;
errors     = "errors" "{" { IDENT "=" NUMBER [ "," ] } "}" ;
struct     = "struct" IDENT "{" { field [ "," ] } "}" ;
enum       = "enum" IDENT "{" { IDENT [ "," ] } "}" ;
topic      = "topic" STRING ":" IDENT { "retained" | "qos" "=" IDENT } ";" ;
ring       = "ring" IDENT ":" IDENT { IDENT "=" IDENT } ";" ;
type       = IDENT [ "<" type { "," type } ">" ] ;
```

`NAME` is reverse-DNS ending in `.vN` and must match `[a-z0-9_.]+\.v\d+`
(`os.lazy.confd.v1`). Several interfaces may share a file, and a `service`
may implement an interface of another file. `sync` is the
default and only cancels an earlier `oneway`.

### Types

Built-ins: `Bool`, `I32`, `I64`, `U32`, `U64`, `F64`, `String` (UTF-8),
`Bytes`, `Array<T>` and `Option<T>` (exactly one parameter each), and the
objects `Channel<I>`, `Buffer` and `Ring<A, ...>` (above: in a request or a
struct, never in a reply, a topic, an `Option` or an `Array`; `I` must be an
interface of the compile with a `oneway` method; at most eight objects per
request; each object field's index is its position in the depth-first
declaration order). A name
declared as a `struct` or `enum` of the same interface is also a type. Enums
travel as `U32` (the variant's index, in declaration order), and an
enum-typed field is a `u32` in the generated Rust (compare it with the
`{ENUM}_{VARIANT}` constants). There is **no `Map`**: the runtime's TLV layer has
a `Map` kind, but `midlc` does not accept `Map<K, V>`, so model a map as
`Array<Entry>` of a struct with `key` and `value` fields.

### Identifiers

* **Interface id**: FNV-1a 64-bit of the interface name (`fnv1a64`).
* **Method id**: an explicit `= N` wins; otherwise FNV-1a 32-bit of the
  method name, masked to 31 bits (`fnv1a32`). Two methods of one interface may
  not share an id; the compiler rejects the file and the author renumbers one
  with `= N`. Renaming a method that relies on the hash changes its id, so pin
  the id first.
* **Field id**: `name: T = N` pins it; without `= N` a field takes its
  **1-based position** within its struct, method argument list or reply list
  (the element of an `Array` or `Option` is always 1). Ids are unique within a
  list and lie in 1..65535; a top-level reply field may not take 15, the
  standard error field (below). `midlc` warns about every implicit id,
  because reordering or inserting a field would silently renumber it, and
  `python tools/midlc/midlc.py --pin-field-ids idl/*.midl` writes the
  current positions into the source (the wire stays byte-identical). Every
  file in `idl/` is pinned, so with explicit ids the declaration order no
  longer matters to the wire.
* **Generated names**: `encode_<snake>`/`decode_<snake>` for structs,
  `encode_<method>_args`/`_reply` for methods, `{ENUM}_{VARIANT}` constants.
  Two declarations that fold to the same Rust identifier are an error.

### Wire encoding

A body is a sequence of TLV fields. Each field is an 8-byte header followed by
the payload: a little-endian `u32` tag, `kind | (id << 8)` (the id is 16 bits),
then a little-endian `u32` payload length. Kinds: `Bool`=1 (one byte), `I32`=2,
`I64`=3, `U32`=4, `U64`=5, `F64`=6 (little-endian), `String`=7, `Bytes`=8,
`Array`=9, `Struct`=10, `Map`=11 (unused by MIDL), `Option`=12, `Error`=13,
`Handle`=14 (a `Channel<I>` field: a `u32` index into the object list),
`Buffer`=15 (a `Buffer`/`Ring` field: `u32` index, `u64` offset, `u64` len).

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
* A request's channels and buffers are entries of the parcel's object list,
  in the depth-first order of its parameters; the body fields hold their
  indexes (see "Objects").

### Compatibility rules

Append-only within a `.vN`: add methods (explicit ids stay put), add struct
fields, method args and reply fields under new ids, add enum variants at the
end. With pinned ids a new field may be declared anywhere in the list;
reusing, renumbering or retyping an id needs a new interface version, as
does removing a field whose absence a peer would misread. The interface hash
is checked at bind time (`docs/messenger.md` section 11).

## Services, errors, scopes and deadlines

Four declarations state protocol facts that used to live in prose or in
constants copied between crates (`docs/client-api-plan.md` 3.1). Each is
generated, so a client or a server never spells one.

### `service`

```idl
service "os.lazy.confd" implements os.lazy.confd.v1;
service "os.lazy.net.nic/{ifname}" implements os.lazy.net.nic.v1;
service "os.lazy.net.stack" implements os.lazy.net.stack.v1, os.lazy.net.socket.v1;
```

A top-level declaration of the **registry name** a daemon registers and the
interfaces it serves under it (a list, because `netd` serves the stack and the
sockets on one endpoint). A name ending in `/{placeholder}` is a **family**:
one name per instance (`os.lazy.net.nic/eth0`). The compiler checks that every
interface exists in the compile (a service may name another file's), that a
name is declared once, and that an interface has at most one singleton name
and one family, so its name is never ambiguous.

Generated, in the interface module: `SERVICE_NAME` (the singleton name) and
`SERVICE_INSTANCE_PREFIX` (a family's name up to and including the `/`); at
crate level, `DECLARED_SERVICES` and `declared_service(interface_id)`. A client
of `os.lazy.confd.v1` resolves `os_lazy_confd_v1::SERVICE_NAME` and writes no
string. The manifest lists the names under `services`, the generated Markdown
has a **Service** section, and the Rhai module gets `SERVICE` and the schema
table the name `msg::connect` resolves.

### `errors`

```idl
interface os.lazy.confd.v1 {
    errors {
        /// No value is stored at the path.
        NotFound = 1001,
        Denied = 1004
    }
}
```

The service's own failure codes, carried in the standard error field below.
Names are PascalCase and unique, codes are unique and non-zero and fit a
`u32`; an interface has one block. Generated: `ServiceError`, a
`#[repr(u32)]` enum with `code()`, `from_code(u32) -> Option<ServiceError>`
and `name()`, and `SERVICE_ERRORS`. (`ServiceError`, not `Error`, because
every interface module already imports the codec's `libmessenger::Error`; no
struct may take the name.) Rhai modules get `ERROR_<NAME>` constants, the
manifest an `errors` array and the Markdown an **Errors** table.

### `scope`

```idl
method Offer(...) -> (token: U64) scope "os.lazy.clipboard.write.v1";
```

The ACL scope interface the request's header carries instead of the
interface's own id, as a quoted interface name (`.vN`, like an interface
name). It names a policy, not a wire interface, so no interface of that name
has to exist. Generated: `METHOD_<X>_SCOPE: u64`, the same FNV-1a 64 hash the
kernel computes for an interface id. A method without `scope` travels on
`INTERFACE_ID` and gets no constant. The clipboard's owner scope
(`Serialize`) is the 8 bytes `os.owner`, not a name hash, so it has no syntax.

### `deadline`, `retry` and `default`

```idl
interface os.lazy.confd.v1 {
    default deadline=2s;
    method Get(path: String) -> (value: String) retry=safe;
    method Fetch(...) -> (...) deadline=5s;
    method Verify(...) -> (...) deadline=never;
}
```

Two attributes state how a client calls a method, so no call site does.

* `deadline=<n>ms|<n>s|never`: how long every client waits for the reply. A
  positive count that fits 32 bits of milliseconds, or `never` (wait for ever:
  a password hash, an install; never on a UI thread, which
  `tools/xui/check_call_bounds.py` enforces). Refused on a `oneway` method
  (there is nothing to wait for).
* `retry=safe|never`: whether the client may send the request again to a
  restarted service after the peer died while it may have been handling it.
  `safe` is for a read or a write that sets a value (running it twice changes
  nothing); `never` (also what a method that states nothing gets) returns the
  error, so a launch, a mount or a password change never runs twice. Refused
  on a `oneway` method. A request the kernel provably never queued (a stale
  handle after a restart) is resent whatever the method says; see
  "Resending" below.
* `default deadline=2s retry=never;`: a line in an interface that gives every
  two-way method the attributes it does not state itself (one `default` per
  interface, stating at least one attribute; a method overrides each
  attribute separately; a `oneway` method takes none).

An interface that gets a generated client must resolve a `deadline` for every
two-way method, its own or the interface `default`: `midlc` refuses one that
does not. There is no crate-wide default bound any more.

Generated, for each two-way method that resolves them:
`METHOD_<X>_DEADLINE: libmessenger::client::Deadline` (`Deadline::millis(n)` or
`Deadline::Never`) and `METHOD_<X>_RETRY: libmessenger::client::Retry`
(`Retry::Safe` or `Retry::Never`, the latter when unstated). The manifest and
the Markdown page carry the resolved values (`deadline_ms` or `"deadline":
"never"`, and `retry`).

#### Resending

A restarted service leaves the client a cached handle whose peer is gone. The
kernel answers a call on it `EPIPE`, which the `Connection` takes as "evict the
handle, resolve again once and ask whether to resend". The kernel reports the
same `EPIPE` for two different failures: `begin_call` refuses a request whose
peer endpoint is closed (never queued: nothing ran), and an `await_reply`
whose peer closed after it took the request ends `PeerDied` (it may have run).
A single `OP_CALL` cannot tell them apart, so the transports
(`user::messenger::client::Native`, `lazyos_sys::client::Std`) issue the two
halves (`OP_CALL_BEGIN`, `OP_CALL_AWAIT`) and report `CallFailure { errno,
delivered }`. The rule in `Connection::call`:

| Failure | `retry=never` | `retry=safe` |
|---|---|---|
| never queued (`delivered` false) | resent once to the restarted service | resent once |
| queued, then the peer died | the error is returned, the handle dropped | resent once |

After any failure the handle is gone, so the next call resolves again. A
`oneway` send is always resent once: the kernel refuses it before it is queued.

## Generated crate layout

`midlc --out libs/generated/src/lib.rs` writes the crate as one file per
interface (`os_lazy_confd_v1.rs`) and per shared module (`errors.rs`,
`topics.rs`, `objects.rs`, `rings.rs`, `services.rs`, and `declared.rs` with the
crate-level tables), all `@generated`, beside a `lib.rs` that declares them.
Each interface module sits behind a cargo feature named for it without the
`os_lazy_` prefix (`confd_v1`); `all` turns every one on and is the default,
so the module paths (`messenger_generated::os_lazy_confd_v1`) are unchanged for
every dependent. A program that wants fewer sets `default-features = false`
and names its own. The `[features]` block of `libs/generated/Cargo.toml` is
rewritten between its two markers on every run and is part of `--check`. The
tables (`DECLARED_TOPICS`, `DECLARED_OBJECTS`, `DECLARED_SERVICES`) are data
and are always present.

## Generated client

An interface with a `service` (a singleton name or a family) gets a typed
client over `libmessenger::client` (`docs/client-api-plan.md` 3.2), in its
module, behind the cargo feature `<interface>_client` (`confd_v1_client`, which
implies `confd_v1`; `all` includes every one). Kernel ACL scopes and policy
loaders, and interfaces no `service` serves (a callback channel's `oneway`
events), get none.

```rust
use messenger_generated::os_lazy_confd_v1 as confd;
let conf = confd::Client::connect(transport)?;            // SERVICE_NAME, the transport's connect bound
let theme: Option<confd::Value> = conf.get("sys/ui/theme")?;
let quick = conf.get_within("sys/ui/theme", Deadline::millis(50))?;
```

* `Client<T: Transport>` with `connect(transport)`, `try_connect(transport)`
  (one attempt; both only when the interface has a singleton `service`),
  `from_connection(Connection<T>)` (a family member, or a test's `Mock`) and
  `connection()`.
* One function per method, named in `snake_case` (a name the constructors own,
  such as `Connect`, gets `_call`). Arguments are flattened: scalars and enums
  by value, `&str`, `&[u8]`, `&[T]`, `&Struct` and `Option<&T>` borrowed;
  object fields (`Buffer`, `Channel<I>`, a struct holding one) by value, moved
  into the parcel's object list.
* The return is `()` for an empty reply, the field's type for a one-field
  reply and the `XxxReply` struct otherwise.
* The request travels on the method's `scope` id when it has one, else on
  `INTERFACE_ID`, waits the method's resolved `deadline` and is resent after a
  dead peer as its `retry` says (above). Every call has exactly one deadline
  override, `<method>_within(.., Deadline)` (the retry policy is the IDL's
  alone); a `oneway` method has none (it sends and returns).
* Failures are `ClientError`, an alias of `CallError<ServiceError>` (of
  `CallError` for an interface without `errors`): a transport failure, a
  `Remote` refusal whose code the interface does not declare, or
  `Service(ServiceError)` for one it does. A request that cannot be encoded is
  `Transport(EINVAL)`, a reply that does not decode `Transport(EBADMSG)`.
* There is no non-blocking `_begin` form yet: the transport trait has no
  asynchronous call, so a UI that must not block keeps using the runtime's own
  lane until one exists.

## Errors

A failed call is answered with **one `Error` field at id 15** instead of the
declared reply fields (`docs/messenger.md` section 12). The id is the same in
every reply of every interface; `midlc` refuses it as a reply field id and
generates it, with the helpers to write and read it, as the crate-level
`errors` module of `libs/generated` (`messenger_generated::errors`:
`ERROR_FIELD`, `ReplyError`, `write`, `write_code`, `encode`, `find`,
`find_code`, `decode`, `is_error`). No service hand-types the id. Fifteen is
the id every service already used; the clipboard service (13) and the
generated async mailbox (1) moved to it.

The field's payload is the error's shape:

| Part | Encoding |
|---|---|
| `code` | `u32` LE, errno-style (positive) unless a `domain` says otherwise |
| `message` | UTF-8, up to the end of the payload or the first NUL |
| detail (optional) | a NUL, then a TLV record: `1` domain, `2` hint, `3` docs (all `String`) |

`domain` is empty (not written) for the service's own errno codes; `hint` is
how to fix it; `docs` is a documentation id such as `err.messenger.denied`.
A reader that knows only the code and the message
(`libmessenger::Field::error_parts`) still reads both from a detailed error.
Decoders of the declared reply skip the field (it is an unknown id to them),
so a client checks for the error before trusting the reply.

## Conformance corpus

`idl/conformance/` is a language-neutral test corpus for anything that reads
MIDL or speaks its wire format:

* `valid/<case>.midl` with `<case>.vectors.json`, sample values (a struct,
  `Method.args`, `Method.reply`, or `$error` for the standard error), and
  `<case>.expected.json`: the parsed model with every interface, method and
  field id resolved, the warnings, and each sample's encoded bytes (hex).
* `invalid/<case>.midl` with `<case>.expected.json`: the exact error `midlc`
  rejects it with.

The expected files are written by `python tools/midlc/conformance.py --write`
from `midlc`'s parser and an encoder independent of the Rust backend
(`tools/midlc/midlc_wire.py`). The same command writes
`libs/generated/tests/conformance.rs`, which compiles every valid case with
the Rust backend and checks that the generated codecs produce, and decode,
exactly the corpus bytes. CI (`.github/workflows/midlc.yml`) runs
`conformance.py --check` and `cargo test -p messenger-generated`, so a change
to `midlc`'s parsing, ids or encoding fails until the corpus is regenerated
deliberately. A second implementation (for example a `libs/midl` crate)
proves itself by reproducing the expected files.
