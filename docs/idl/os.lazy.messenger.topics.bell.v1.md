# `os.lazy.messenger.topics.bell.v1`

Interface id: `0xd4c79d9b36918ea0`

The subscription doorbell (`os.lazy.messenger.topics.v1` `Bell`): what
`messengerd` sends on a subscriber's bell channel. One `Ready` per batch:
the subscriber drains the subscription until `NextEvent` comes back empty,
which re-arms the bell.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Ready | 197800596 | oneway | `(subscription: U64) -> ()` |
