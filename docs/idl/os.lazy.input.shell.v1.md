# `os.lazy.input.shell.v1`

Interface id: `0xc258ed5b9b5debfe`

The compositor side of `inputd`. Only the compositor may call it: `inputd`
accepts these calls solely from the task that holds the display grant (the
kernel says who that is), so no client can move focus or register a window on
someone else's behalf. Everything is per kernel-stamped sender, never per request field.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Attach | 1 | sync | `() -> () transfers (events: Channel<os.lazy.input.shell.v1>)` |
| SetFocus | 2 | sync | `(surface: Option<U64>) -> ()` |
| RegisterSurface | 3 | sync | `(surface: U64, owner: U64) -> ()` |
| UnregisterSurface | 4 | sync | `(surface: U64) -> ()` |
| RegisterHotkey | 5 | sync | `(code: U32, mods: U32) -> (id: U64)` |
| UnregisterHotkey | 6 | sync | `(id: U64) -> ()` |
| ApproveGrant | 7 | sync | `(session: U64, allow: Bool) -> ()` |
| SetBounds | 8 | sync | `(width: U32, height: U32) -> ()` |
| GetPointer | 9 | sync | `() -> (x: I32, y: I32, buttons: U32)` |
| NoteFocus | 10 | oneway | `(surface: Option<U64>) -> ()` |
| NoteSurface | 11 | oneway | `(surface: U64, owner: U64) -> ()` |
| ForgetSurface | 12 | oneway | `(surface: U64) -> ()` |
| HotkeyFired | 20 | oneway | `(id: U64) -> ()` |
| GrantRequested | 21 | oneway | `(session: U64, kind: U32, surface: U64) -> ()` |
| EscapeChord | 22 | oneway | `() -> ()` |
| SessionOpened | 23 | oneway | `(surface: U64) -> ()` |
| SessionClosed | 24 | oneway | `(surface: U64) -> ()` |
| PointerEvent | 25 | oneway | `(x: I32, y: I32, buttons: U32, wheel: I32, wheel_h: I32, ts_ns: U64, seq: U64) -> ()` |
| GrabChanged | 26 | oneway | `(surface: Option<U64>) -> ()` |

## Transfers

Objects a request carries outside its body, in the parcel's
`handles` and `buffers` vectors.

| Method | Name | Slot |
|---|---|---|
| Attach | `events` | `handles[0]`, a channel the receiver sends `os.lazy.input.shell.v1` on |
