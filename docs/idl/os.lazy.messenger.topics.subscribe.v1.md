# `os.lazy.messenger.topics.subscribe.v1`

Interface id: `0xefbc15f14c9d4bef`

Kernel ACL scope for `Subscribe`/`Unsubscribe`; same request layout as the
publish scope, with `mode` `1` (subscribe) and a filter as `name`.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| AuthorizeTopic | 727248511 | sync | `(name: String, mode: U32, txn: U64) -> ()` |

## enum `Mode`

- Publish, Subscribe
