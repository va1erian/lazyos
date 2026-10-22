# `sys::healthd`

The Rhai module for the Messenger interface `os.lazy.healthd.v1`. Wire-level reference: [os.lazy.healthd.v1](../../idl/os.lazy.healthd.v1.md).

The service health aggregator (issue #93): retained health rows, a
heartbeat channel and the aggregate snapshot.

Both methods answer with the same snapshot shape (the aggregate `summary`
first, then one retained row per service). A service publishes a heartbeat
with `Report`; `Status` only reads. Failures are returned as a structured
error field (errno-style code, friendly text), not as a typed reply.

## Functions

### `report(name, status, detail)`

`Report(name: String, status: String, detail: String) -> (summary: HealthRecord, records: Array<HealthRecord>)`

Publish `health/<name>` with `status`/`detail`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `status` | `String` | string |
| `detail` | `String` | string |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `summary` | `HealthRecord` | map (`HealthRecord`) |
| `records` | `Array<HealthRecord>` | array of map (`HealthRecord`) |

```rhai
let result = sys::healthd::report(name, status, detail);
```

### `status()`

`Status() -> (summary: HealthRecord, records: Array<HealthRecord>)`

Snapshot retained health rows plus the aggregate.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `summary` | `HealthRecord` | map (`HealthRecord`) |
| `records` | `Array<HealthRecord>` | array of map (`HealthRecord`) |

```rhai
let result = sys::healthd::status();
```

## Types

### `HealthRecord`

One retained health row.

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `status` | `String` | string |
| `detail` | `String` | string |
| `tick` | `U64` | int |

```rhai
let value = sys::healthd::new_health_record();
```

## Topics

### `system/health/summary`

Service name (`summary` for the aggregate row).
Health status (`ok`/`degraded`/`down`).
Human-readable detail.
Tick the row was produced.
The retained aggregate snapshot (issue #307), published on
`system/health/summary` with the same payload as a service row. It is
declared before the `{name}` pattern so the literal wins in the
declaration table; a service literally named `summary` would collide
with it, so the supervisor never names one that.

Payload `HealthRecord`, QoS `latest`, retained.

Helpers: `SUMMARY_PATTERN`, `on_summary(handler)`, `subscribe_summary()`, `publish_summary(payload)`.

```rhai
sys::healthd::on_summary(|event| print(event.payload));
```

### `system/health/{name}`

The retained per-service health row (issue #307): `healthd` publishes
one on `system/health/<name>`, and a late subscriber is handed the live
row instead of polling `Status`.

Payload `HealthRecord`, QoS `latest`, retained.

Helpers: `HEALTH_PATTERN`, `health_topic(name)`, `on_health(name, handler)`, `subscribe_health(name)`, `publish_health(name, payload)`.

```rhai
sys::healthd::on_health(name, |event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.healthd.v1 on the wire](../../idl/os.lazy.healthd.v1.md)
