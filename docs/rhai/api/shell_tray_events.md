# `sys::shell_tray_events`

The Rhai module for the Messenger interface `os.lazy.shell.tray.events.v1`. Wire-level reference: [os.lazy.shell.tray.events.v1](../../idl/os.lazy.shell.tray.events.v1.md).

What the shell sends an app about its item: `event` methods on the channel
the app transferred with `Set`.

## Types

### `Rect`

A rectangle in screen pixels.

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `w` | `U32` | int |
| `h` | `U32` | int |

```rhai
let value = sys::shell_tray_events::new_rect();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.shell.tray.events.v1 on the wire](../../idl/os.lazy.shell.tray.events.v1.md)
