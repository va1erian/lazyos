# `os.lazy.notify.events.v1`

Interface id: `0x13b1573de8af167e`

What the shell sends an app about its toasts: `event` methods on the
channel the app transferred with `Listen`.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Activated | 1 | event | `(id: U64) -> ()` |
| ActionInvoked | 2 | event | `(id: U64, action: U32) -> ()` |
| Closed | 3 | event | `(id: U64, reason: U32) -> ()` |
| Ping | 4 | event | `() -> ()` |
