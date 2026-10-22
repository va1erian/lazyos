# `sys::elevd`

The Rhai module for the Messenger interface `os.lazy.elevd.v1`. Wire-level reference: [os.lazy.elevd.v1](../../idl/os.lazy.elevd.v1.md).

The elevation service (docs/accounts-plan.md U2, issue #625).

Nobody is handed root or a capability: `elevd` runs a fixed table of
privileged *operations* itself (`libs/elevpolicy`), each only after an
administrator typed their name and password on the trusted prompt `xuid`
draws (`os.lazy.display.prompt.v1` below). The services that perform the
operations (accounts, confd, pkgd, timed, init, keyd) accept the privileged path
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

## Functions

### `request(operation, args)`

`Request(operation: String, args: Array<String>) -> (detail: String, values: Array<String>)`

Perform `operation` (a row of the operation table, e.g.
`account.create`, `conf.set`, `time.set`) with its `args`, after an
administrator approved it on the prompt. Every change asks every time;
only the elevated editor's view (`conf.elevate`, then `conf.list` and
`conf.get`) covers the same caller (uid, label and session; never an
unlabelled one) for five minutes, until its session ends or it calls
`Release`. `detail` describes what was done,
`values` carries the operation's results (`conf.list`'s paths,
`conf.get`'s kind and value).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `operation` | `String` | string |
| `args` | `Array<String>` | array of string |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `detail` | `String` | string |
| `values` | `Array<String>` | array of string |

```rhai
let result = sys::elevd::request(operation, args);
```

### `release()`

`Release() -> ()`

End the caller's standing approvals now (an elevated editor closing).

Returns `()`.

```rhai
let result = sys::elevd::release();
```

## Types

### `Record`

One request's audit record: the payload of
`system/events/elevd/request`.

| Name | IDL type | Script value |
|---|---|---|
| `operation` | `String` | string |
| `summary` | `String` | string |
| `uid` | `U32` | int |
| `user` | `String` | string |
| `label` | `U32` | int |
| `admin` | `String` | string |
| `outcome` | `String` | string |

```rhai
let value = sys::elevd::new_record();
```

## Topics

### `system/events/elevd/request`

The operation table row.
What it would change, as the prompt showed it.
The asking task's uid and account name.
The asking task's kernel label id (0: unlabelled).
The administrator who approved (or was named), if any.
`granted`, `refused`, `cancelled`, `timedout`, `locked`,
`failed` (approved, but the service refused), `invalid`, `held`
(refused without a prompt after the asker's unanswered prompts),
`busy` (the asker already had a request in hand, or too many
waited) or `nokeys` (the compositor could not secure the
keyboard, so no prompt was shown).
Every request, whatever came of it. Not retained: the journal is the
record.

Payload `Record`, QoS `latest`.

Helpers: `REQUEST_PATTERN`, `on_request(handler)`, `subscribe_request()`, `publish_request(payload)`.

```rhai
sys::elevd::on_request(|event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.elevd.v1 on the wire](../../idl/os.lazy.elevd.v1.md)
