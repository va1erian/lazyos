# `sys::sysmond`

The Rhai module for the Messenger interface `os.lazy.sysmond.v1`. Wire-level reference: [os.lazy.sysmond.v1](../../idl/os.lazy.sysmond.v1.md).

The system monitor service (issue #144): one live system-stats snapshot.

The snapshot is the kernel's fixed-layout `sysinfo` block returned as raw
bytes, so a client decodes the same block `top` reads directly from the
kernel. Failures are returned as a structured error field (errno-style
code, friendly text), not as a typed reply.

`sysmond` also republishes the snapshot as the retained, typed
`system/stats/memory` and `system/stats/tasks` topics (issues #144, #307),
so a dashboard subscribes once and is handed the latest values instead of
polling `Snapshot`.

## Functions

### `snapshot()`

`Snapshot() -> (data: Bytes)`

Return one live system-stats snapshot.

Returns `data`: `Bytes` (blob).

```rhai
let result = sys::sysmond::snapshot();
```

## Types

### `MemoryStats`

The memory counters of one snapshot: the payload of the retained
`system/stats/memory` topic.

| Name | IDL type | Script value |
|---|---|---|
| `ticks` | `U64` | int |
| `frames_total` | `U64` | int |
| `frames_live` | `U64` | int |
| `frames_free` | `U64` | int |
| `slab_live` | `U64` | int |
| `slab_peak` | `U64` | int |
| `heap_used` | `U64` | int |
| `heap_total` | `U64` | int |

```rhai
let value = sys::sysmond::new_memory_stats();
```

### `TaskRow`

One live scheduler slot: `state`, `wait` and `class` are the short
labels the text view used to print, and `name` the NUL-trimmed short
name.

| Name | IDL type | Script value |
|---|---|---|
| `pid` | `U64` | int |
| `ppid` | `U64` | int |
| `state` | `String` | string |
| `wait` | `String` | string |
| `class` | `String` | string |
| `cpu` | `U64` | int |
| `name` | `String` | string |

```rhai
let value = sys::sysmond::new_task_row();
```

### `TasksStats`

The live-task table of one snapshot: `live` is the count and `tasks`
one row per live slot. The payload of the retained
`system/stats/tasks` topic.

| Name | IDL type | Script value |
|---|---|---|
| `live` | `U64` | int |
| `tasks` | `Array<TaskRow>` | array of map (`TaskRow`) |

```rhai
let value = sys::sysmond::new_tasks_stats();
```

## Topics

### `system/stats/memory`

The retained memory counters (issue #307): a late subscriber is handed
the latest value immediately.

Payload `MemoryStats`, QoS `latest`, retained.

Helpers: `STATS_MEMORY_PATTERN`, `on_stats_memory(handler)`, `subscribe_stats_memory()`, `publish_stats_memory(payload)`.

```rhai
sys::sysmond::on_stats_memory(|event| print(event.payload));
```

### `system/stats/tasks`

The retained live-task table (issue #307). `live` repeats the row
count so a subscriber can show the total without scanning `tasks`.

Payload `TasksStats`, QoS `latest`, retained.

Helpers: `STATS_TASKS_PATTERN`, `on_stats_tasks(handler)`, `subscribe_stats_tasks()`, `publish_stats_tasks(payload)`.

```rhai
sys::sysmond::on_stats_tasks(|event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.sysmond.v1 on the wire](../../idl/os.lazy.sysmond.v1.md)
