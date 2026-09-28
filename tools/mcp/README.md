# MCP debug bridge (prototype)

Debug-tooling-only MCP server that exposes live LazyOS Messenger/IPC state
(`FabricStats`) as a structured tool, instead of requiring an AI agent to
screenshot the framebuffer console or scrape human-formatted serial text.

See the "MCP Debug Bridge" wiki design doc for the full proposal and roadmap
(scheduler/task, memory allocator, and VFS tools are future phases). This is
Phase 1: it adds no new kernel surface, just a new `messengerctl` command
(`stats-json`) that prints the same `FabricStats` snapshot `stats` already
renders, as a single JSON line on a machine-parseable prefix, and a host-side
script that drives it over the existing QMP + serial-log tooling.

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

## Wire format

`messengerctl`'s `stats-json` command writes one line to the console (which
`SYS_WRITE` mirrors to the serial log — see `kernel/src/process/mod.rs`'s
`sys_write`):

```
MCP:FABRIC_STATS:{"services":1,"endpoints":2,...}
```

`debug_bridge.py` tails the serial log for that prefix and parses the JSON
payload. This keeps the kernel/userspace change minimal (one new
`messengerctl` command, no new syscalls or devices) while proving the
end-to-end shape of the MCP bridge described in the design doc.
