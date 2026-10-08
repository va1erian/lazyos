#!/usr/bin/env python3
"""Prototype MCP debug bridge for LazyOS (debug-tooling only).

Exposes live `messengerctl` state — the Messenger/IPC fabric snapshot and the
scheduler's task list — as structured MCP tools, instead of requiring an agent
to screenshot the framebuffer console or scrape human-formatted serial text.

Design: see the "MCP Debug Bridge" design doc (`docs/mcp-debug-bridge.md`).

- Phase 1 (`fabric_stats`): wraps `messengerctl stats-json`
  (`user/src/bin/messengerctl.rs`), which prints the existing `FabricStats`
  snapshot as one `MCP:FABRIC_STATS:{...}` JSON line.
- Phase 2 (`list_tasks`): wraps `messengerctl tasks-json`, backed by a new
  read-only scheduler accessor (`kernel/src/task/introspect.rs`, native
  syscall 13) with no prior query interface, printed as one
  `MCP:TASK_SNAPSHOT:{...}` JSON line.

Both commands' output is already mirrored to the serial log (`SYS_WRITE` does
this — see `kernel/src/process/mod.rs`'s `sys_write`), so this script:

1. Boots (or attaches to) a headless QEMU instance the same way
   `tools/screenshot/qemu_session.py` does (QMP socket + serial-to-file).
2. Types the relevant `messengerctl` command into the guest's prompt over QMP.
3. Tails the serial log for the matching `MCP:<NAME>:{...}` line and parses
   the JSON payload.
4. Exposes each as an MCP tool.

Requires the `mcp` Python package (`pip install mcp`) for the actual MCP
server transport. The QEMU-driving logic (`DebugBridgeSession`) has no
dependency on `mcp` and can be exercised standalone via `--self-test` for
protocol debugging without an MCP client attached.

Usage
-----
    python tools/mcp/debug_bridge.py --image target/lazyos.img

Then point an MCP-capable client at this script (stdio transport). It exposes
two tools: `fabric_stats` and `list_tasks`.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "screenshot"))

from qemu_qmp import Qmp, accel_args, build_qemu_command, find_qemu, free_port  # noqa: E402


def marker_pattern(marker: str) -> re.Pattern[str]:
    """Regex for one `MCP:<marker>:{...}` serial line.

    `re.MULTILINE` is required: the payload line is followed by the shell prompt
    (and any other serial output) in the same buffer, and without it `$` only
    matches at the very end of the buffer, so the line was found only when the
    poll happened to land before anything else was printed.
    """
    return re.compile(rf"MCP:{re.escape(marker)}:(\{{.*\}})[ \t\r]*$", re.MULTILINE)


class DebugBridgeSession:
    """Owns a headless QEMU guest and answers `messengerctl`-backed queries.

    Reuses the same QMP + serial-log-file plumbing as
    `tools/screenshot/qemu_session.py`; no new QEMU-side protocol.
    """

    def __init__(
        self,
        image: str,
        out_dir: Path,
        qemu_path: str | None = None,
        accel: str = "auto",
        boot_wait: float = 8.0,
    ):
        self.qemu = find_qemu(qemu_path)
        self.out_dir = out_dir
        self.out_dir.mkdir(parents=True, exist_ok=True)
        self.serial_log = self.out_dir / "mcp_debug_bridge_serial.log"
        self.serial_log.write_text("")
        qmp_port = free_port()
        command = build_qemu_command(
            self.qemu, image, qmp_port, self.serial_log,
            extra_args=accel_args(accel, self.qemu),
        )
        self.proc = subprocess.Popen(command)
        self.qmp = Qmp("127.0.0.1", qmp_port, timeout=15)
        time.sleep(boot_wait)
        self._serial_pos = 0

    def close(self) -> None:
        try:
            self.qmp.close()
        finally:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def _new_serial_text(self) -> str:
        data = self.serial_log.read_bytes()
        text = data[self._serial_pos:].decode("utf-8", errors="replace")
        self._serial_pos = len(data)
        return text

    def _query(self, command: str, marker: str, timeout: float = 5.0) -> dict:
        """Type `command` and return the JSON payload of the first
        `MCP:<marker>:{...}` line that appears on serial."""
        pattern = marker_pattern(marker)
        self._new_serial_text()  # discard anything already buffered
        self.qmp.type_text(f"{command}\n")
        deadline = time.time() + timeout
        buf = ""
        while time.time() < deadline:
            buf += self._new_serial_text()
            match = pattern.search(buf)
            if match:
                return json.loads(match.group(1))
            time.sleep(0.1)
        raise TimeoutError(
            f"no MCP:{marker}: line seen on serial within {timeout}s "
            "(is messengerctl running and LAZYOS_MESSENGERCTL=1 set?)"
        )

    def fabric_stats(self, timeout: float = 5.0) -> dict:
        """`stats-json`: the live `FabricStats` snapshot (IPC/Messenger fabric)."""
        return self._query("stats-json", "FABRIC_STATS", timeout)

    def list_tasks(self, timeout: float = 5.0) -> dict:
        """`tasks-json`: the live scheduler task list."""
        return self._query("tasks-json", "TASK_SNAPSHOT", timeout)


def _self_test(image: str, out_dir: Path, qemu_path: str | None, accel: str) -> None:
    session = DebugBridgeSession(image, out_dir, qemu_path, accel)
    try:
        print("fabric_stats:")
        print(json.dumps(session.fabric_stats(), indent=2))
        print("\nlist_tasks:")
        print(json.dumps(session.list_tasks(), indent=2))
    finally:
        session.close()


def _run_mcp_server(image: str, out_dir: Path, qemu_path: str | None, accel: str) -> None:
    try:
        from mcp.server.fastmcp import FastMCP
    except ImportError as exc:
        raise SystemExit(
            "the 'mcp' package is required to run as an MCP server "
            "(pip install mcp); use --self-test to exercise the QEMU "
            "plumbing without it"
        ) from exc

    server = FastMCP("lazyos-debug-bridge")
    session = DebugBridgeSession(image, out_dir, qemu_path, accel)

    @server.tool()
    def fabric_stats() -> dict:
        """Live snapshot of the LazyOS Messenger/IPC fabric (FabricStats).

        Returns channel/queue counters, buffer stats, ACL/audit
        counters, and per-task handle/buffer usage, read from a running
        debug-build LazyOS guest via `messengerctl stats-json` over serial.
        """
        return session.fabric_stats()

    @server.tool()
    def list_tasks() -> dict:
        """Live scheduler task list.

        Returns one row per live task slot: pid/ppid/pgid/sid, scheduler
        state (runnable/blocked/done), priority class, weight, and CPU
        ticks charged, read from a running debug-build LazyOS guest via
        `messengerctl tasks-json` over serial.
        """
        return session.list_tasks()

    try:
        server.run()
    finally:
        session.close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True, help="LazyOS disk image")
    parser.add_argument("--out", default="shots/mcp", help="scratch dir for the serial log")
    parser.add_argument("--qemu", default=None, help="path to qemu-system-x86_64")
    parser.add_argument("--accel", default="auto", help="auto|kvm|whpx|none")
    parser.add_argument(
        "--self-test", action="store_true",
        help="boot, query each tool once, print JSON, exit (no MCP client needed)",
    )
    args = parser.parse_args()
    out_dir = Path(args.out)

    if args.self_test:
        _self_test(args.image, out_dir, args.qemu, args.accel)
    else:
        _run_mcp_server(args.image, out_dir, args.qemu, args.accel)


if __name__ == "__main__":
    main()
