# `sys::notify`

The Rhai module for the Messenger interface `os.lazy.notify.v1`. Wire-level reference: [os.lazy.notify.v1](../../idl/os.lazy.notify.v1.md).

Desktop notifications (docs/notify-plan.md, issue #747): a service or app
tells the user that something happened, and LazyShell shows it as a toast.
Served by LazyShell on Messenger under the name `os.lazy.notify`.

The shell owns the presenter: placement, stacking, animation, the
timeouts and the policy (rate limits, do-not-disturb, per-app mute). A
sender only says *what* and never *how*, so a hung app cannot stall a
toast. Nothing is kept: there is no history and no way to list what was
shown, so a toast that is missed is gone. (An app with a tray item sets
the tray's `Attention` state alongside a Warn or Error toast: that is the
durable signal, docs/tray-plan.md section 10.)

Identity comes from the kernel, never from the call: the toast's header
shows the registry name `init.ListApps` gives the sender's app, so an app
cannot post as another app or as the system. A caller that is not a launched
app but runs in the shell's session under the shell's user (a script run
from the Terminal) is shown as "Terminal". Callers must be in the shell's
login session and be uid 0 or the shell's uid; a labelled app also needs
this interface in its manifest. Anyone else gets `EACCES`.

Every field is validated: a title of at most 64 characters (not empty), a
body of at most 256 (a line feed ends a line of it; every other control
character is shown as a space), at most 3 actions of at most 24
characters with unique non-zero ids, a Lucide name or an image of at most 64 x 64
pixels, `progress` at most 100, a `level` that is a `Level` value;
anything else is `EINVAL`. An unusable icon is not an error: it falls
back to the level's own. At most 16 notifications of one sender are
alive (shown or waiting) at once (`ENOSPC`); a sender posting faster than
the rate limit gets `EBUSY` (nothing is shown). Failures reply with the
standard error field (id 15, `docs/midl.md`).

## Functions

### `notify(level, title, body, icon, actions, progress, replaces_id, timeout)`

`Notify(level: Level, title: String, body: String, icon: Option<Icon>, actions: Array<Action>, progress: Option<U32>, replaces_id: Option<U64>, timeout: Option<U32>) -> (id: U64)`

Show a toast and return its id. With `replaces_id` naming a live
toast of this sender, that toast is updated in place (a copy that
goes from "Copying..." to "Done" is one toast, not two) and the same
id comes back; an id that is gone or someone else's is ignored and a
new toast is made.

`timeout` is in milliseconds: absent is the level's default (Info and
Success 5 s), 0 until dismissed, otherwise 1 s to 60 s. Warn and
Error never time out, and neither does a toast whose `progress` is
below 100. `actions` is empty for none; `progress` absent for none.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `level` | `Level` | string (`Level` variant) |
| `title` | `String` | string |
| `body` | `String` | string |
| `icon` | `Option<Icon>` | map (`Icon`) or () |
| `actions` | `Array<Action>` | array of map (`Action`) |
| `progress` | `Option<U32>` | int or () |
| `replaces_id` | `Option<U64>` | int or () |
| `timeout` | `Option<U32>` | int or () |

Returns `id`: `U64` (int).

```rhai
let result = sys::notify::notify(level, title, body, icon, actions, progress, replaces_id, timeout);
```

### `close(id)`

`Close(id: U64) -> ()`

Close the sender's toast `id` (`Closed` with reason `Sender`). Not an
error when it is already gone, or is not this sender's.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U64` | int |

Returns `()`.

```rhai
let result = sys::notify::close(id);
```

Not callable from a script (the request carries a kernel object): `Listen`.

## Types

### `Icon`

A toast's picture: at most one source. An empty `Icon`, a Lucide name
the named-icon library (`lazyicons`, `docs/icons.md`) does not know,
or an image larger than 64 x 64 or whose `data` is not
`width * height * 4` bytes falls back to the level's icon.

| Name | IDL type | Script value |
|---|---|---|
| `lucide` | `Option<String>` | string or () |
| `image` | `Option<Image>` | map (`Image`) or () |

```rhai
let value = sys::notify::new_icon();
```

### `Image`

A Lucide outline by its kebab-case name (`usb`), tinted with the
toast's ink.
Full-colour straight RGBA, drawn as given.
Straight RGBA8 pixels, rows tightly packed.

| Name | IDL type | Script value |
|---|---|---|
| `width` | `U32` | int |
| `height` | `U32` | int |
| `data` | `Bytes` | blob |

```rhai
let value = sys::notify::new_image();
```

### `Action`

A button on the toast. `id` is what `ActionInvoked` reports back.
Mouse only: a toast never takes keyboard focus, so an action must
never be the only way to do something (docs/notify-plan.md).

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U32` | int |
| `label` | `String` | string |

```rhai
let value = sys::notify::new_action();
```

### `Level`

A string; the constants of this module are:

- `LEVEL_INFO` = `"Info"`
- `LEVEL_SUCCESS` = `"Success"`
- `LEVEL_WARN` = `"Warn"`
- `LEVEL_ERROR` = `"Error"`

`LEVEL` is the array of all of them.

### `CloseReason`

A string; the constants of this module are:

- `CLOSE_REASON_USER` = `"User"`
- `CLOSE_REASON_TIMEOUT` = `"Timeout"`
- `CLOSE_REASON_REPLACED` = `"Replaced"`
- `CLOSE_REASON_SENDER` = `"Sender"`
- `CLOSE_REASON_RESTART` = `"Restart"`
- `CLOSE_REASON_MUTED` = `"Muted"`

`CLOSE_REASON` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.notify.v1 on the wire](../../idl/os.lazy.notify.v1.md)
