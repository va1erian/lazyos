# `os.lazy.input.v1`

Interface id: `0x5026bd54a60f1ff6`

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

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Open | 1 | sync | `(surface: Option<U64>) -> (session: U64) transfers (events: Channel<os.lazy.input.v1>)` |
| Close | 2 | sync | `(session: U64) -> ()` |
| GetState | 3 | sync | `() -> (layout: String, mods: U32, repeat_delay_ms: U32, repeat_interval_ms: U32)` |
| RequestGrant | 4 | sync | `(session: U64, kind: U32) -> ()` |
| ReleaseGrant | 5 | sync | `(session: U64) -> ()` |
| Ping | 6 | sync | `(session: U64, token: U64) -> (token: U64, seq: U64)` |
| AttachKeyState | 7 | sync | `(session: U64) -> () transfers (state: Buffer)` |
| KeyEvent | 10 | oneway | `(code: U32, sym: U32, mods: U32, state: U32, ts_ns: U64, seq: U64) -> ()` |
| TextInput | 11 | oneway | `(utf8: String) -> ()` |
| KeyboardEnter | 12 | oneway | `(down: Array<U32>) -> ()` |
| KeyboardLeave | 13 | oneway | `() -> ()` |
| LayoutChanged | 14 | oneway | `(layout: String) -> ()` |
| GrantChanged | 15 | oneway | `(kind: U32, active: Bool, reason: U32) -> ()` |

## Transfers

Objects a request carries outside its body, in the parcel's
`handles` and `buffers` vectors.

| Method | Name | Slot |
|---|---|---|
| Open | `events` | `handles[0]`, a channel the receiver sends `os.lazy.input.v1` on |
| AttachKeyState | `state` | `buffers[0]`, a shared buffer |

## enum `KeyState`

- Down, Up, Repeat

## enum `GrantKind`

- None, Keyboard

## enum `GrantReason`

- Approved, Denied, Released, FocusLost, Escaped, Closed
