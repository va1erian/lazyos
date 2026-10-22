# `sys::init_app`

The Rhai module for the Messenger interface `os.lazy.init.app.v1`. Wire-level reference: [os.lazy.init.app.v1](../../idl/os.lazy.init.app.v1.md).

A launched app's own line to `init` (docs/tray-plan.md section 5), served
by `init` under the name `os.lazy.init.app`. It is a separate interface
from `os.lazy.init.v1` so an app can be granted it without `Launch`,
`Stop` or `Shutdown`.

Only a task `init` launched may call, found by the kernel-stamped sender
in `init`'s table, and only for its own instance; anyone else gets
`ESRCH`. Failures reply with the standard error field (id 15,
`docs/midl.md`).

Not callable from a script (the request carries a kernel object): `Watch`.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.init.app.v1 on the wire](../../idl/os.lazy.init.app.v1.md)
