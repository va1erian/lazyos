# `os.lazy.messenger.topics.publish.v1`

Interface id: `0x7ffc19b03e941e16`

Kernel ACL scope for publishing. `messengerd` asks the kernel
(`authorize_topic`) once per publish; the kernel evaluates every topic
segment against policy on this interface id, using `fnv1a32(segment)` as the
method id (so `+`/`#` hash like any segment). Only the interface id and the
request layout are contractual; the request never travels as a parcel to a
service.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| AuthorizeTopic | 727248511 | sync | `(name: String, mode: U32, txn: U64) -> ()` |

## enum `Mode`

- Publish, Subscribe
