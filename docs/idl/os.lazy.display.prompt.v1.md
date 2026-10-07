# `os.lazy.display.prompt.v1`

Interface id: `0x84929e679d4d88b2`

The trusted prompt (docs/accounts-plan.md U2), served by `xuid` on the
display endpoint and accepted from `elevd` alone. `xuid` dims the screen
and draws the prompt above every client, panel and the shell; while it is
up no client gets a key or a pointer event, no keyboard grab holds, and no
window can rise above it. It names the caller from what `elevd` read off
the request's kernel stamp (`label_id` is resolved by `xuid` itself).
Cancel is the default; Escape cancels. One prompt at a time (`EBUSY`).

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Prompt | 1337716571 | sync | `(summary: String, uid: U32, user: String, label_id: U32, admin: String, error: String) -> (outcome: U32, name: String, secret: String)` |

## enum `PromptOutcome`

- Approved, Cancelled, TimedOut
