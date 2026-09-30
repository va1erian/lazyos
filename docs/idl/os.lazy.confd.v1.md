# `os.lazy.confd.v1`

Interface id: `0xdf3c79dfb9f8f2e0`

The hierarchical configuration registry (issue #260).

The store is a tree of one typed value per path; reachable only through
Messenger, and every call is checked against the kernel-stamped caller uid.
Change notifications are best-effort: a subscriber observes
`(path, deleted)` and must re-read the value.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Get | 915881719 | sync | `(path: String) -> (value: Option<Value>)` |
| Set | 682729123 | sync | `(path: String, value: Value) -> ()` |
| Delete | 1469573738 | sync | `(path: String) -> ()` |
| List | 220805025 | sync | `(prefix: String) -> (paths: Array<String>)` |
| Info | 266462757 | sync | `() -> (store_dir: String, persistent: Bool)` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/confd/changed/#` | `Change` | latest | no | `publish:system/confd/changed/#`, `subscribe:system/confd/changed/#` |

## struct `Value`

- `kind: U32`
- `bool_value: Option<Bool>`
- `i64_value: Option<I64>`
- `u64_value: Option<U64>`
- `str_value: Option<String>`
- `bytes_value: Option<Bytes>`

## struct `Change`

- `path: String`
- `deleted: Bool`
