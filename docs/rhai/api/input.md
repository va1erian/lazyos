# `sys::input`

The Rhai module for the Messenger interface `os.lazy.input.v1`. Wire-level reference: [os.lazy.input.v1](../../idl/os.lazy.input.v1.md).

The system input service (`inputd`; `docs/input-plan.md`).

Two interfaces, served on one endpoint and registered under both names:
`os.lazy.input.v1` for any client that wants keystrokes and
`os.lazy.input.shell.v1` for the compositor. Input is deliberately not part
of `os.lazy.display.v1`: display is about surfaces and buffers, input has its
own authorisation and versioning, and `display.v1`'s frozen `KeyDown`/`KeyUp`
(8/9) are synthesised by the compositor for legacy surfaces only.

**Keys and text are separate streams.** `KeyEvent` reports the *physical*
key (`code`, a USB HID usage from page 0x07, identical on every layout) plus
its meaning under the active layout (`sym`); it is what games, shortcuts and
terminals want. `TextInput` carries composed characters for text widgets and
is sent only for character-producing presses (never for Ctrl/Alt/Super
chords). Neither has to reverse-engineer the other.

Events go from `inputd` straight to the endpoint the client transferred in
`Open`, only while that client's surface has keyboard focus, and only to
that client.

Failures of calls are returned as a structured error field (id 15,
errno-style code plus friendly text) instead of the declared reply fields.

## Functions

### `close(session)`

`Close(session: U64) -> ()`

End a session. Only the task that opened it may.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `session` | `U64` | int |

Returns `()`.

```rhai
let result = sys::input::close(session);
```

### `get_state()`

`GetState() -> (layout: String, mods: U32, repeat_delay_ms: U32, repeat_interval_ms: U32)`

The current layout name, modifier and lock bits (`mods` as in
`KeyEvent`) and the key-repeat timing in milliseconds.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `layout` | `String` | string |
| `mods` | `U32` | int |
| `repeat_delay_ms` | `U32` | int |
| `repeat_interval_ms` | `U32` | int |

```rhai
let result = sys::input::get_state();
```

### `request_grant(session, kind)`

`RequestGrant(session: U64, kind: U32) -> ()`

Ask for a grab of kind `kind` (a `GrantKind`) for `session`, which
must be the caller's and have keyboard focus (`EACCES` otherwise;
`EINVAL` for an unknown kind). The compositor decides: the answer
arrives later as `GrantChanged`. A keyboard grab sends the session
every key, the compositor's own chords (Alt+Tab, Ctrl+Esc) included,
until the session releases it, loses focus or closes, or the user
presses the reserved escape chord (Ctrl+Alt+Esc), which nobody can
grab or register and which always reverts the grab.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `session` | `U64` | int |
| `kind` | `U32` | int |

Returns `()`.

```rhai
let result = sys::input::request_grant(session, kind);
```

### `release_grant(session)`

`ReleaseGrant(session: U64) -> ()`

Give up the grab (or a pending request) `session` holds. Only its
owner may; releasing nothing is not an error.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `session` | `U64` | int |

Returns `()`.

```rhai
let result = sys::input::release_grant(session);
```

### `ping(session, token)`

`Ping(session: U64, token: U64) -> (token: U64, seq: U64)`

Liveness and freshness for the caller's own `session`: echoes `token`
and returns `seq`, the raw sequence number of the newest key edge
`inputd` has processed *while that session has keyboard focus* (`0`
otherwise, so a session cannot watch typing aimed at another window),
so a client polling its key-state page can tell the page is current
(`KeyStateSeq >= seq`). `EACCES` for another task's session.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `session` | `U64` | int |
| `token` | `U64` | int |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `token` | `U64` | int |
| `seq` | `U64` | int |

```rhai
let result = sys::input::ping(session, token);
```

Not callable from a script (the request carries a kernel object): `Open`, `AttachKeyState`, `AttachGamepadState`.

## Types

### `KeyState`

A string; the constants of this module are:

- `KEY_STATE_DOWN` = `"Down"`
- `KEY_STATE_UP` = `"Up"`
- `KEY_STATE_REPEAT` = `"Repeat"`

`KEY_STATE` is the array of all of them.

### `GrantKind`

A string; the constants of this module are:

- `GRANT_KIND_NONE` = `"None"`
- `GRANT_KIND_KEYBOARD` = `"Keyboard"`

`GRANT_KIND` is the array of all of them.

### `GrantReason`

A string; the constants of this module are:

- `GRANT_REASON_APPROVED` = `"Approved"`
- `GRANT_REASON_DENIED` = `"Denied"`
- `GRANT_REASON_RELEASED` = `"Released"`
- `GRANT_REASON_FOCUS_LOST` = `"FocusLost"`
- `GRANT_REASON_ESCAPED` = `"Escaped"`
- `GRANT_REASON_CLOSED` = `"Closed"`

`GRANT_REASON` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.input.v1 on the wire](../../idl/os.lazy.input.v1.md)
