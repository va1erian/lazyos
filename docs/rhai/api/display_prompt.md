# `sys::display_prompt`

The Rhai module for the Messenger interface `os.lazy.display.prompt.v1`. Wire-level reference: [os.lazy.display.prompt.v1](../../idl/os.lazy.display.prompt.v1.md).

The trusted prompt (docs/accounts-plan.md U2), served by `xuid` on the
display endpoint and accepted from `elevd` alone. `xuid` dims the screen
and draws the prompt above every client, panel and the shell; while it is
up no client gets a key or a pointer event, no keyboard grab holds, and no
window can rise above it. It names the caller from what `elevd` read off
the request's kernel stamp (`label_id` is resolved by `xuid` itself).
Cancel is the default; Escape cancels. One prompt at a time (`EBUSY`).

## Functions

### `prompt(summary, uid, user, label_id, admin, error)`

`Prompt(summary: String, uid: U32, user: String, label_id: U32, admin: String, error: String) -> (outcome: U32, name: String, secret: String)`

Show the prompt for `summary` (what would change), asked by the task
of `uid` (`user`) under kernel label `label_id`, and wait for the
person at the screen. `admin` pre-fills the name field (the asker,
when it is an administrator); `error` is shown above the fields (a
retry after a refused password). The reply comes when they answer:
`outcome` is a `PromptOutcome`, with `name` and `secret` when
approved.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `summary` | `String` | string |
| `uid` | `U32` | int |
| `user` | `String` | string |
| `label_id` | `U32` | int |
| `admin` | `String` | string |
| `error` | `String` | string |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `outcome` | `U32` | int |
| `name` | `String` | string |
| `secret` | `String` | string |

```rhai
let result = sys::display_prompt::prompt(summary, uid, user, label_id, admin, error);
```

## Types

### `PromptOutcome`

A string; the constants of this module are:

- `PROMPT_OUTCOME_APPROVED` = `"Approved"`
- `PROMPT_OUTCOME_CANCELLED` = `"Cancelled"`
- `PROMPT_OUTCOME_TIMED_OUT` = `"TimedOut"`

`PROMPT_OUTCOME` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.display.prompt.v1 on the wire](../../idl/os.lazy.display.prompt.v1.md)
