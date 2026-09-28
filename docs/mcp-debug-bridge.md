# MCP Debug Bridge (Debug-Build Only)

## Status
Prototype. Phases 1 (`fabric_stats`) and 2 (`list_tasks`) are implemented in
`tools/mcp/debug_bridge.py` (see `tools/mcp/README.md`), but over a simpler
transport than the one designed below: `messengerctl` prints `MCP:<NAME>:{json}`
lines on the console, which the kernel mirrors to serial, and the host script
tails the serial log through the existing QMP tooling. Phase 2 added native
syscall 13 (`kernel/src/task/introspect.rs`). The virtio-serial responder and
Cargo feature gate described in "Design" are not implemented; phases 3
(`memory_stats`, partly covered by syscall 14) and 4 (`inspect_vfs_node`) are
open. CI: `.github/workflows/mcp-bridge.yml`.

## Summary

Add a debug-build-only MCP (Model Context Protocol) server that exposes live
LazyOS kernel/OS state as structured, queryable tools for AI-assisted
development. It complements — and does not replace — the existing
`TEST:`/serial harness (batch correctness/soak checks) and the QMP
screenshot/input pipeline (visual/interaction verification required by
`AGENTS.md`).

## Motivation

LazyOS already has rich introspection, but it is optimized for a human at a
framebuffer console, not for an AI agent driving iteration:

- `messengerctl` (userspace debug shell) exposes `stats`, `services`/`health`,
  `sessions`, `log`/`log tail`, `topics`/`tail`, backed by the `messenger`
  syscall's `stats` op, which returns a versioned `FabricStats` ABI block
  (channels, queues, handles, buffers, services, audit counters).
- The kernel test harness (`kernel/src/tests.rs`, `tools/test/run.py`) emits
  one-shot `TEST:<name>:PASS|FAIL:<detail>` lines over serial — a batch
  protocol, not a live query interface.
- `tools/screenshot/qemu_qmp.py` drives QMP for screenshots and keyboard/mouse
  input injection.

Today, to answer a question like "how many tasks are alive" or "is this IPC
queue backed up," an agent must: script keystrokes into `messengerctl` over
QMP, wait for a framebuffer render, screenshot it, and OCR/pixel-parse the
result (or scrape serial text). This is slow and lossy for what is
fundamentally a structured data query. Worse, there is **no query surface at
all** for scheduler/task state, memory allocator internals, or VFS/ext2 tree
state — those are only exercised by build-time unit/soak tests, with no way
to inspect live state during an interactive session.

## Goals

- Give an AI agent (or a human) a fast, structured, JSON-based way to query
  live kernel/OS state during a running QEMU session, without going through
  the framebuffer/screenshot pipeline.
- Reuse existing in-kernel data sources (`FabricStats`, service registry)
  rather than duplicating them.
- Add new instrumentation where none exists: scheduler run-queue/task list,
  memory allocator stats, VFS/ext2 node inspection.
- Debug-build only: zero cost and zero attack surface in release builds.

## Non-goals

- Replacing the `TEST:` serial harness or the QMP screenshot/input pipeline.
  Visual/pixel verification (per `AGENTS.md`) and batch correctness/soak
  tests remain mandatory for graphics changes and kernel component coverage.
- Exposing this channel in release/production builds.
- General-purpose remote control of the guest (that's what QMP input
  injection already does).

## Design

### Transport

Reuse the pattern already established for QMP: a TCP socket exposed by QEMU,
this time carrying a small line-delimited JSON request/response protocol
from a **new virtio-serial (or virtio-vsock) device**, gated behind a
`cfg(feature = "debug-mcp")` (or `LAZYOS_TESTS`-style build flag) kernel
service. This avoids overloading the existing VGA/text console and keeps the
channel out of release builds entirely.

Boot flow addition (debug builds only):
1. `run_demo.py`/`qemu_shot.py` gain an optional `--mcp-port <port>` that maps
   a virtio-serial chardev to a host TCP port.
2. The kernel debug build starts a small in-kernel responder task that reads
   JSON requests off that virtio-serial port and writes JSON responses.
3. A host-side Python MCP server (`tools/mcp/debug_bridge.py`) connects to
   that port and exposes each request type as an MCP tool.

### Kernel-side responder

A single dispatcher task (only compiled in debug builds) that decodes a
request `{ "op": "<name>", ...args }` and calls into existing kernel
subsystems:

| Tool (MCP)         | Backing kernel op                          | New instrumentation needed? |
|---------------------|--------------------------------------------|------------------------------|
| `fabric_stats`      | `messenger` syscall `stats` op / `FabricStats` | No — wraps existing ABI |
| `list_services`     | Messenger service registry                 | No |
| `tail_topic`        | Messenger pub/sub subscribe (bounded)      | No, minor plumbing |
| `list_tasks`        | Scheduler run-queue walk                   | Yes — new read-only accessor |
| `memory_stats`      | Allocator (heap/page-frame) counters       | Yes — new read-only accessor |
| `inspect_vfs_node`  | VFS/ext2 tree walk by path or inode        | Yes — new read-only accessor |

All new accessors are **read-only snapshots** taken under the same locks the
subsystem already uses internally (no new locking model), serialized to a
small fixed struct, versioned the same way `FabricStats` is versioned (a
leading version word) so the host-side tool can detect skew.

### Host-side MCP server

A thin Python MCP server (`tools/mcp/debug_bridge.py`) that:
- Connects to the virtio-serial TCP port for a running QEMU instance
  (reusing connection conventions from `qemu_qmp.py`).
- Exposes one MCP tool per kernel op above, returning parsed JSON.
- Is entirely optional tooling — no kernel functionality depends on it
  existing; a human can equally well speak the same JSON protocol by hand
  for debugging.

### Build gating

- New kernel code lives behind a Cargo feature (e.g. `debug-mcp`), analogous
  to the existing `cfg(lazyos_tests)` gating for the test harness.
- `build.rs`/CI never enables it for release artifacts; only
  `tools/mcp/debug_bridge.py`'s own launch path (or an explicit
  `--debug-mcp` flag on `run_demo.py`) builds with it.

## Interaction with existing tooling

- **Does not replace** `tools/test/run.py` / `TEST:` protocol — that remains
  the batch correctness/soak gate required before any kernel component is
  considered done (per `AGENTS.md`).
- **Does not replace** `tools/screenshot/*.py` — visual verification of
  graphics/UI changes still requires real pixels per `AGENTS.md`.
- **Sits alongside** QMP: QMP still handles screenshots and input injection;
  the debug MCP bridge handles structured state queries. They can be used
  in the same session (e.g. inject input via QMP, then query task/IPC state
  via the bridge to confirm the effect, without needing a screenshot for
  non-visual state).

## Rollout plan (prototype scope)

1. **Phase 1** (highest value, lowest risk): wrap existing `FabricStats` and
   service registry data. No kernel changes beyond a thin debug-build
   virtio-serial responder task. Validates the transport and MCP tool
   plumbing end-to-end.
2. **Phase 2**: add `list_tasks` (scheduler) — read-only run-queue snapshot.
3. **Phase 3**: add `memory_stats` (allocator counters already computed for
   `mem_suite` tests, just needs a live accessor).
4. **Phase 4**: add `inspect_vfs_node` (VFS/ext2 walk).

Each phase should ship with its own kernel-side correctness test in
`kernel/src/tests.rs` per the repo's testing requirement, and a
`tools/mcp/README.md` documenting the wire protocol and tool list.

## Open questions

- Virtio-serial vs. reusing the QMP socket (adding a custom QMP command)
  vs. a raw extra serial port: virtio-serial is preferred since it avoids
  touching QMP's protocol surface, but needs confirming QEMU config support
  in `run_demo.py`'s existing invocation.
- Should the bridge be read-only forever, or eventually support safe
  mutation (e.g. killing a task, forcing a GC pass) for more powerful
  agent-driven debugging? Recommend starting strictly read-only.
