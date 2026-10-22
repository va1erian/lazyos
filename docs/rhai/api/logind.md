# `sys::logind`

The Rhai module for the Messenger interface `os.lazy.logind.v1`. Wire-level reference: [os.lazy.logind.v1](../../idl/os.lazy.logind.v1.md).

The console login service (issue #101): owns the session table and
answers the query `messengerctl sessions` renders.

## Functions

### `sessions()`

`Sessions() -> (active: U64, sessions: Array<Session>)`

Snapshot the session table, oldest session first.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `active` | `U64` | int |
| `sessions` | `Array<Session>` | array of map (`Session`) |

```rhai
let result = sys::logind::sessions();
```

### `login(user, secret)`

`Login(user: String, secret: String) -> (session: U64)`

Log in from the graphical login screen (issue #623): authenticate
`user` with `secret` and open their desktop session, the same path a
built-in autologin takes. Only the login screen's own identity (the
`_greeter` system uid, unlabelled) may call it; a refused password is
`EACCES` after the failed-login delay, a session already open `EBUSY`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `user` | `String` | string |
| `secret` | `String` | string |

Returns `session`: `U64` (int).

```rhai
let result = sys::logind::login(user, secret);
```

### `logout()`

`Logout() -> (session: U64)`

End the caller's graphical session (issue #623): `init` stops every
task stamped with that session, and the login screen comes back. Only
a task of the active graphical session may call it (`EPERM`
otherwise).

Returns `session`: `U64` (int).

```rhai
let result = sys::logind::logout();
```

## Types

### `Session`

One login session.

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U64` | int |
| `user` | `String` | string |
| `uid` | `U32` | int |
| `pid` | `U64` | int |
| `state` | `String` | string |
| `started` | `U64` | int |

```rhai
let value = sys::logind::new_session();
```

### `LoginStart`

Session id minted by `logind`.
Account name.
User id stamped on the session.
Task slot of the session's shell (`0` until spawned).
`active` while the shell runs, `exited` after it is reaped.
Tick the session started.
A session began (issue #307): the payload of `system/events/login/start`.

| Name | IDL type | Script value |
|---|---|---|
| `user` | `String` | string |
| `uid` | `U32` | int |
| `session` | `U64` | int |
| `pid` | `U64` | int |
| `state` | `String` | string |

```rhai
let value = sys::logind::new_login_start();
```

### `LoginSession`

Account name.
User id stamped on the session.
Session id minted by `logind`.
Task slot of the shell.
Session state (`active`).
One session's state (issue #307): the payload of
`system/events/login/session/<id>`.

| Name | IDL type | Script value |
|---|---|---|
| `user` | `String` | string |
| `uid` | `U32` | int |
| `pid` | `U64` | int |
| `state` | `String` | string |
| `home` | `String` | string |

```rhai
let value = sys::logind::new_login_session();
```

### `LoginDenied`

Account name.
User id stamped on the session.
Task slot of the shell.
Session state (`active`/`exited`).
The account's home directory (issue #508): `init` gives the
session's apps `HOME` from it, so it never looks the account up
again per launch.
A refused attempt (issue #307): the payload of
`system/events/login/denied`. The reason is a short, non-secret word
(`unknown-user`, `bad-secret`, `spawn-failed`), never the secret.

| Name | IDL type | Script value |
|---|---|---|
| `user` | `String` | string |
| `reason` | `String` | string |

```rhai
let value = sys::logind::new_login_denied();
```

### `LoginEnd`

Account name offered.
Why the attempt was refused.
A session ended (issue #307): the payload of `system/events/login/end`.

| Name | IDL type | Script value |
|---|---|---|
| `user` | `String` | string |
| `uid` | `U32` | int |
| `session` | `U64` | int |
| `status` | `U64` | int |

```rhai
let value = sys::logind::new_login_end();
```

## Topics

### `system/events/login/start`

Account name.
User id stamped on the session.
Session id minted by `logind`.
Exit status of the shell.
The login lifecycle topics (issue #307). `logind` publishes all four
retained, matching its current call sites, so a late `logd` still sees
the latest session state.

Payload `LoginStart`, QoS `latest`, retained.

Helpers: `START_PATTERN`, `on_start(handler)`, `subscribe_start()`, `publish_start(payload)`.

```rhai
sys::logind::on_start(|event| print(event.payload));
```

### `system/events/login/session/{id}`

One retained event per session id, so a late subscriber sees the
newest state of each live session.

Payload `LoginSession`, QoS `latest`, retained.

Helpers: `SESSION_PATTERN`, `session_topic(id)`, `on_session(id, handler)`, `subscribe_session(id)`, `publish_session(id, payload)`.

```rhai
sys::logind::on_session(id, |event| print(event.payload));
```

### `system/events/login/denied`

The most recent refusal (retained, as published today).

Payload `LoginDenied`, QoS `latest`, retained.

Helpers: `DENIED_PATTERN`, `on_denied(handler)`, `subscribe_denied()`, `publish_denied(payload)`.

```rhai
sys::logind::on_denied(|event| print(event.payload));
```

### `system/events/login/end`

The most recent logout (retained, as published today).

Payload `LoginEnd`, QoS `latest`, retained.

Helpers: `END_PATTERN`, `on_end(handler)`, `subscribe_end()`, `publish_end(payload)`.

```rhai
sys::logind::on_end(|event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.logind.v1 on the wire](../../idl/os.lazy.logind.v1.md)
