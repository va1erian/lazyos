# `os.lazy.sysmond.v1`

Interface id: `0x5cd4605eb47c3d8f`

The system monitor service (issue #144): one live system-stats snapshot.

The snapshot is the kernel's fixed-layout `sysinfo` block returned as raw
bytes, so a client decodes the same block `top` reads directly from the
kernel. Failures are returned as a structured error field (errno-style
code, friendly text), not as a typed reply.

`sysmond` also republishes the snapshot as the retained, typed
`system/stats/memory` and `system/stats/tasks` topics (issues #144, #307),
so a dashboard subscribes once and is handed the latest values instead of
polling `Snapshot`.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Snapshot | 1312231341 | sync | `() -> (data: Bytes)` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/stats/memory` | `MemoryStats` | latest | yes | `publish:system/stats/memory`, `subscribe:system/stats/memory` |
| `system/stats/tasks` | `TasksStats` | latest | yes | `publish:system/stats/tasks`, `subscribe:system/stats/tasks` |

## struct `MemoryStats`

- `ticks: U64`
- `frames_total: U64`
- `frames_live: U64`
- `frames_free: U64`
- `slab_live: U64`
- `slab_peak: U64`
- `heap_used: U64`
- `heap_total: U64`

## struct `TaskRow`

- `pid: U64`
- `ppid: U64`
- `state: String`
- `wait: String`
- `class: String`
- `cpu: U64`
- `name: String`

## struct `TasksStats`

- `live: U64`
- `tasks: Array<TaskRow>`
