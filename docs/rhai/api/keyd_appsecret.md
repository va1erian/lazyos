# `sys::keyd_appsecret`

The Rhai module for the Messenger interface `os.lazy.keyd.appsecret.v1`. Wire-level reference: [os.lazy.keyd.appsecret.v1](../../idl/os.lazy.keyd.appsecret.v1.md).

An app's own passwords and keys, kept by `keyd` across reboots
(docs/ai-agent-plan.md section 5.3 "P2b", docs/security-model.md section 8).

`keyd` serves this interface on its own endpoint (also registered as
`os.lazy.keyd.appsecret`) and it is also the ACL scope it travels on: a
package asks for it by listing `os.lazy.keyd.appsecret.v1` in its
manifest's `interfaces`. It is rated MEDIUM: an app can keep and read back
only its own secrets, never another app's or the system's.

**Scope.** Every secret belongs to the caller's kernel-stamped uid and
installed-app label (`app:<system_name>`), never to a name in the request.
An unlabelled caller is refused (`EPERM`), and so is a development run
(`dev:<name>`) or a helper process (`app:<name>/<child>`): they have other
labels, so a project under development or a sandboxed child never sees the
installed app's secrets. Another user running the same app has its own.

**Limits.** A `name` is 1..=64 bytes of printable ASCII (`EINVAL`
otherwise); a `secret` is 1..=1024 bytes (`EINVAL`); an app holds at most
16 per user (`ENOSPC` past that, and when the table is full). `EIO` means
the change could not be made durable and nothing changed. `ENOENT` is no
such secret. Secrets persist across reboots, sealed under the machine key;
`keyd` deletes the secrets of an app that is no longer installed. No
secret is ever written to a log or an error text. Failures are returned as
a structured error field (errno-style code, friendly text).

## Functions

### `put(name, secret)`

`Put(name: String, secret: Bytes) -> ()`

Keep `secret` under `name` for the calling app and user, replacing one
of that name.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `secret` | `Bytes` | blob |

Returns `()`.

```rhai
let result = sys::keyd_appsecret::put(name, secret);
```

### `get(name)`

`Get(name: String) -> (secret: Bytes)`

The calling app's secret `name` (`ENOENT` when there is none).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |

Returns `secret`: `Bytes` (blob).

```rhai
let result = sys::keyd_appsecret::get(name);
```

### `delete(name)`

`Delete(name: String) -> ()`

Forget the calling app's secret `name` (`ENOENT` when there is none).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |

Returns `()`.

```rhai
let result = sys::keyd_appsecret::delete(name);
```

### `list()`

`List() -> (names: Array<String>)`

The names of the calling app's secrets, sorted. Names only.

Returns `names`: `Array<String>` (array of string).

```rhai
let result = sys::keyd_appsecret::list();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.keyd.appsecret.v1 on the wire](../../idl/os.lazy.keyd.appsecret.v1.md)
