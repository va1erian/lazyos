# `os.lazy.messenger.registry.v1`

Interface id: `0x51d501afec09806c`

The Messenger service name registry (issues #89, #300).

Names are published by tasks and resolved into endpoint handles. Two paths
reach the same kernel table: the native `register`/`resolve`/`unregister`/
`list` gate ops (which take these parcels as their request and, for `List`,
fill the reply into the caller's buffer) and the `messengerd` daemon, which
forwards a client's request on its behalf over the bootstrap channel. The
kernel publishes `os.lazy.messenger.registry` itself at boot, so it is the
one name every task can resolve.

A daemon failure is answered with a hand-written structured `ERROR` field
(id 15, outside the generated field range) instead of a typed reply; the
kernel gate reports failures as negative errno values.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Register | 658098656 | sync | `(name: String, endpoint: Option<U64>, interfaces: Array<U64>, lease_ticks: U64) -> ()` |
| Resolve | 1645633795 | sync | `(name: String) -> (handle: U64)` |
| Unregister | 1480320227 | sync | `(name: String) -> ()` |
| List | 220805025 | sync | `() -> (entries: Array<Entry>)` |

## struct `Entry`

- `name: String`
- `object: U64`
- `owner: U64`
- `interfaces: Array<U64>`
- `lease_remaining: U64`
