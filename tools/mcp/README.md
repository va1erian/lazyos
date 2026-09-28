# MCP debug bridge (prototype)

Debug-tooling-only MCP server that exposes live LazyOS state as structured
tools, instead of requiring an AI agent to screenshot the framebuffer console
or scrape human-formatted serial text.

See `docs/mcp-debug-bridge.md` for the full design proposal and rollout plan
(memory allocator and VFS tools are future phases).

- **Phase 1** (`fabric_stats`): no new kernel surface. `messengerctl` gained a
  `stats-json` command that prints the existing `FabricStats` snapshot
  (`stats` already renders it as a table) as one JSON line on a
  machine-parseable prefix.
- **Phase 2** (`list_tasks`): a new read-only scheduler accessor
  (`kernel/src/task/introspect.rs`, native syscall 13, `sys_tasks`) — there
  was no live query interface for task/scheduler state before this.
  `messengerctl` gained a matching `tasks-json` command.

Both ride the same host-side plumbing: a script that drives `messengerctl`
over the existing QMP + serial-log tooling (`tools/screenshot/qemu_qmp.py`).

## Requirements

- A LazyOS debug image built with `LAZYOS_MESSENGERCTL=1` (see
  `tools/run_demo.py` / the kernel build script) so `messengerctl` is running.
- `pip install mcp` to run as an actual MCP server (stdio transport).

## Usage

Sanity-check the QEMU/serial plumbing without an MCP client:

```bash
python tools/mcp/debug_bridge.py --image target/lazyos.img --self-test
```

Run as an MCP server (point an MCP-capable client at this command over stdio):

```bash
python tools/mcp/debug_bridge.py --image target/lazyos.img
```

## Tools exposed

- `fabric_stats` — live `FabricStats` snapshot: services/channels/endpoints,
  queue/message counters, buffer and fence stats, ACL/audit counters, and
  per-task handle/buffer usage.
- `list_tasks` — live scheduler task list: one row per live task slot with
  pid/ppid/pgid/sid, scheduler state (runnable/blocked/done), priority class,
  weight, and CPU ticks charged.

## Wire format

Each `messengerctl` command writes one line to the console (which
`SYS_WRITE` mirrors to the serial log — see `kernel/src/process/mod.rs`'s
`sys_write`):

```
MCP:FABRIC_STATS:{"services":1,"endpoints":2,...}
MCP:TASK_SNAPSHOT:{"version":1,"tasks":[{"pid":0,"ppid":0,...},...]}
```

`debug_bridge.py` tails the serial log for the matching `MCP:<NAME>:` prefix
and parses the JSON payload. This keeps each phase's kernel/userspace change
minimal — one new read-only accessor and one new `messengerctl` command per
phase, no new devices — while proving the end-to-end shape of the MCP bridge
described in the design doc.
