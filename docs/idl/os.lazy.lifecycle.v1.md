# `os.lazy.lifecycle.v1`

Interface id: `0x778a92e489f41682`

The service lifecycle contract (docs/shutdown.md): the one control message
`init` sends a supervised service during an orderly shutdown. A service
that serves it registers this interface next to its own; one that does not
is sent `SIGTERM` instead.

The service finishes the request it is serving, makes its state durable
(`confd` fsyncs its store, `logd` flushes its journals, `pkgd` fsyncs
`/logs/pkg.log`) and exits with status 0. Only a sender holding `CAP_SYS_ADMIN` (the supervisor) is
obeyed; anyone else's message is ignored. `init` waits for the exit, not
for a reply, and kills the service when its stop deadline passes.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Shutdown | 1911669355 | oneway | `(reason: String) -> ()` |
