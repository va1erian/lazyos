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
