# `os.lazy.keyd.v1`

Interface id: `0xd948c3355ba590bf`

The secrets and crypto service (issue #102).

`keyd` owns every long-term secret; no method returns key material. Keys
are scoped to the uid that generated them (the kernel-stamped sender), so
`Sign`, `Wrap`, `Unwrap` and `List` only see the caller's own keys.
Payloads of `Bytes` are capped at 8 KiB so a reply always fits the call
buffer. Failures are returned as a structured error field (errno-style
code, friendly text), not as a typed reply.

Key tables are meant to live in `SHARE_ONLY` shared buffers once a
userspace syscall for them exists; the wire has no buffer or handle
parameters today, so nothing here needs `Buffer`/`Handle` yet.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Verify | 761007172 | sync | `(user: String, secret: String) -> (ok: Bool)` |
| Sign | 258162884 | sync | `(key: U64, digest: Bytes) -> (tag: Bytes)` |
| Wrap | 540869 | sync | `(key: U64, plaintext: Bytes) -> (blob: Bytes)` |
| Unwrap | 936778462 | sync | `(key: U64, blob: Bytes) -> (plaintext: Bytes)` |
| Random | 1091948930 | sync | `(len: U64) -> (bytes: Bytes)` |
| Generate | 1196778162 | sync | `(kind: String) -> (id: U64)` |
| List | 220805025 | sync | `() -> (keys: Array<KeyInfo>)` |
| Ping | 2142761129 | sync | `() -> ()` |
| Provision | 1596114784 | sync | `(user: String, secret: String) -> (verifier: String)` |
| Forget | 1849666444 | sync | `(user: String) -> ()` |

## struct `KeyInfo`

- `id: U64`
- `kind: String`
- `uses: U64`
- `last_use: U64`
