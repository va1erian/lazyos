# `os.lazy.accounts.v1`

Interface id: `0x2cbf60abbc1951bc`

The account database service (issues #101, #624; docs/accounts-plan.md
U1). `accountsd` runs as the `_accounts` system uid and owns
`/accounts/db`; `/system/etc/passwd` and `/system/etc/group` are views
of it. Lookups and `ListUsers` are open to any caller; `Authenticate` is
open but slowed per account name and per caller (`EAGAIN` while a brake
holds). Creating, deleting and promoting accounts, and setting another
user's password, are accepted from `elevd` alone (after an administrator
approved them on the trusted prompt, U2), never from a uid or a
capability; the login screen may create the machine's first account during
the first-boot setup. Failures are a structured error field (errno-style
code, friendly text).

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Lookup | 1772818603 | sync | `(name: Option<String>, uid: Option<U32>) -> (found: Bool, user: Option<User>)` |
| Authenticate | 1137183084 | sync | `(name: String, secret: String) -> (ok: Bool)` |
| Create | 420340861 | sync | `(name: String, secret: String, admin: Bool) -> (user: User)` |
| Delete | 1469573738 | sync | `(name: String, home: String) -> ()` |
| SetPassword | 985541106 | sync | `(name: String, old: Option<String>, secret: String) -> ()` |
| SetAdmin | 1706675794 | sync | `(name: String, admin: Bool) -> ()` |
| ListUsers | 702470087 | sync | `() -> (users: Array<User>, setup: Bool)` |

## struct `User`

- `name: String`
- `uid: U32`
- `gid: U32`
- `home: String`
- `shell: String`
- `admin: Bool`
