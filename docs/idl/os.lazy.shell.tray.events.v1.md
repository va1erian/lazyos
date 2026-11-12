# `os.lazy.shell.tray.events.v1`

Interface id: `0x7dc550e02c4d9bf`

What the shell sends an app about its item: `event` methods on the channel
the app transferred with `Set`.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Activate | 1 | event | `(anchor: Rect, popup: U64) -> ()` |
| SecondaryActivate | 2 | event | `(anchor: Rect) -> ()` |
| MenuItem | 3 | event | `(id: U32, checked: Bool) -> ()` |
| Scroll | 4 | event | `(delta: I32) -> ()` |
| Ping | 5 | event | `() -> ()` |

## struct `Rect`

- `x: I32`
- `y: I32`
- `w: U32`
- `h: U32`
