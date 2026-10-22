# `sys::lifecycle`

The Rhai module for the Messenger interface `os.lazy.lifecycle.v1`. Wire-level reference: [os.lazy.lifecycle.v1](../../idl/os.lazy.lifecycle.v1.md).

The service lifecycle contract (docs/shutdown.md): the one control message
`init` sends a supervised service during an orderly shutdown. A service
that serves it registers this interface next to its own; one that does not
is sent `SIGTERM` instead.

The service finishes the request it is serving, makes its state durable
(`confd` fsyncs its store, `logd` flushes its journals, `pkgd` fsyncs
`/logs/pkg.log`) and exits with status 0. Only a sender holding `CAP_SYS_ADMIN` (the supervisor) is
obeyed; anyone else's message is ignored. `init` waits for the exit, not
for a reply, and kills the service when its stop deadline passes.

## Functions

### `shutdown(reason)`

`Shutdown(reason: String) -> () oneway`

Persist and exit. `reason` is the shutdown's reason text, for the log.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `reason` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::lifecycle::shutdown(reason);
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.lifecycle.v1 on the wire](../../idl/os.lazy.lifecycle.v1.md)
