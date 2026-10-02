# `os.lazy.logind.v1`

Interface id: `0x98121a421f33722d`

The console login service (issue #101): owns the session table and
answers the query `messengerctl sessions` renders.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Sessions | 916097772 | sync | `() -> (active: U64, sessions: Array<Session>)` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/events/login/start` | `LoginStart` | latest | yes | `publish:system/events/login/start`, `subscribe:system/events/login/start` |
| `system/events/login/session/+` | `LoginSession` | latest | yes | `publish:system/events/login/session/+`, `subscribe:system/events/login/session/+` |
| `system/events/login/denied` | `LoginDenied` | latest | yes | `publish:system/events/login/denied`, `subscribe:system/events/login/denied` |
| `system/events/login/end` | `LoginEnd` | latest | yes | `publish:system/events/login/end`, `subscribe:system/events/login/end` |

## struct `Session`

- `id: U64`
- `user: String`
- `uid: U32`
- `pid: U64`
- `state: String`
- `started: U64`

## struct `LoginStart`

- `user: String`
- `uid: U32`
- `session: U64`
- `pid: U64`
- `state: String`

## struct `LoginSession`

- `user: String`
- `uid: U32`
- `pid: U64`
- `state: String`
- `home: String`

## struct `LoginDenied`

- `user: String`
- `reason: String`

## struct `LoginEnd`

- `user: String`
- `uid: U32`
- `session: U64`
- `status: U64`
