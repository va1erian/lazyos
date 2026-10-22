# `sys::timed`

The Rhai module for the Messenger interface `os.lazy.timed.v1`. Wire-level reference: [os.lazy.timed.v1](../../idl/os.lazy.timed.v1.md).

The time-of-day service (issue #369): UTC from the kernel wall clock, the
system time zone from `confd` (`sys/time/zone`), and a retained
`time/tick` event each minute.

Zones come from a small built-in table (fixed offsets plus DST rules), so a
zone name is either one of those or refused. Failures are returned as a
structured error field (errno-style code, friendly text), not as a typed
reply.

## Functions

### `now()`

`Now() -> (unix_ms: I64, tz_offset_s: I32, tz_name: String, dst: Bool)`

The current instant and its local-time parameters.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `unix_ms` | `I64` | int |
| `tz_offset_s` | `I32` | int |
| `tz_name` | `String` | string |
| `dst` | `Bool` | bool |

```rhai
let result = sys::timed::now();
```

### `get_zone()`

`GetZone() -> (name: String)`

The configured zone name (`UTC` when none is set).

Returns `name`: `String` (string).

```rhai
let result = sys::timed::get_zone();
```

### `set_zone(name)`

`SetZone(name: String) -> ()`

Switch to the built-in zone `name` and persist it to `confd`.
An unknown name fails with `EINVAL`. A machine setting, like the
clock: the caller's kernel-stamped credentials must hold
`CAP_SYS_TIME`, or the caller be `elevd`; anyone else gets `EPERM`
(a session asks `elevd` to write `sys/time/zone`, which `timed`
follows).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |

Returns `()`.

```rhai
let result = sys::timed::set_zone(name);
```

### `set_time(unix_secs)`

`SetTime(unix_secs: I64) -> ()`

Step the wall clock to `unix_secs` (UTC). The caller's kernel-stamped
credentials must hold `CAP_SYS_TIME`, or the caller be `elevd` (once
an administrator approved `time.set`); anyone else gets `EPERM`. A time
before the build's floor (`sntp::CLOCK_FLOOR_UNIX`, 2026-01-01) or past
2200 fails with `EINVAL`: the service would lift an earlier clock again
the next time it starts.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `unix_secs` | `I64` | int |

Returns `()`.

```rhai
let result = sys::timed::set_time(unix_secs);
```

### `ntp_status()`

`NtpStatus() -> (enabled: Bool, state: U32, server: String, stratum: U32, offset_ms: I64, delay_ms: I64, last_sync_unix: I64, next_sync_unix: I64)`

The state of the network time client (docs/ntp-plan.md): whether it is
switched on (`sys/time/ntp/enabled`), what it is doing (`state`, an
`NtpState` ordinal), and the last exchange that was accepted. Open to
everyone. Times are UTC seconds, 0 when there is none yet;
`offset_ms` and `delay_ms` are the last accepted exchange's server
minus local clock and round trip.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `enabled` | `Bool` | bool |
| `state` | `U32` | int |
| `server` | `String` | string |
| `stratum` | `U32` | int |
| `offset_ms` | `I64` | int |
| `delay_ms` | `I64` | int |
| `last_sync_unix` | `I64` | int |
| `next_sync_unix` | `I64` | int |

```rhai
let result = sys::timed::ntp_status();
```

### `sync_now()`

`SyncNow() -> ()`

Ask the network time client to query a server now. Open to everyone,
and rate-limited like the schedule: it fails with `EAGAIN` when a
query went out less than 15 seconds ago or is still in flight, or a
server asked to be left alone (`RATE`) and its hold has not passed,
`ENETUNREACH` when there is
no network and `ENODEV` when the client is off. Whatever the server
says is still judged by the same clock-step policy as a scheduled
exchange, so a caller cannot choose the time, only when to ask.

Returns `()`.

```rhai
let result = sys::timed::sync_now();
```

## Types

### `Tick`

The retained `time/tick` topic payload (issue #307). `unix` is UTC
seconds, `offset` the local offset in seconds with DST included, and
`zone_name` the zone (`Europe/Paris`).

| Name | IDL type | Script value |
|---|---|---|
| `unix` | `I64` | int |
| `offset` | `I32` | int |
| `zone_name` | `String` | string |

```rhai
let value = sys::timed::new_tick();
```

### `NtpState`

A string; the constants of this module are:

- `NTP_STATE_DISABLED` = `"Disabled"`
- `NTP_STATE_WAITING_FOR_NETWORK` = `"WaitingForNetwork"`
- `NTP_STATE_SYNCING` = `"Syncing"`
- `NTP_STATE_SYNCHRONIZED` = `"Synchronized"`
- `NTP_STATE_FAILED` = `"Failed"`

`NTP_STATE` is the array of all of them.

## Topics

### `time/tick`

Published each minute (and on any zone or clock change) and retained, so
a subscriber that starts late immediately learns the current time.

Payload `Tick`, QoS `latest`, retained.

Helpers: `TICK_PATTERN`, `on_tick(handler)`, `subscribe_tick()`, `publish_tick(payload)`.

```rhai
sys::timed::on_tick(|event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.timed.v1 on the wire](../../idl/os.lazy.timed.v1.md)
