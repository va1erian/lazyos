# `sys::init_app_events`

The Rhai module for the Messenger interface `os.lazy.init.app.events.v1`. Wire-level reference: [os.lazy.init.app.events.v1](../../idl/os.lazy.init.app.events.v1.md).

What `init` sends an app on the channel it transferred with `Watch`.

## Functions

### `reopen(args)`

`Reopen(args: String) -> () oneway`

The app was launched again in this session (the start menu, a
desktop icon, `mimed`, the tray's Open row) while this instance runs:
show yourself and open `args` (empty, or `Launch`'s one path). The
first launch of a resident app by the session's autostart carries
`--background` in its argv instead (no window until this arrives).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `args` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::init_app_events::reopen(args);
```

### `quit(grace_ms)`

`Quit(grace_ms: U32) -> () oneway`

The user, a logout or the package manager asked the app to quit:
save and exit within `grace_ms`, after which `init` kills it. The
grace is a fixed 3 s counted from the `Stop`, so `grace_ms` is what
is left of it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `grace_ms` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::init_app_events::quit(grace_ms);
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.init.app.events.v1 on the wire](../../idl/os.lazy.init.app.events.v1.md)
