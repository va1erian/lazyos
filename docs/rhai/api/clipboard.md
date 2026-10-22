# `sys::clipboard`

The Rhai module for the Messenger interface `os.lazy.clipboard.v1`. Wire-level reference: [os.lazy.clipboard.v1](../../idl/os.lazy.clipboard.v1.md).

The per-session clipboard service (issue #115).

A client publishes an *offer* (typed MIME payloads, eager or lazy) and
receives a token; a paste is a `Request` for one MIME of a token in the
caller's own session. The session of every call is the kernel-stamped
caller session, never a wire field, so a token from another session is
refused with `EACCES`.

One wire interface, three policy ids: MIDL gives every interface a single
id, but the ACL layer scopes the service by capability, so a parcel's
header carries a *scope interface id* while its method id comes from this
file. `Offer` travels on `fnv1a64("os.lazy.clipboard.write.v1")`,
`Request` on `fnv1a64("os.lazy.clipboard.read.v1")`, `Serialize` (served by
an offer's owner, not the service) on the owner scope, and `Ping` and
`Current` on this interface's own id. The scope names are ACL names, not
wire fields, so they live as constants in `user/src/messenger/clipboard`.
Failures reply with the standard error field (id 15, `docs/midl.md`) instead of the
declared reply fields, which never use that id.

## Functions

### `offer(owner, sink, mimes, data)`

`Offer(owner: String, sink: Option<String>, mimes: Array<String>, data: Array<Payload>) -> (token: U64)`

Publish typed payloads for the caller's session and return the new
offer token. An eager offer fills `data` (one bounded copy is kept); a
lazy offer sets `sink`, the registry name of the endpoint where the
owner serves `Serialize`, and leaves `data` empty.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `owner` | `String` | string |
| `sink` | `Option<String>` | string or () |
| `mimes` | `Array<String>` | array of string |
| `data` | `Array<Payload>` | array of map (`Payload`) |

Returns `token`: `U64` (int).

```rhai
let result = sys::clipboard::offer(owner, sink, mimes, data);
```

### `request(token, mime)`

`Request(token: U64, mime: String) -> (token: U64, mime: String, lazy: Bool, bytes: Bytes, size: U64)`

Read one MIME of an offer. `token` 0 selects the newest offer in the
caller's session that lists `mime`; another session's token fails with
`EACCES` and is audited, an unknown one with `ENOENT`. `size` is the
payload's length. A payload up to the inline cap rides in `bytes`; a
larger one leaves `bytes` empty and is read with `Fetch`, using the
reply's `token`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `token` | `U64` | int |
| `mime` | `String` | string |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `token` | `U64` | int |
| `mime` | `String` | string |
| `lazy` | `Bool` | bool |
| `bytes` | `Bytes` | blob |
| `size` | `U64` | int |

```rhai
let result = sys::clipboard::request(token, mime);
```

### `serialize(token, mime)`

`Serialize(token: U64, mime: String) -> (bytes: Bytes)`

Serialize one MIME of a lazy offer on demand. The service calls this on
the offer owner's `sink` endpoint when a paste happens.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `token` | `U64` | int |
| `mime` | `String` | string |

Returns `bytes`: `Bytes` (blob).

```rhai
let result = sys::clipboard::serialize(token, mime);
```

### `ping()`

`Ping() -> ()`

Liveness probe.

Returns `()`.

```rhai
let result = sys::clipboard::ping();
```

### `current()`

`Current() -> (offer: Option<OfferMeta>)`

Metadata of the caller session's newest offer, never content; empty
when no offer is live.

Returns `offer`: `Option<OfferMeta>` (map (`OfferMeta`) or ()).

```rhai
let result = sys::clipboard::current();
```

Not callable from a script (the request carries a kernel object): `OfferBuffer`, `Fetch`.

## Types

### `Payload`

One inline `{MIME, bytes}` payload of an eager offer.

| Name | IDL type | Script value |
|---|---|---|
| `mime` | `String` | string |
| `bytes` | `Bytes` | blob |

```rhai
let value = sys::clipboard::new_payload();
```

### `OfferMeta`

Metadata for one live offer: identity and MIME types, never content.
Also the body of the retained `session/<id>/clipboard/changed` event.

| Name | IDL type | Script value |
|---|---|---|
| `token` | `U64` | int |
| `owner` | `String` | string |
| `session` | `U64` | int |
| `mimes` | `Array<String>` | array of string |
| `lazy` | `Bool` | bool |
| `tick` | `U64` | int |

```rhai
let value = sys::clipboard::new_offer_meta();
```

### `PasteEvent`

One allowed paste, the payload of `system/events/clipboard/paste`
(issue #307). `seq` is the service's monotonically increasing paste
counter, `uid`/`session` the kernel-stamped caller, `app` the owner
slot, and `lazy` whether the bytes came from a lazy owner callback.

| Name | IDL type | Script value |
|---|---|---|
| `seq` | `U64` | int |
| `uid` | `U32` | int |
| `session` | `U64` | int |
| `mime` | `String` | string |
| `app` | `U64` | int |
| `bytes` | `U64` | int |
| `lazy` | `Bool` | bool |

```rhai
let value = sys::clipboard::new_paste_event();
```

### `ClipboardDenial`

One refused cross-session paste, the payload of
`system/events/security/clipboard` (issue #307): the audit record of a
token read denied by the clipboard read scope.

| Name | IDL type | Script value |
|---|---|---|
| `seq` | `U64` | int |
| `uid` | `U32` | int |
| `session` | `U64` | int |
| `mime` | `String` | string |
| `app` | `U64` | int |
| `token` | `U64` | int |

```rhai
let value = sys::clipboard::new_clipboard_denial();
```

## Topics

### `session/{session}/clipboard/changed`

The retained per-session offer-announcement topic (issue #307): paste
UIs refresh from it without polling, and a late subscriber is handed
the live offer. The payload is an `OfferMeta`, never content.

Payload `OfferMeta`, QoS `latest`, retained.

Helpers: `SESSION_CHANGED_PATTERN`, `session_changed_topic(session)`, `on_session_changed(session, handler)`, `subscribe_session_changed(session)`, `publish_session_changed(session, payload)`.

```rhai
sys::clipboard::on_session_changed(session, |event| print(event.payload));
```

### `system/events/clipboard/paste`

Published for every allowed paste, so `logd` keeps the audit trail.

Payload `PasteEvent`, QoS `latest`.

Helpers: `PASTE_PATTERN`, `on_paste(handler)`, `subscribe_paste()`, `publish_paste(payload)`.

```rhai
sys::clipboard::on_paste(|event| print(event.payload));
```

### `system/events/security/clipboard`

Published for every refused cross-session paste.

Payload `ClipboardDenial`, QoS `latest`.

Helpers: `SECURITY_PATTERN`, `on_security(handler)`, `subscribe_security()`, `publish_security(payload)`.

```rhai
sys::clipboard::on_security(|event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.clipboard.v1 on the wire](../../idl/os.lazy.clipboard.v1.md)
