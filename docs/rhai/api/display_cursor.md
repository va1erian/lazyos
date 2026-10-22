# `sys::display_cursor`

The Rhai module for the Messenger interface `os.lazy.display.cursor.v1`. Wire-level reference: [os.lazy.display.cursor.v1](../../idl/os.lazy.display.cursor.v1.md).

The mouse pointer's shape over an app's own windows (`docs/cursor-plan.md`).

`xuid` serves this interface on the display service (`os.lazy.display.v1`,
the endpoint every display client already holds) and is also the ACL scope
it travels on: the parcel's interface id is this interface's, so the
kernel's label rules decide whether an app may change the pointer before
the compositor sees the call. A package asks for it by listing
`os.lazy.display.cursor.v1` in its manifest's `interfaces`; it is not
implied by `os.lazy.display.v1`. Unlabelled tasks (the Terminal, system
services) pass by the usual uid rules.

The compositor itself picks the arrow over the title bar, a resize shape
over a resizable window's border and while a resize runs, and the shape the
surface asked for over its content. Failures are returned as a structured
error field (id 15) like the display interface's.

## Functions

### `set_cursor(surface, shape)`

`SetCursor(surface: U64, shape: CursorShape) -> ()`

Set the shape shown while the pointer is over `surface`'s content. The
caller must own the surface (`EACCES` otherwise); an unknown surface is
`ENOENT` and a `shape` that is not a `CursorShape` value is `EINVAL`.
The shape stays until the next call or until the surface goes away;
`Default` restores the arrow.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `shape` | `CursorShape` | string (`CursorShape` variant) |

Returns `()`.

```rhai
let result = sys::display_cursor::set_cursor(surface, shape);
```

## Types

### `CursorShape`

A string; the constants of this module are:

- `CURSOR_SHAPE_DEFAULT` = `"Default"`
- `CURSOR_SHAPE_BUSY` = `"Busy"`
- `CURSOR_SHAPE_TEXT` = `"Text"`
- `CURSOR_SHAPE_HAND` = `"Hand"`
- `CURSOR_SHAPE_RESIZE_H` = `"ResizeH"`
- `CURSOR_SHAPE_RESIZE_V` = `"ResizeV"`
- `CURSOR_SHAPE_RESIZE_NWSE` = `"ResizeNwse"`
- `CURSOR_SHAPE_RESIZE_NESW` = `"ResizeNesw"`

`CURSOR_SHAPE` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.display.cursor.v1 on the wire](../../idl/os.lazy.display.cursor.v1.md)
