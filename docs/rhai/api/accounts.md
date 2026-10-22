# `sys::accounts`

The Rhai module for the Messenger interface `os.lazy.accounts.v1`. Wire-level reference: [os.lazy.accounts.v1](../../idl/os.lazy.accounts.v1.md).

The account database service (issues #101, #624; docs/accounts-plan.md
U1). `accountsd` runs as the `_accounts` system uid and owns
`/accounts/db`; `/system/etc/passwd` and `/system/etc/group` are views
of it. Lookups and `ListUsers` are open to any caller; `Authenticate` is
open but slowed (`EAGAIN` while a brake holds): a caller's failures count
against its own uid, and against the account name only when `logind` or
`elevd` asked, so no session can lock another account out. Creating, deleting and promoting accounts, and setting another
user's password, are accepted from `elevd` alone (after an administrator
approved them on the trusted prompt, U2), never from a uid or a
capability; the login screen may create the machine's first account during
the first-boot setup. Failures are a structured error field (errno-style
code, friendly text).

## Functions

### `lookup(name, uid)`

`Lookup(name: Option<String>, uid: Option<U32>) -> (found: Bool, user: Option<User>)`

Find a user by `name` or, when `name` is absent, by `uid`.
`found` is false (and `user` empty) when no account matches.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `Option<String>` | string or () |
| `uid` | `Option<U32>` | int or () |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `found` | `Bool` | bool |
| `user` | `Option<User>` | map (`User`) or () |

```rhai
let result = sys::accounts::lookup(name, uid);
```

### `authenticate(name, secret)`

`Authenticate(name: String, secret: String) -> (ok: Bool)`

Verify `secret` against the stored verifier of `name`. After three
failures in a row for a name or from a caller, further attempts are
refused with `EAGAIN` for a delay that doubles from 1 s to 60 s.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `secret` | `String` | string |

Returns `ok`: `Bool` (bool).

```rhai
let result = sys::accounts::authenticate(name, secret);
```

### `create(name, secret, admin)`

`Create(name: String, secret: String, admin: Bool) -> (user: User)`

Create the account `name` with the password `secret` (an
administrator when `admin`): the next free uid, a 0700 home made from
the skeleton. From `elevd`, or from the login screen while the machine
has no account (the owner, who must be an admin). `EEXIST` for a taken
name, `EINVAL` for a bad one, `EPERM` for any other caller.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `secret` | `String` | string |
| `admin` | `Bool` | bool |

Returns `user`: `User` (map (`User`)).

```rhai
let result = sys::accounts::create(name, secret, admin);
```

### `delete(name, home)`

`Delete(name: String, home: String) -> ()`

Delete the account `name` (from `elevd` only). `home` says what
happens to its home: `keep`, `archive` (moved aside, root-only) or
`remove`. The last administrator cannot be deleted (`EBUSY`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `home` | `String` | string |

Returns `()`.

```rhai
let result = sys::accounts::delete(name, home);
```

### `set_password(name, old, secret)`

`SetPassword(name: String, old: Option<String>, secret: String) -> ()`

Set `name`'s password to `secret`. A user may change their own with
the current one in `old` (checked and slowed like `Authenticate`,
`EACCES` when wrong); `elevd` may set anyone's without it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `old` | `Option<String>` | string or () |
| `secret` | `String` | string |

Returns `()`.

```rhai
let result = sys::accounts::set_password(name, old, secret);
```

### `set_admin(name, admin)`

`SetAdmin(name: String, admin: Bool) -> ()`

Make `name` an administrator or not (from `elevd` only). Taking it
from the last administrator is `EBUSY`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `admin` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::accounts::set_admin(name, admin);
```

### `list_users()`

`ListUsers() -> (users: Array<User>, setup: Bool)`

Every account, in database order. `setup` is true while the machine
has no account (the login screen then asks for its owner).

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `users` | `Array<User>` | array of map (`User`) |
| `setup` | `Bool` | bool |

```rhai
let result = sys::accounts::list_users();
```

## Types

### `User`

One account record as a lookup returns it (never the secret).

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `uid` | `U32` | int |
| `gid` | `U32` | int |
| `home` | `String` | string |
| `shell` | `String` | string |
| `admin` | `Bool` | bool |

```rhai
let value = sys::accounts::new_user();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.accounts.v1 on the wire](../../idl/os.lazy.accounts.v1.md)
