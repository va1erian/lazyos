# `os.lazy.messenger.topics.v1`

Interface id: `0xc5734f978fef7231`

The Messenger publish/subscribe broker (issue #92, `docs/messenger.md`
section 7.2), served by `messengerd` as `os.lazy.messenger.topics`.

Topics are hierarchical (`a/b/c`); a subscription filter may use `+` for one
segment and a trailing `#` for the tail. `Publish` carries an opaque payload
parcel (at most 8 KiB); `NextEvent` parks the caller until an event is
queued or its deadline passes. Every call is checked against the
kernel-stamped sender and, for `Publish`/`Subscribe`, against the kernel
topic ACL (see `os.lazy.messenger.topics.publish.v1` and `.subscribe.v1`).
Failures reply with a structured error field (id 15, errno-style code plus
friendly text) instead of the declared reply fields, which never use that
id.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Publish | 1818372520 | sync | `(topic: String, payload: Bytes, retained: Bool) -> (matched: U64)` |
| Subscribe | 6992035 | sync | `(filter: String, qos: U32, depth: U32) -> (subscription: U64)` |
| Unsubscribe | 2099666486 | sync | `(subscription: U64) -> ()` |
| NextEvent | 1278354512 | sync | `(subscription: U64) -> (event: Event)` |
| Ack | 483717538 | sync | `(subscription: U64, sequence: U64) -> ()` |
| ListTopics | 225427937 | sync | `() -> (topics: Array<TopicInfo>)` |
| Stats | 267161228 | sync | `(subscription: U64) -> (stats: Stats)` |
| Ping | 2142761129 | sync | `() -> ()` |

## struct `Event`

- `topic: String`
- `publisher: U64`
- `sequence: U64`
- `retained: Bool`
- `payload: Bytes`

## struct `TopicInfo`

- `topic: String`
- `subscribers: U64`
- `retained: Bool`

## struct `Stats`

- `qos: U32`
- `depth: U32`
- `queued: U64`
- `delivered: U64`
- `matched: U64`
- `drops: U64`

## enum `Qos`

- Latest, Buffered, Conflate, Reliable
