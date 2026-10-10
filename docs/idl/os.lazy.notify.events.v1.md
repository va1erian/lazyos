# `os.lazy.notify.events.v1`

Interface id: `0x13b1573de8af167e`

What the shell sends an app about its toasts: oneway methods on the
channel the app transferred with `Listen`.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Activated | 1 | oneway | `(id: U64) -> ()` |
| ActionInvoked | 2 | oneway | `(id: U64, action: U32) -> ()` |
| Closed | 3 | oneway | `(id: U64, reason: U32) -> ()` |
| Ping | 4 | oneway | `() -> ()` |
