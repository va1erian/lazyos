# `sys::shell_tray_events`

The Rhai module for the Messenger interface `os.lazy.shell.tray.events.v1`. Wire-level reference: [os.lazy.shell.tray.events.v1](../../idl/os.lazy.shell.tray.events.v1.md).

What the shell sends an app about its item: oneway methods on the channel
the app transferred with `Set`.

## Functions

### `activate(anchor, popup)`

`Activate(anchor: Rect, popup: U64) -> () oneway`

Primary click. `anchor` is the icon in screen pixels; `popup` a
one-shot token for a flyout (0 when flyouts are unavailable).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `anchor` | `Rect` | map (`Rect`) |
| `popup` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::shell_tray_events::activate(anchor, popup);
```

### `secondary_activate(anchor)`

`SecondaryActivate(anchor: Rect) -> () oneway`

Secondary click on an item without a menu.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `anchor` | `Rect` | map (`Rect`) |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::shell_tray_events::secondary_activate(anchor);
```

### `menu_item(id, checked)`

`MenuItem(id: U32, checked: Bool) -> () oneway`

The user picked menu row `id`; `checked` is a check or radio row's
new state (the app sends `Update` to show it).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U32` | int |
| `checked` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::shell_tray_events::menu_item(id, checked);
```

### `scroll(delta)`

`Scroll(delta: I32) -> () oneway`

The wheel rolled `delta` notches over the icon (positive: up).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `delta` | `I32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::shell_tray_events::scroll(delta);
```

### `ping()`

`Ping() -> () oneway`

Liveness probe; nothing to answer.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::shell_tray_events::ping();
```

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
