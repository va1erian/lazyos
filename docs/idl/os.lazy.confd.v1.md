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

## Service

- Served under the registry name `os.lazy.confd`; registered once.

## Scopes, deadlines and retries

| Method | Scope interface | Scope id | Deadline | Retry |
|---|---|---|---|---|
| Get | this interface | - | 2000 ms | safe |
| Set | this interface | - | 2000 ms | safe |
| Delete | this interface | - | 2000 ms | safe |
| List | this interface | - | 2000 ms | safe |
| Info | this interface | - | 2000 ms | safe |

## Errors

Carried in the reply's standard error field (id 15).

| Code | Name | Meaning |
|---|---|---|
| 1001 | NotFound | No value is stored at the path. |
| 1002 | BadPath | The path is not a valid confd path. |
| 1003 | TooLarge | The value or the store exceeds a confd size limit. |
| 1004 | Denied | The caller may not access that path. |
| 1005 | Io | The store could not be read or written; safe to retry. |
| 1006 | BadValue | The value is malformed (a `kind` whose payload is missing). |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/confd/changed/#` | `Change` | latest | no | `publish:system/confd/changed/#`, `subscribe:system/confd/changed/#` |
| `user/+/confd/changed/#` | `Change` | latest | no | `publish:user/+/confd/changed/#`, `subscribe:user/+/confd/changed/#` |

## struct `Value`

- `kind: Kind`
- `bool_value: Option<Bool>`
- `i64_value: Option<I64>`
- `u64_value: Option<U64>`
- `str_value: Option<String>`
- `bytes_value: Option<Bytes>`

## struct `Change`

- `path: String`
- `deleted: Bool`

## enum `Kind`

- Bool, I64, U64, Str, Bytes
