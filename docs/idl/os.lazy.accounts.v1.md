# `os.lazy.accounts.v1`

Interface id: `0x2cbf60abbc1951bc`

The account database service. Lookups are open to any caller; creating a
user is restricted to uid 0 by the kernel-stamped sender identity.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Lookup | 1772818603 | sync | `(name: Option<String>, uid: Option<U32>) -> (found: Bool, user: Option<User>)` |
| Authenticate | 1137183084 | sync | `(name: String, secret: String) -> (ok: Bool)` |
| Create | 420340861 | sync | `(user: NewUser) -> (ok: Bool, detail: String)` |

## struct `User`

- `name: String`
- `uid: U32`
- `gid: U32`
- `home: String`
- `shell: String`

## struct `NewUser`

- `name: String`
- `uid: U32`
- `gid: U32`
- `secret: String`
- `home: String`
- `shell: String`
