# `os.lazy.init.app.v1`

Interface id: `0x616633071591076d`

A launched app's own line to `init` (docs/tray-plan.md section 5), served
by `init` under the name `os.lazy.init.app`. It is a separate interface
from `os.lazy.init.v1` so an app can be granted it without `Launch`,
`Stop` or `Shutdown`.

Only a task `init` launched may call, found by the kernel-stamped sender
in `init`'s table, and only for its own instance; anyone else gets
`ESRCH`. Failures reply with the standard error field (id 15,
`docs/midl.md`).

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Watch | 1 | sync | `(events: Channel<os.lazy.init.app.events.v1>) -> ()` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| Watch | `events` | `Channel<os.lazy.init.app.events.v1>` | `objects[0]`, a channel the receiver sends `os.lazy.init.app.events.v1` on |
