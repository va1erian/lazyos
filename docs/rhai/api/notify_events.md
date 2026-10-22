# `sys::notify_events`

The Rhai module for the Messenger interface `os.lazy.notify.events.v1`. Wire-level reference: [os.lazy.notify.events.v1](../../idl/os.lazy.notify.events.v1.md).

What the shell sends an app about its toasts: oneway methods on the
channel the app transferred with `Listen`.

## Functions

### `activated(id)`

`Activated(id: U64) -> () oneway`

The user clicked the toast's body.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::notify_events::activated(id);
```

### `action_invoked(id, action)`

`ActionInvoked(id: U64, action: U32) -> () oneway`

The user pressed action `action` of toast `id`; the toast closes.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U64` | int |
| `action` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::notify_events::action_invoked(id, action);
```

### `closed(id, reason)`

`Closed(id: U64, reason: U32) -> () oneway`

The toast is gone; `reason` is a `CloseReason` value. It is sent once
per toast, also for one that was never shown (`Muted`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U64` | int |
| `reason` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::notify_events::closed(id, reason);
```

### `ping()`

`Ping() -> () oneway`

Liveness probe; nothing to answer.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::notify_events::ping();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.notify.events.v1 on the wire](../../idl/os.lazy.notify.events.v1.md)
