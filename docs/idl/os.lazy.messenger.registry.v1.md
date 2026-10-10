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

A daemon failure is answered with the standard error field (`docs/midl.md`)
(id 15, which no reply field may take) instead of a typed reply; the
kernel gate reports failures as negative errno values.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Register | 658098656 | sync | `(name: String, endpoint: Option<U64>, interfaces: Array<U64>, lease_ticks: U64, interface_names: Array<String>) -> ()` |
| Resolve | 1645633795 | sync | `(name: String) -> (handle: U64)` |
| Connect | 1535748249 | sync | `(name: String) -> (handle: U64)` |
| Connected | 2079757168 | oneway | `(name: String, connection: Channel<os.lazy.messenger.registry.v1>) -> ()` |
| Unregister | 1480320227 | sync | `(name: String) -> ()` |
| List | 220805025 | sync | `() -> (entries: Array<Entry>)` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| Connected | `connection` | `Channel<os.lazy.messenger.registry.v1>` | `objects[0]`, a channel the receiver sends `os.lazy.messenger.registry.v1` on |

## struct `Entry`

- `name: String`
- `object: U64`
- `owner: U64`
- `interfaces: Array<U64>`
- `lease_remaining: U64`
- `owner_uid: U64`
- `owner_label: U64`
- `owner_session: U64`
