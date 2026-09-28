# `os.lazy.echo.v1`

Interface id: `0xcc4ac1057e84db93`

A tiny demo service: echo whatever you send (issue #90 sample IDL).

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Echo | 998075300 | sync | `(text: String, count: U32) -> (reply: String)` |
| Ping | 2142761129 | sync | `() -> (alive: Bool)` |
| Notify | 314575196 | oneway | `(event: Event) -> ()` |

## struct `Event`

- `topic: String`
- `at: U64`

## enum `Level`

- Info, Warn, Error
