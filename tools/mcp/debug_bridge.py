#!/usr/bin/env python3
"""Prototype MCP debug bridge for LazyOS (debug-tooling only).

Exposes live `messengerctl` Messenger/IPC fabric state (the `FabricStats`
snapshot) as a structured MCP tool, instead of requiring an agent to screenshot
the framebuffer console or scrape human-formatted serial text.

Design: see the "MCP Debug Bridge" wiki design doc. This is Phase 1 of that
proposal: it wraps the *existing* `messengerctl stats-json` command (see
`user/src/bin/messengerctl.rs`) rather than adding any new kernel surface.
`messengerctl` already mirrors console output to the serial log (`SYS_WRITE`
does this — see `kernel/src/process/mod.rs`'s `sys_write`), so this script:

1. Boots (or attaches to) a headless QEMU instance the same way
   `tools/screenshot/qemu_session.py` does (QMP socket + serial-to-file).
2. Types `stats-json\n` into the guest's `messengerctl` prompt over QMP.
3. Tails the serial log for the `MCP:FABRIC_STATS:{...}` line the new
   `stats-json` command prints, and parses the JSON payload.
4. Exposes that as the `fabric_stats` MCP tool.

Requires the `mcp` Python package (`pip install mcp`) for the actual MCP
server transport. The QEMU-driving logic (`FabricStatsSession`) has no
dependency on `mcp` and can be exercised standalone via `--self-test` for
protocol debugging without an MCP client attached.

Usage
-----
    python tools/mcp/debug_bridge.py --image target/lazyos.img

Then point an MCP-capable client at this script (stdio transport). It exposes
one tool for now: `fabric_stats`.
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

_STATS_MARKER = re.compile(r"MCP:FABRIC_STATS:(\{.*\})\s*$")


class FabricStatsSession:
    """Owns a headless QEMU guest and answers `fabric_stats()` queries.

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

    def fabric_stats(self, timeout: float = 5.0) -> dict:
        """Type `stats-json` and return the parsed `FabricStats` snapshot."""
        self._new_serial_text()  # discard anything already buffered
        self.qmp.type_text("stats-json\n")
        deadline = time.time() + timeout
        buf = ""
        while time.time() < deadline:
            buf += self._new_serial_text()
            match = _STATS_MARKER.search(buf)
            if match:
                return json.loads(match.group(1))
            time.sleep(0.1)
        raise TimeoutError(
            "no MCP:FABRIC_STATS: line seen on serial within "
            f"{timeout}s (is messengerctl running and LAZYOS_MESSENGERCTL=1 set?)"
        )


def _self_test(image: str, out_dir: Path, qemu_path: str | None, accel: str) -> None:
    session = FabricStatsSession(image, out_dir, qemu_path, accel)
    try:
        stats = session.fabric_stats()
        print(json.dumps(stats, indent=2))
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
    session = FabricStatsSession(image, out_dir, qemu_path, accel)

    @server.tool()
    def fabric_stats() -> dict:
        """Live snapshot of the LazyOS Messenger/IPC fabric (FabricStats).

        Returns channel/queue counters, buffer/fence stats, ACL/audit
        counters, and per-task handle/buffer usage, read from a running
        debug-build LazyOS guest via `messengerctl stats-json` over serial.
        """
        return session.fabric_stats()

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
        help="boot, query fabric_stats once, print JSON, exit (no MCP client needed)",
    )
    args = parser.parse_args()
    out_dir = Path(args.out)

    if args.self_test:
        _self_test(args.image, out_dir, args.qemu, args.accel)
    else:
        _run_mcp_server(args.image, out_dir, args.qemu, args.accel)


if __name__ == "__main__":
    main()
