# `sys::echo`

The Rhai module for the Messenger interface `os.lazy.echo.v1`. Wire-level reference: [os.lazy.echo.v1](../../idl/os.lazy.echo.v1.md).

A tiny demo service: echo whatever you send (issue #90 sample IDL).

## Functions

### `echo(text, count)`

`Echo(text: String, count: U32) -> (reply: String)`

Reply with `text` and the number of bytes received.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `text` | `String` | string |
| `count` | `U32` | int |

Returns `reply`: `String` (string).

```rhai
let result = sys::echo::echo(text, count);
```

### `ping()`

`Ping() -> (alive: Bool)`

Health probe.

Returns `alive`: `Bool` (bool).

```rhai
let result = sys::echo::ping();
```

### `notify(event)`

`Notify(event: Event) -> () oneway`

Broadcast an event; no reply is expected.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `event` | `Event` | map (`Event`) |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::echo::notify(event);
```

## Types

### `Event`

A timestamped notice.

| Name | IDL type | Script value |
|---|---|---|
| `topic` | `String` | string |
| `at` | `U64` | int |

```rhai
let value = sys::echo::new_event();
```

### `Level`

A string; the constants of this module are:

- `LEVEL_INFO` = `"Info"`
- `LEVEL_WARN` = `"Warn"`
- `LEVEL_ERROR` = `"Error"`

`LEVEL` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.echo.v1 on the wire](../../idl/os.lazy.echo.v1.md)
