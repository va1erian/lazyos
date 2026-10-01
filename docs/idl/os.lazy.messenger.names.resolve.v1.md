# `os.lazy.messenger.names.resolve.v1`

Interface id: `0x51c42ba74885199f`

Kernel ACL scope for resolving a service name. The kernel evaluates a
labelled task's `Resolve` of `name` against its rules on this interface id,
using `fnv1a32(name)` as the method id, so a rule grants one exact name.
Only the interface id is contractual; nothing ever serves this interface.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Resolve | 1645633795 | sync | `(name: String) -> ()` |
