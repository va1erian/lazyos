# `os.lazy.timed.v1`

Interface id: `0xc3982ac21906d77`

The time-of-day service (issue #369): UTC from the kernel wall clock, the
system time zone from `confd` (`sys/time/zone`), and a retained
`time/tick` event each minute.

Zones come from a small built-in table (fixed offsets plus DST rules), so a
zone name is either one of those or refused. Failures are returned as a
structured error field (errno-style code, friendly text), not as a typed
reply.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Now | 188597655 | sync | `() -> (unix_ms: I64, tz_offset_s: I32, tz_name: String, dst: Bool)` |
| GetZone | 697211125 | sync | `() -> (name: String)` |
| SetZone | 1574816713 | sync | `(name: String) -> ()` |
| SetTime | 670376986 | sync | `(unix_secs: I64) -> ()` |

## struct `Tick`

- `unix: I64`
- `offset: I32`
- `zone_name: String`
