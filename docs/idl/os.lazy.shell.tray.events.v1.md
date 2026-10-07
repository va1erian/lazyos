# `os.lazy.shell.tray.events.v1`

Interface id: `0x7dc550e02c4d9bf`

What the shell sends an app about its item: oneway methods on the channel
the app transferred with `Set`.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Activate | 1 | oneway | `(anchor: Rect, popup: U64) -> ()` |
| SecondaryActivate | 2 | oneway | `(anchor: Rect) -> ()` |
| MenuItem | 3 | oneway | `(id: U32, checked: Bool) -> ()` |
| Scroll | 4 | oneway | `(delta: I32) -> ()` |
| Ping | 5 | oneway | `() -> ()` |

## struct `Rect`

- `x: I32`
- `y: I32`
- `w: U32`
- `h: U32`
