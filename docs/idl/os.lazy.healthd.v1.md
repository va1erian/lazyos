# `os.lazy.healthd.v1`

Interface id: `0xd022082ef0aaed78`

The service health aggregator (issue #93): retained health rows, a
heartbeat channel and the aggregate snapshot.

Both methods answer with the same snapshot shape (the aggregate `summary`
first, then one retained row per service). A service publishes a heartbeat
with `Report`; `Status` only reads. Failures are returned as a structured
error field (errno-style code, friendly text), not as a typed reply.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Report | 1182923275 | sync | `(name: String, status: String, detail: String) -> (summary: HealthRecord, records: Array<HealthRecord>)` |
| Status | 6222351 | sync | `() -> (summary: HealthRecord, records: Array<HealthRecord>)` |

## struct `HealthRecord`

- `name: String`
- `status: String`
- `detail: String`
- `tick: U64`
