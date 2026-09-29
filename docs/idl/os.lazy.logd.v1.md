# `os.lazy.logd.v1`

Interface id: `0x9c5197a46ce8a872`

The structured, hash-chained event log service (issue #93).

Every record chains over the previous record's hash, so `Verify` detects
tampering with any retained record. Failures are returned as a structured
error field (errno-style code, friendly text), not as a typed reply.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Tail | 299916675 | sync | `(count: Option<U64>) -> (records: Array<LogRecord>)` |
| Count | 1642576020 | sync | `() -> (count: U64)` |
| Verify | 761007172 | sync | `() -> (ok: Bool, index: U64)` |

## struct `LogRecord`

- `seq: U64`
- `tick: U64`
- `topic: String`
- `detail: String`
- `hash: U64`
