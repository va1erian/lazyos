# `sys::messenger_topics`

The Rhai module for the Messenger interface `os.lazy.messenger.topics.v1`. Wire-level reference: [os.lazy.messenger.topics.v1](../../idl/os.lazy.messenger.topics.v1.md).

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

## Functions

### `publish(topic, payload, retained)`

`Publish(topic: String, payload: Bytes, retained: Bool) -> (matched: U64)`

Publish `payload` under the literal `topic`; `retained` also remembers
it as the topic's retained value. Returns how many subscriptions the
event reached.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `topic` | `String` | string |
| `payload` | `Bytes` | blob |
| `retained` | `Bool` | bool |

Returns `matched`: `U64` (int).

```rhai
let result = sys::messenger_topics::publish(topic, payload, retained);
```

### `subscribe(filter, qos, depth)`

`Subscribe(filter: String, qos: U32, depth: U32) -> (subscription: U64)`

Create a subscription for `filter`. `qos` is a `Qos` value; `depth` is
the queue depth for `Buffered` (clamped to 1..=64) and ignored by the
other policies, which use fixed depths. Retained values are replayed.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `filter` | `String` | string |
| `qos` | `U32` | int |
| `depth` | `U32` | int |

Returns `subscription`: `U64` (int).

```rhai
let result = sys::messenger_topics::subscribe(filter, qos, depth);
```

### `unsubscribe(subscription)`

`Unsubscribe(subscription: U64) -> ()`

Drop a subscription; later publishes stop matching it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `subscription` | `U64` | int |

Returns `()`.

```rhai
let result = sys::messenger_topics::unsubscribe(subscription);
```

### `next_event(subscription)`

`NextEvent(subscription: U64) -> (event: Event)`

Wait for (or, with an expired deadline, poll) the next event of a
subscription. A `Reliable` subscription redelivers its head until acked.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `subscription` | `U64` | int |

Returns `event`: `Event` (map (`Event`)).

```rhai
let result = sys::messenger_topics::next_event(subscription);
```

### `ack(subscription, sequence)`

`Ack(subscription: U64, sequence: U64) -> ()`

Retire every event up to `sequence` on a `Reliable` subscription.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `subscription` | `U64` | int |
| `sequence` | `U64` | int |

Returns `()`.

```rhai
let result = sys::messenger_topics::ack(subscription, sequence);
```

### `list_topics()`

`ListTopics() -> (topics: Array<TopicInfo>)`

Topics the broker has seen, with live subscriber counts.

Returns `topics`: `Array<TopicInfo>` (array of map (`TopicInfo`)).

```rhai
let result = sys::messenger_topics::list_topics();
```

### `stats(subscription)`

`Stats(subscription: U64) -> (stats: Stats)`

Queue and drop counters of one subscription.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `subscription` | `U64` | int |

Returns `stats`: `Stats` (map (`Stats`)).

```rhai
let result = sys::messenger_topics::stats(subscription);
```

### `ping()`

`Ping() -> ()`

Round-trip probe used to detect a live broker.

Returns `()`.

```rhai
let result = sys::messenger_topics::ping();
```

Not callable from a script (the request carries a kernel object): `Bell`.

## Types

### `Event`

One delivered event: broker metadata plus the publisher's opaque payload
parcel (still encoded).

| Name | IDL type | Script value |
|---|---|---|
| `topic` | `String` | string |
| `publisher` | `U64` | int |
| `sequence` | `U64` | int |
| `retained` | `Bool` | bool |
| `payload` | `Bytes` | blob |

```rhai
let value = sys::messenger_topics::new_event();
```

### `TopicInfo`

One row of the topic list.

| Name | IDL type | Script value |
|---|---|---|
| `topic` | `String` | string |
| `subscribers` | `U64` | int |
| `retained` | `Bool` | bool |

```rhai
let value = sys::messenger_topics::new_topic_info();
```

### `Stats`

Per-subscription delivery counters.

| Name | IDL type | Script value |
|---|---|---|
| `qos` | `U32` | int |
| `depth` | `U32` | int |
| `queued` | `U64` | int |
| `delivered` | `U64` | int |
| `matched` | `U64` | int |
| `drops` | `U64` | int |

```rhai
let value = sys::messenger_topics::new_stats();
```

### `Qos`

A string; the constants of this module are:

- `QOS_LATEST` = `"Latest"`
- `QOS_BUFFERED` = `"Buffered"`
- `QOS_CONFLATE` = `"Conflate"`
- `QOS_RELIABLE` = `"Reliable"`

`QOS` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.messenger.topics.v1 on the wire](../../idl/os.lazy.messenger.topics.v1.md)
