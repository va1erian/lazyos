# `sys::input_shell`

The Rhai module for the Messenger interface `os.lazy.input.shell.v1`. Wire-level reference: [os.lazy.input.shell.v1](../../idl/os.lazy.input.shell.v1.md).

The compositor side of `inputd`. Only the compositor may call it: `inputd`
accepts these calls solely from the task that holds the display grant (the
kernel says who that is), so no client can move focus or register a window on
someone else's behalf. Everything is per kernel-stamped sender, never per request field.

Only `Attach` goes to the shared service endpoint. Every later call (and
the compositor's own `Open`, for its trusted prompt's surface) goes on the
channel `Attach` transferred, which no client holds: `inputd` serves it
ahead of the shared endpoint, so a client filling that queue cannot delay
a focus change (docs/accounts-plan.md U2). `Attach` on that channel is
refused (`EINVAL`).

## Functions

### `set_focus(surface)`

`SetFocus(surface: Option<U64>) -> ()`

Move keyboard focus to `surface` (absent: nobody is focused and no key
content is delivered). The previous holder gets `KeyboardLeave`, the new
one `KeyboardEnter`; repeat is cancelled.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `Option<U64>` | int or () |

Returns `()`.

```rhai
let result = sys::input_shell::set_focus(surface);
```

### `register_surface(surface, owner)`

`RegisterSurface(surface: U64, owner: U64) -> ()`

Declare that `owner` (a task slot, as the compositor stamps in
`Surface.owner`) created `surface`, so `inputd` can match that task's
`Open` to the window. `owner` 0 (never a client's slot) names the
compositor itself: a surface of its own, such as the trusted prompt's
(docs/accounts-plan.md U2), whose session only the compositor opens.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `owner` | `U64` | int |

Returns `()`.

```rhai
let result = sys::input_shell::register_surface(surface, owner);
```

### `unregister_surface(surface)`

`UnregisterSurface(surface: U64) -> ()`

Forget a destroyed surface; its sessions are closed.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |

Returns `()`.

```rhai
let result = sys::input_shell::unregister_surface(surface);
```

### `register_hotkey(code, mods)`

`RegisterHotkey(code: U32, mods: U32) -> (id: U64)`

Register a chord: `code` (HID usage) with exactly the Shift/Ctrl/Alt/
Super bits of `mods`. A match is consumed (its press and release are
never delivered to a client) and reported as `HotkeyFired`, except
while a session holds a keyboard grab. The reserved escape chord
(Escape with Ctrl+Alt) is refused with `EACCES`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `code` | `U32` | int |
| `mods` | `U32` | int |

Returns `id`: `U64` (int).

```rhai
let result = sys::input_shell::register_hotkey(code, mods);
```

### `unregister_hotkey(id)`

`UnregisterHotkey(id: U64) -> ()`

Drop a chord.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U64` | int |

Returns `()`.

```rhai
let result = sys::input_shell::unregister_hotkey(id);
```

### `approve_grant(session, allow)`

`ApproveGrant(session: U64, allow: Bool) -> ()`

Answer a `GrantRequested`. `allow` starts the grab, provided the
session still has keyboard focus (otherwise it is denied); either way
the client gets `GrantChanged`. `ENOENT` when no request of that
session is pending.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `session` | `U64` | int |
| `allow` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::input_shell::approve_grant(session, allow);
```

### `set_bounds(width, height)`

`SetBounds(width: U32, height: U32) -> ()`

The screen size the cursor is clamped to (each side `1..=16384`, else
`EINVAL`). `inputd` re-clamps the cursor at once and, if that moved it,
sends a `PointerEvent`. It replaces the kernel's own cursor bounds.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `width` | `U32` | int |
| `height` | `U32` | int |

Returns `()`.

```rhai
let result = sys::input_shell::set_bounds(width, height);
```

### `get_pointer()`

`GetPointer() -> (x: I32, y: I32, buttons: U32)`

The cursor position and held `buttons` (as in `PointerEvent`), to seed
the compositor's cursor after `Attach`.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `buttons` | `U32` | int |

```rhai
let result = sys::input_shell::get_pointer();
```

### `note_focus(surface)`

`NoteFocus(surface: Option<U64>) -> () oneway`

One-way `SetFocus`: the compositor's main loop never waits on `inputd`
(docs/performance-plan.md P3.6). Requests on the compositor's channel
are handled in the order sent, and before each client request, so a
surface noted before the compositor answers `CreateSurface` is known
by the time its client opens a session. Refused calls are dropped.
The trusted prompt never relies on a note: it waits for `SetFocus`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `Option<U64>` | int or () |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::note_focus(surface);
```

### `note_surface(surface, owner)`

`NoteSurface(surface: U64, owner: U64) -> () oneway`

One-way `RegisterSurface` (see `NoteFocus`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `owner` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::note_surface(surface, owner);
```

### `forget_surface(surface)`

`ForgetSurface(surface: U64) -> () oneway`

One-way `UnregisterSurface` (see `NoteFocus`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::forget_surface(surface);
```

### `note_input_done(seq)`

`NoteInputDone(seq: U64) -> () oneway`

One-way: the compositor has handled every `PointerEvent` up to `seq`
and already noted the focus they led to. A click can move focus, so
after forwarding a button press `inputd` holds the key content that
follows (in order, briefly: a compositor that never answers costs a
fraction of a second, not the keys) until this note covers the press;
otherwise a key typed right after a click would reach the window the
click left.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `seq` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::note_input_done(seq);
```

### `note_keys_held(held)`

`NoteKeysHeld(held: Bool) -> () oneway`

One-way: the shell's panel menu has the keyboard (`held`), or no
longer has it. Focus does not move, so the focused session sees no
`KeyboardLeave`/`KeyboardEnter`; its key content is held instead:
presses, repeats and text made while held never reach it, the release
of a key it saw go down before the hold is still delivered (nothing
sticks), a key pressed while held stays invisible to it until pressed
again, and its key-state page shows nothing held meanwhile
(`inputmap::hold`, issue #648).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `held` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::note_keys_held(held);
```

### `note_session_layout(layout)`

`NoteSessionLayout(layout: Option<String>) -> () oneway`

One-way: the logged-in user's own keyboard layout (`confd`'s
`user/<uid>/input/layout`, which `inputd` may not read), or absent to
follow the machine default `sys/input/layout` again (the login
screen, a logout). An unknown name also means the machine default.
It ends with the compositor (`inputmap::session_layout`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `layout` | `Option<String>` | string or () |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::note_session_layout(layout);
```

### `hotkey_fired(id)`

`HotkeyFired(id: U64) -> () oneway`

Shell event: a registered chord was pressed.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::hotkey_fired(id);
```

### `grant_requested(session, kind, surface)`

`GrantRequested(session: U64, kind: U32, surface: U64) -> () oneway`

Shell event: the client owning `surface` asked for a grab of `kind`
(a `GrantKind`) for `session`; answer with `ApproveGrant`. The
request is withdrawn if focus moves first.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `session` | `U64` | int |
| `kind` | `U32` | int |
| `surface` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::grant_requested(session, kind, surface);
```

### `escape_chord()`

`EscapeChord() -> () oneway`

Shell event: the reserved escape chord (Ctrl+Alt+Esc) was pressed. It
reverted any grab (`GrabChanged` follows when one was held) and is
never delivered to a client.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::escape_chord();
```

### `session_opened(surface)`

`SessionOpened(surface: U64) -> () oneway`

Shell event: `surface` now receives keys through an input session, so
the compositor must stop synthesising legacy `KeyDown`/`KeyUp` for it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::session_opened(surface);
```

### `session_closed(surface)`

`SessionClosed(surface: U64) -> () oneway`

Shell event: the last input session of `surface` ended; legacy key
delivery applies again.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::session_closed(surface);
```

### `pointer_event(x, y, buttons, wheel, wheel_h, ts_ns, seq)`

`PointerEvent(x: I32, y: I32, buttons: U32, wheel: I32, wheel_h: I32, ts_ns: U64, seq: U64) -> () oneway`

Shell event: the pointer changed. One cursor for every pointing device
(`docs/usb-hid-plan.md`): `x`, `y` are screen pixels inside the bounds,
`buttons` the bits held after this event (`1` left, `2` right,
`4` middle, `8` back, `0x10` forward; diff it for the edge), `wheel` and
`wheel_h` the notches since the previous event (positive is up / right).
Motion is coalesced; the position and wheel apply *before* the button
change. Only the compositor receives it: the window under the cursor is
its decision. `ts_ns`/`seq` are the newest raw record's.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `buttons` | `U32` | int |
| `wheel` | `I32` | int |
| `wheel_h` | `I32` | int |
| `ts_ns` | `U64` | int |
| `seq` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::pointer_event(x, y, buttons, wheel, wheel_h, ts_ns, seq);
```

### `grab_changed(surface)`

`GrabChanged(surface: Option<U64>) -> () oneway`

Shell event: `surface` now holds the keyboard grab (absent: nobody
does). While a grab is held the compositor must not act on its own
chords from the kernel key stream either: they belong to the grabber.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `Option<U64>` | int or () |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::grab_changed(surface);
```

### `key_activity()`

`KeyActivity() -> () oneway`

Shell event: a key went down on some keyboard (PS/2 or USB; sent after
`inputd` handled it, so a key held back while the shell asked for the
keys was already dropped). The kernel key stream only carries PS/2, so
this is how the compositor sees that someone is typing, for the idle
timer and to wake a blank screen.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::input_shell::key_activity();
```

Not callable from a script (the request carries a kernel object): `Attach`.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.input.shell.v1 on the wire](../../idl/os.lazy.input.shell.v1.md)
