# `sys::messenger_registry`

The Rhai module for the Messenger interface `os.lazy.messenger.registry.v1`. Wire-level reference: [os.lazy.messenger.registry.v1](../../idl/os.lazy.messenger.registry.v1.md).

The Messenger service name registry (issues #89, #300).

Names are published by tasks and resolved into endpoint handles. Two paths
reach the same kernel table: the native `register`/`resolve`/`unregister`/
`list` gate ops (which take these parcels as their request and, for `List`,
fill the reply into the caller's buffer) and the `messengerd` daemon, which
forwards a client's request on its behalf over the bootstrap channel. The
kernel publishes `os.lazy.messenger.registry` itself at boot, so it is the
one name every task can resolve.

A daemon failure is answered with the standard error field (`docs/midl.md`)
(id 15, which no reply field may take) instead of a typed reply; the
kernel gate reports failures as negative errno values.

## Functions

### `register(name, endpoint, interfaces, lease_ticks, interface_names)`

`Register(name: String, endpoint: Option<U64>, interfaces: Array<U64>, lease_ticks: U64, interface_names: Array<String>) -> ()`

Publish `endpoint` (a handle in the owner's table) under `name`. The
owner becomes the registering task. A `lease_ticks` of `0` registers a
permanent name; otherwise the name expires after that many ticks.
`interface_names` spells out `interfaces`, in order (issue #495): an
id is the FNV-1a 64 hash of its name, so the kernel cannot tell from
an id alone which domain an interface belongs to. When present it must
match `interfaces` one for one, hash for hash. A task labelled
`app:<id>` (or `dev:<id>`) must send it, and every name must lie in its
own domain, `<id>.<name>.v<N>`; anything else is refused (`EACCES`,
audited). Other callers may leave it empty.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `endpoint` | `Option<U64>` | int or () |
| `interfaces` | `Array<U64>` | array of int |
| `lease_ticks` | `U64` | int |
| `interface_names` | `Array<String>` | array of string |

Returns `()`.

```rhai
let result = sys::messenger_registry::register(name, endpoint, interfaces, lease_ticks, interface_names);
```

### `resolve(name)`

`Resolve(name: String) -> (handle: U64)`

Look `name` up. Over the gate the new handle is the call's return value;
over the daemon it is `handle`, already open in the caller's table.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |

Returns `handle`: `U64` (int).

```rhai
let result = sys::messenger_registry::resolve(name);
```

### `unregister(name)`

`Unregister(name: String) -> ()`

Withdraw `name`. Only its owner (or an administrator) may.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |

Returns `()`.

```rhai
let result = sys::messenger_registry::unregister(name);
```

### `list()`

`List() -> (entries: Array<Entry>)`

Snapshot the name table.

Returns `entries`: `Array<Entry>` (array of map (`Entry`)).

```rhai
let result = sys::messenger_registry::list();
```

Not callable from a script (the request carries a kernel object): `Connected`.

## Types

### `Entry`

One registered name.

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `object` | `U64` | int |
| `owner` | `U64` | int |
| `interfaces` | `Array<U64>` | array of int |
| `lease_remaining` | `U64` | int |
| `owner_uid` | `U64` | int |
| `owner_label` | `U64` | int |
| `owner_session` | `U64` | int |

```rhai
let value = sys::messenger_registry::new_entry();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.messenger.registry.v1 on the wire](../../idl/os.lazy.messenger.registry.v1.md)
