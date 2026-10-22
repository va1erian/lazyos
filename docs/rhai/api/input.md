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

### `key_event(code, sym, mods, state, ts_ns, seq)`

`KeyEvent(code: U32, sym: U32, mods: U32, state: U32, ts_ns: U64, seq: U64) -> () oneway`

Event: a key changed state. `code` is the physical key, `sym` its
keysym under the active layout (a Unicode scalar for character keys,
an X11-style `0xFFxx` value otherwise, `0` when the key has no meaning
on the current level), `mods` the modifier and lock bits in force after
this event (`1` Shift, `2` Ctrl, `4` Alt, `8` Super, `0x10` AltGr,
`0x20` CapsLock, `0x40` NumLock, `0x80` ScrollLock), `state` a
`KeyState`, `ts_ns` the kernel timestamp and `seq` the raw sequence
number. Auto-repeat is always flagged `Repeat`, never `Down`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `code` | `U32` | int |
| `sym` | `U32` | int |
| `mods` | `U32` | int |
| `state` | `U32` | int |
| `ts_ns` | `U64` | int |
| `seq` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input::key_event(code, sym, mods, state, ts_ns, seq);
```

### `text_input(utf8)`

`TextInput(utf8: String) -> () oneway`

Event: composed text (UTF-8) for a character-producing press or repeat.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `utf8` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input::text_input(utf8);
```

### `keyboard_enter(down)`

`KeyboardEnter(down: Array<U32>) -> () oneway`

Event: keyboard focus arrived. `down` lists the physical keys held right
now, so the client seeds its key state.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `down` | `Array<U32>` | array of int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input::keyboard_enter(down);
```

### `keyboard_leave()`

`KeyboardLeave() -> () oneway`

Event: keyboard focus left. The client **must release every key** it
believes is down; `inputd` also cancels repeat.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input::keyboard_leave();
```

### `layout_changed(layout)`

`LayoutChanged(layout: String) -> () oneway`

Event: the layout changed (a `confd` write, live).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `layout` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input::layout_changed(layout);
```

### `grant_changed(kind, active, reason)`

`GrantChanged(kind: U32, active: Bool, reason: U32) -> () oneway`

Event: the session's grab of kind `kind` started (`active`) or ended;
`reason` is a `GrantReason`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `kind` | `U32` | int |
| `active` | `Bool` | bool |
| `reason` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input::grant_changed(kind, active, reason);
```

### `gamepad_axis(device, code, value, ts_ns, seq)`

`GamepadAxis(device: U32, code: U32, value: I32, ts_ns: U64, seq: U64) -> () oneway`

Event: a gamepad axis changed (only the focused session that attached
a pad page receives it).
`device` is the pad's id (as in the state page), `code` the Generic
Desktop usage (`0x30` X, `0x31` Y, `0x32` Z, `0x33` Rx, `0x34` Ry,
`0x35` Rz, `0x36` Slider, `0x37` Dial), `value` `-32768..=32767` with
`0` the axis's rest, the deadzone already applied (Y and Ry are
positive down). The hat is not an axis event: it arrives as the
d-pad buttons (`GamepadButton` codes 33..=36). Changes are coalesced
per drain: one event per axis, the newest value.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `device` | `U32` | int |
| `code` | `U32` | int |
| `value` | `I32` | int |
| `ts_ns` | `U64` | int |
| `seq` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input::gamepad_axis(device, code, value, ts_ns, seq);
```

### `gamepad_button(device, code, pressed, ts_ns, seq)`

`GamepadButton(device: U32, code: U32, pressed: Bool, ts_ns: U64, seq: U64) -> () oneway`

Event: a gamepad button changed (only the focused session that
attached a pad page receives it). `code` is the HID button usage `1..=32`, or `33` d-pad up, `34`
down, `35` left, `36` right (the hat, one edge per direction;
diagonals press two). Edges are never merged.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `device` | `U32` | int |
| `code` | `U32` | int |
| `pressed` | `Bool` | bool |
| `ts_ns` | `U64` | int |
| `seq` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input::gamepad_button(device, code, pressed, ts_ns, seq);
```

### `gamepad_enter()`

`GamepadEnter() -> () oneway`

Event: gamepad input arrived with this session's focus: read the
state page to seed. Sent to a session that attached a pad page
whenever keyboard focus enters it, when it attaches one while it
already has focus, and right after a `GamepadLeave` that was a resync.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input::gamepad_enter();
```

### `gamepad_leave()`

`GamepadLeave() -> () oneway`

Event: focus left, or events were lost. The client **must release
every pad button** it believes is down and centre the axes; the page
was cleared first. After a dropped-events resync (the keyboard's
`KeyboardLeave` + `KeyboardEnter` pair) it is followed at once by a
`GamepadEnter` with the page live again; the page is cleared before
the Leave and restored before the Enter either way.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input::gamepad_leave();
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
