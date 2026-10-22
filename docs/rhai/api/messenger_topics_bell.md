# `sys::messenger_topics_bell`

The Rhai module for the Messenger interface `os.lazy.messenger.topics.bell.v1`. Wire-level reference: [os.lazy.messenger.topics.bell.v1](../../idl/os.lazy.messenger.topics.bell.v1.md).

The subscription doorbell (`os.lazy.messenger.topics.v1` `Bell`): what
`messengerd` sends on a subscriber's bell channel. One `Ready` per batch:
the subscriber drains the subscription until `NextEvent` comes back empty,
which re-arms the bell.

## Functions

### `ready(subscription)`

`Ready(subscription: U64) -> () oneway`

`subscription` has events waiting.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `subscription` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::messenger_topics_bell::ready(subscription);
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.messenger.topics.bell.v1 on the wire](../../idl/os.lazy.messenger.topics.bell.v1.md)
