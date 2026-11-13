# `os.lazy.clipboard.v1`

Interface id: `0x5a8da8f22670b758`

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
`Request` on `fnv1a64("os.lazy.clipboard.read.v1")` (both declared with
`scope` on the method and generated as `METHOD_*_SCOPE`), `Serialize`
(served by an offer's owner, not the service) on the owner scope, a magic
id that is not a name hash and so has no `scope` syntax, and `Ping` and
`Current` on this interface's own id. The deadlines are what every client
gives the call: half a second for an ordinary one (a copy or paste key
runs on a UI thread), five seconds for one that moves a large payload.
Failures reply with the standard error field (id 15, `docs/midl.md`) instead of the
declared reply fields, which never use that id.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Offer | 1313375869 | sync | `(owner: String, sink: Option<String>, mimes: Array<String>, data: Array<Payload>) -> (token: U64)` |
| OfferBuffer | 2074208755 | sync | `(owner: String, mime: String, data: Buffer) -> (token: U64)` |
| Request | 38093138 | sync | `(token: U64, mime: String) -> (token: U64, mime: String, lazy: Bool, bytes: Bytes, size: U64)` |
| Fetch | 1396322691 | sync | `(token: U64, mime: String, into: Buffer) -> (written: U64)` |
| Serialize | 1116160801 | sync | `(token: U64, mime: String) -> (bytes: Bytes)` |
| Ping | 2142761129 | sync | `() -> ()` |
| Current | 869319546 | sync | `() -> (offer: Option<OfferMeta>)` |

## Service

- Served under the registry name `os.lazy.clipboard`; registered once.

## Scopes, deadlines and retries

| Method | Scope interface | Scope id | Deadline | Retry |
|---|---|---|---|---|
| Offer | `os.lazy.clipboard.write.v1` | `0x828647c8d352e8f3` | 500 ms | never |
| OfferBuffer | this interface | - | 5000 ms | never |
| Request | `os.lazy.clipboard.read.v1` | `0x8c863642df2825cc` | 500 ms | never |
| Fetch | this interface | - | 5000 ms | never |
| Serialize | this interface | - | 5000 ms | never |
| Ping | this interface | - | 5000 ms | safe |
| Current | this interface | - | 5000 ms | safe |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| OfferBuffer | `data` | `Buffer` | `objects[0]`, a shared buffer |
| Fetch | `into` | `Buffer` | `objects[0]`, a shared buffer |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `session/+/clipboard/changed` | `OfferMeta` | latest | yes | `publish:session/+/clipboard/changed`, `subscribe:session/+/clipboard/changed` |
| `system/events/clipboard/paste` | `PasteEvent` | latest | no | `publish:system/events/clipboard/paste`, `subscribe:system/events/clipboard/paste` |
| `system/events/security/clipboard` | `ClipboardDenial` | latest | no | `publish:system/events/security/clipboard`, `subscribe:system/events/security/clipboard` |

## struct `Payload`

- `mime: String`
- `bytes: Bytes`

## struct `OfferMeta`

- `token: U64`
- `owner: String`
- `session: U64`
- `mimes: Array<String>`
- `lazy: Bool`
- `tick: U64`

## struct `PasteEvent`

- `seq: U64`
- `uid: U32`
- `session: U64`
- `mime: String`
- `app: U64`
- `bytes: U64`
- `lazy: Bool`

## struct `ClipboardDenial`

- `seq: U64`
- `uid: U32`
- `session: U64`
- `mime: String`
- `app: U64`
- `token: U64`
