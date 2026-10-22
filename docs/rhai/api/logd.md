# `sys::logd`

The Rhai module for the Messenger interface `os.lazy.logd.v1`. Wire-level reference: [os.lazy.logd.v1](../../idl/os.lazy.logd.v1.md).

The structured, hash-chained event log service (issue #93).

Every record chains over the previous record's hash, so `Verify` detects
tampering with any retained record. Records are also appended to one
persistent journal per source, `/logs/<source>.log` (issue #508), read back
with `Sources` and `TailFile`. Failures are returned as a structured error
field (errno-style code, friendly text), not as a typed reply.

## Functions

### `tail(count)`

`Tail(count: Option<U64>) -> (records: Array<LogRecord>)`

Return the newest `count` records; an absent `count` means `10`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `count` | `Option<U64>` | int or () |

Returns `records`: `Array<LogRecord>` (array of map (`LogRecord`)).

```rhai
let result = sys::logd::tail(count);
```

### `count()`

`Count() -> (count: U64)`

Return the number of records appended since boot.

Returns `count`: `U64` (int).

```rhai
let result = sys::logd::count();
```

### `verify()`

`Verify() -> (ok: Bool, index: U64)`

Recompute the hash chain and report `ok`/first bad `index` (the record
count when the chain is intact).

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `ok` | `Bool` | bool |
| `index` | `U64` | int |

```rhai
let result = sys::logd::verify();
```

### `sources()`

`Sources() -> (sources: Array<String>)`

The sources that have a persisted journal (`/logs/<source>.log`),
sorted; empty when the store is absent. Served to uid 0 only, like
`TailFile`: the journals carry every user's activity.

Returns `sources`: `Array<String>` (array of string).

```rhai
let result = sys::logd::sources();
```

### `tail_file(source, count)`

`TailFile(source: String, count: U64) -> (lines: Array<String>)`

The newest `count` lines of `/logs/<source>.log` (records of earlier
boots included), oldest first. Served to uid 0 only (`EACCES`
otherwise); a source outside `[a-z0-9_-]{1,32}` fails with `EINVAL`
and one without a journal with `ENOENT`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `source` | `String` | string |
| `count` | `U64` | int |

Returns `lines`: `Array<String>` (array of string).

```rhai
let result = sys::logd::tail_file(source, count);
```

## Types

### `LogRecord`

One hash-chained log record.

| Name | IDL type | Script value |
|---|---|---|
| `seq` | `U64` | int |
| `tick` | `U64` | int |
| `topic` | `String` | string |
| `detail` | `String` | string |
| `hash` | `U64` | int |

```rhai
let value = sys::logd::new_log_record();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.logd.v1 on the wire](../../idl/os.lazy.logd.v1.md)
