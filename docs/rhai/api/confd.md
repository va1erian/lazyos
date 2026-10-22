# `sys::confd`

The Rhai module for the Messenger interface `os.lazy.confd.v1`. Wire-level reference: [os.lazy.confd.v1](../../idl/os.lazy.confd.v1.md).

The hierarchical configuration registry (issue #260).

The store is a tree of one typed value per path; reachable only through
Messenger, and every call is checked against the kernel-stamped caller uid.
Change notifications are best-effort: a subscriber observes
`(path, deleted)` and must re-read the value.

## Functions

### `get(path)`

`Get(path: String) -> (value: Option<Value>)`

Read the value at `path`. An absent path has an empty `value`.
A path the caller may not read fails with `CONFD_DENIED`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `path` | `String` | string |

Returns `value`: `Option<Value>` (map (`Value`) or ()).

```rhai
let result = sys::confd::get(path);
```

### `set(path, value)`

`Set(path: String, value: Value) -> ()`

Create or overwrite `path`; the change is persisted before the reply.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `path` | `String` | string |
| `value` | `Value` | map (`Value`) |

Returns `()`.

```rhai
let result = sys::confd::set(path, value);
```

### `delete(path)`

`Delete(path: String) -> ()`

Remove `path`; deleting an absent path succeeds.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `path` | `String` | string |

Returns `()`.

```rhai
let result = sys::confd::delete(path);
```

### `list(prefix)`

`List(prefix: String) -> (paths: Array<String>)`

Every path the caller may read under `prefix`, in sorted order.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `prefix` | `String` | string |

Returns `paths`: `Array<String>` (array of string).

```rhai
let result = sys::confd::list(prefix);
```

### `info()`

`Info() -> (store_dir: String, persistent: Bool)`

Where the store lives: the directory confd chose and whether it
survives a reboot (`/conf`; `false` when it fell back to the ramfs
`/transient/conf`).
Not restricted by path, so any caller may ask.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `store_dir` | `String` | string |
| `persistent` | `Bool` | bool |

```rhai
let result = sys::confd::info();
```

## Types

### `Value`

One typed value. `kind` selects which optional payload field is set:
`0` bool, `1` i64, `2` u64, `3` string, `4` bytes.

| Name | IDL type | Script value |
|---|---|---|
| `kind` | `U32` | int |
| `bool_value` | `Option<Bool>` | bool or () |
| `i64_value` | `Option<I64>` | int or () |
| `u64_value` | `Option<U64>` | int or () |
| `str_value` | `Option<String>` | string or () |
| `bytes_value` | `Option<Bytes>` | blob or () |

```rhai
let value = sys::confd::new_value();
```

### `Change`

One committed change (issue #307): the payload of the
`system/confd/changed/<path>` topic. It deliberately never carries the
value; a subscriber re-reads the path.

| Name | IDL type | Script value |
|---|---|---|
| `path` | `String` | string |
| `deleted` | `Bool` | bool |

```rhai
let value = sys::confd::new_change();
```

## Topics

### `system/confd/changed/{path...}`

Announced for every committed `sys/` change (issue #260): the topic is
`system/confd/changed/<path>`, where `<path>` is the changed path, so a
subscriber watches a subtree with `system/confd/changed/sys/#`. Not
retained: a change is an event, not state.

Payload `Change`, QoS `latest`.

Helpers: `CHANGED_PATTERN`, `changed_topic(path)`, `on_changed(path, handler)`, `subscribe_changed(path)`, `publish_changed(path, payload)`.

```rhai
sys::confd::on_changed(path, |event| print(event.payload));
```

### `user/{uid}/confd/changed/{path...}`

Announced for every committed `user/<uid>/<path>` change (issue #407):
the topic is `user/<uid>/confd/changed/<path>`, in the kernel's per-uid
topic namespace, so only that uid and root may subscribe (as only they
may read the key). The payload's `path` is the full key. Not retained.

Payload `Change`, QoS `latest`.

Helpers: `USER_CHANGED_PATTERN`, `user_changed_topic(uid, path)`, `on_user_changed(uid, path, handler)`, `subscribe_user_changed(uid, path)`, `publish_user_changed(uid, path, payload)`.

```rhai
sys::confd::on_user_changed(uid, path, |event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.confd.v1 on the wire](../../idl/os.lazy.confd.v1.md)
