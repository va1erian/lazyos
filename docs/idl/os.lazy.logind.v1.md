# `os.lazy.logind.v1`

Interface id: `0x98121a421f33722d`

The console login service (issue #101): owns the session table and
answers the query `messengerctl sessions` renders.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Sessions | 916097772 | sync | `() -> (active: U64, sessions: Array<Session>)` |

## struct `Session`

- `id: U64`
- `user: String`
- `uid: U32`
- `pid: U64`
- `state: String`
- `started: U64`
