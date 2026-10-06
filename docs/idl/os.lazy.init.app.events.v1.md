# `os.lazy.init.app.events.v1`

Interface id: `0x23f37f265bbdfe2c`

What `init` sends an app on the channel it transferred with `Watch`.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Reopen | 1 | oneway | `(args: String) -> ()` |
| Quit | 2 | oneway | `(grace_ms: U32) -> ()` |
