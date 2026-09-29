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
`Request` on `fnv1a64("os.lazy.clipboard.read.v1")`, `Serialize` (served by
an offer's owner, not the service) on the owner scope, and `Ping` and
`Current` on this interface's own id. The scope names are ACL names, not
wire fields, so they live as constants in `user/src/messenger/clipboard`.
Failures reply with a structured error field (id 13) instead of the
declared reply fields, which never use that id.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Offer | 1313375869 | sync | `(owner: String, sink: Option<String>, mimes: Array<String>, data: Array<Payload>) -> (token: U64)` |
| Request | 38093138 | sync | `(token: U64, mime: String) -> (token: U64, mime: String, lazy: Bool, bytes: Bytes)` |
| Serialize | 1116160801 | sync | `(token: U64, mime: String) -> (bytes: Bytes)` |
| Ping | 2142761129 | sync | `() -> ()` |
| Current | 869319546 | sync | `() -> (offer: Option<OfferMeta>)` |

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
