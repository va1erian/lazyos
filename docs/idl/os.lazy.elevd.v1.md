# `os.lazy.elevd.v1`

Interface id: `0xc9dbdc9caf1c9788`

The elevation service (docs/accounts-plan.md U2, issue #625).

Nobody is handed root or a capability: `elevd` runs a fixed table of
privileged *operations* itself (`libs/elevpolicy`), each only after an
administrator typed their name and password on the trusted prompt `xuid`
draws (`os.lazy.display.prompt.v1` below). The services that perform the
operations (accounts, confd, pkgd, timed, init) accept the privileged path
from `elevd`'s own identity (its system uid, unlabelled) and nobody else.

Every request is audited: one `system/events/elevd/request` record per
request (granted, refused, cancelled, timed out), which `logd` journals to
`/logs/elevd.log`. Wrong admin passwords lock the asker out for a growing
delay (`EAGAIN`), and so does a prompt the asker had cancelled or left to
time out (`EAGAIN`, no prompt: 5 s, doubling up to 2 minutes, ended by an
approval). A caller has one request in hand at a time (`EBUSY` for another
while one is answered or waiting). Failures are a structured error field
(errno-style code, friendly text): `EINVAL` an unknown operation or bad
arguments, `EPERM` a caller outside a login session, `ECANCELED` the
prompt was cancelled, `ETIMEDOUT` nobody answered it, `EACCES` no
administrator approved it, `ENODEV` no prompt can be shown (no display),
`EAGAIN` also when the compositor could not take the keyboard from the
apps for the prompt, or the performing service's own error.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Request | 38093138 | sync | `(operation: String, args: Array<String>) -> (detail: String, values: Array<String>)` |
| Release | 1830722334 | sync | `() -> ()` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/events/elevd/request` | `Record` | latest | no | `publish:system/events/elevd/request`, `subscribe:system/events/elevd/request` |

## struct `Record`

- `operation: String`
- `summary: String`
- `uid: U32`
- `user: String`
- `label: U32`
- `admin: String`
- `outcome: String`
