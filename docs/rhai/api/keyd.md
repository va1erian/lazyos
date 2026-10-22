# `sys::keyd`

The Rhai module for the Messenger interface `os.lazy.keyd.v1`. Wire-level reference: [os.lazy.keyd.v1](../../idl/os.lazy.keyd.v1.md).

The secrets and crypto service (issue #102).

`keyd` owns every long-term secret; no method returns key material. Keys
are scoped to the uid that generated them (the kernel-stamped sender), so
`Sign`, `Wrap`, `Unwrap` and `List` only see the caller's own keys.
Payloads of `Bytes` are capped at 8 KiB so a reply always fits the call
buffer. Failures are returned as a structured error field (errno-style
code, friendly text), not as a typed reply.

Key material stays in `keyd`'s own memory: no method carries a `Buffer`
or a `Channel`, so no client ever maps a page that holds a key.

## Functions

### `verify(user, secret)`

`Verify(user: String, secret: String) -> (ok: Bool)`

Check a username/password pair against the stored Argon2id verifier.
Accepted only from the accounts service (the `_accounts` system uid,
unlabelled), which slows password guessing (`Authenticate`); anyone
else gets `EPERM`. The plaintext `secret` crosses the channel.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `user` | `String` | string |
| `secret` | `String` | string |

Returns `ok`: `Bool` (bool).

```rhai
let result = sys::keyd::verify(user, secret);
```

### `sign(key, digest)`

`Sign(key: U64, digest: Bytes) -> (tag: Bytes)`

HMAC-SHA256 `digest` under the caller's stored key `key`; returns the tag.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `key` | `U64` | int |
| `digest` | `Bytes` | blob |

Returns `tag`: `Bytes` (blob).

```rhai
let result = sys::keyd::sign(key, digest);
```

### `wrap(key, plaintext)`

`Wrap(key: U64, plaintext: Bytes) -> (blob: Bytes)`

Seal `plaintext` under the caller's stored key `key`; returns an
authenticated blob.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `key` | `U64` | int |
| `plaintext` | `Bytes` | blob |

Returns `blob`: `Bytes` (blob).

```rhai
let result = sys::keyd::wrap(key, plaintext);
```

### `unwrap(key, blob)`

`Unwrap(key: U64, blob: Bytes) -> (plaintext: Bytes)`

Open a `blob` produced by `Wrap` under `key`; returns the plaintext.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `key` | `U64` | int |
| `blob` | `Bytes` | blob |

Returns `plaintext`: `Bytes` (blob).

```rhai
let result = sys::keyd::unwrap(key, blob);
```

### `random(len)`

`Random(len: U64) -> (bytes: Bytes)`

`len` cryptographically strong random bytes.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `len` | `U64` | int |

Returns `bytes`: `Bytes` (blob).

```rhai
let result = sys::keyd::random(len);
```

### `generate(kind)`

`Generate(kind: String) -> (id: U64)`

Create a fresh random key of type `kind` (`hmac` or `wrap`); returns
its opaque id.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `kind` | `String` | string |

Returns `id`: `U64` (int).

```rhai
let result = sys::keyd::generate(kind);
```

### `list()`

`List() -> (keys: Array<KeyInfo>)`

The caller's key ids, types and use counters; never material.

Returns `keys`: `Array<KeyInfo>` (array of map (`KeyInfo`)).

```rhai
let result = sys::keyd::list();
```

### `ping()`

`Ping() -> ()`

Round-trip probe.

Returns `()`.

```rhai
let result = sys::keyd::ping();
```

### `provision(user, secret)`

`Provision(user: String, secret: String) -> (verifier: String)`

Install or replace an account's password verifier (docs/accounts-plan.md
U1). Accepted only from the accounts service (the `_accounts` system
uid, unlabelled): whoever may plant a verifier may become that user.
`keyd` derives the Argon2id verifier over a fresh salt, starts
answering `Verify` with it, and returns it as the account database
stores it (`argon2id:<m_kib>:<t>:<p>:<salt>:<hash>`), so the account
survives a reboot; the secret does not outlive the call.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `user` | `String` | string |
| `secret` | `String` | string |

Returns `verifier`: `String` (string).

```rhai
let result = sys::keyd::provision(user, secret);
```

### `forget(user)`

`Forget(user: String) -> ()`

Drop an account's verifier: it can no longer log in. Accepted only
from the accounts service, like `Provision`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `user` | `String` | string |

Returns `()`.

```rhai
let result = sys::keyd::forget(user);
```

### `restore(user, verifier)`

`Restore(user: String, verifier: String) -> ()`

Put back a verifier the account database holds
(`argon2id:<m_kib>:<t>:<p>:<salt>:<hash>`), replacing what `keyd` has
for `user`: `accountsd` undoes a `Provision` whose database write
failed, so `keyd` never answers for a password the database lost.
Accepted only from the accounts service, like `Provision`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `user` | `String` | string |
| `verifier` | `String` | string |

Returns `()`.

```rhai
let result = sys::keyd::restore(user, verifier);
```

### `store_secret(scope, name, secret)`

`StoreSecret(scope: String, name: String, secret: Bytes) -> ()`

Keep `secret` (1..=256 bytes, a Wi-Fi passphrase) under `name`
(1..=64 of letters, digits and `. _ - :`), replacing one of that name
(docs/wifi-prerequisites-plan.md WP2). `scope` is `user`: the caller's
own, by its kernel-stamped uid; or `system`: available to every user
and before login, accepted only from `elevd` (the `net.wifi.system`
operation, after an administrator approved it on the trusted prompt;
`EPERM` for anyone else). Secrets persist across reboots, sealed under
a machine key (docs/security-model.md section 8). `EINVAL` for a bad
scope, name or secret; `ENOSPC` when the caller (or the table) holds
as many as it may; `EIO` when it could not be made durable (nothing
changed). No method ever returns a secret.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `scope` | `String` | string |
| `name` | `String` | string |
| `secret` | `Bytes` | blob |

Returns `()`.

```rhai
let result = sys::keyd::store_secret(scope, name, secret);
```

### `delete_secret(scope, name)`

`DeleteSecret(scope: String, name: String) -> ()`

Forget the secret `name` of `scope` (the same rules as `StoreSecret`;
`ENOENT` when there is none).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `scope` | `String` | string |
| `name` | `String` | string |

Returns `()`.

```rhai
let result = sys::keyd::delete_secret(scope, name);
```

### `list_secrets(scope)`

`ListSecrets(scope: String) -> (names: Array<String>)`

The names of the caller's `user` secrets, or of the `system` ones
(any caller may list those: they name the machine's networks). Names
only, never material.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `scope` | `String` | string |

Returns `names`: `Array<String>` (array of string).

```rhai
let result = sys::keyd::list_secrets(scope);
```

### `wifi_pmk(scope, name, ssid, owner)`

`WifiPmk(scope: String, name: String, ssid: Bytes, owner: U32) -> (pmk: Bytes)`

The WPA2 pairwise master key of the passphrase `name` of `scope` for
`ssid` (PBKDF2-HMAC-SHA1, 4096 rounds, IEEE 802.11 Annex J.4; a
64-hex-digit secret is a raw PSK and is its own PMK). Computed once
per (secret, SSID) and cached sealed beside the secret. Answered only
to `wlanmd` (the `_wlan` system uid, unlabelled, outside any session);
everyone else gets `EPERM`. For `user` the secret is the one `owner`
(a uid) owns, so a PMK is never given for another user's secret by
naming it; for `system`, `owner` must be 0. `name` must be a Wi-Fi
profile id (`w` and 12 lowercase hex digits), so no PMK is ever
derived from a secret that is not a network's (`EPERM` otherwise).
`ENOENT` for no such secret, `EINVAL` for a secret that is not a
passphrase (8 to 63 octets of UTF-8 without control characters) or
raw PSK, or an SSID outside 1..=32 octets.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `scope` | `String` | string |
| `name` | `String` | string |
| `ssid` | `Bytes` | blob |
| `owner` | `U32` | int |

Returns `pmk`: `Bytes` (blob).

```rhai
let result = sys::keyd::wifi_pmk(scope, name, ssid, owner);
```

### `store_secret_for(scope, name, secret, owner)`

`StoreSecretFor(scope: String, name: String, secret: Bytes, owner: U32) -> ()`

Keep `secret` under `name` for the user `owner` (scope `user`).
Answered only to `wlanmd` (the `_wlan` system uid, unlabelled, outside
any session), which takes a passphrase in `os.lazy.net.wifi.v1`
`AddNetwork` for the user who called it, and only for a `name` that is
a Wi-Fi profile id (`w` and 12 lowercase hex digits); everyone else,
any other name and the `system` scope (written by `elevd` alone) get
`EPERM`. Otherwise the rules of `StoreSecret`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `scope` | `String` | string |
| `name` | `String` | string |
| `secret` | `Bytes` | blob |
| `owner` | `U32` | int |

Returns `()`.

```rhai
let result = sys::keyd::store_secret_for(scope, name, secret, owner);
```

### `delete_secret_for(scope, name, owner)`

`DeleteSecretFor(scope: String, name: String, owner: U32) -> ()`

Forget the secret `name` of `owner` in `scope` (the rules of
`StoreSecretFor`; `ENOENT` when there is none).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `scope` | `String` | string |
| `name` | `String` | string |
| `owner` | `U32` | int |

Returns `()`.

```rhai
let result = sys::keyd::delete_secret_for(scope, name, owner);
```

## Types

### `KeyInfo`

One row of the key list: identity and counters only, never material.

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U64` | int |
| `kind` | `String` | string |
| `uses` | `U64` | int |
| `last_use` | `U64` | int |

```rhai
let value = sys::keyd::new_key_info();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.keyd.v1 on the wire](../../idl/os.lazy.keyd.v1.md)
