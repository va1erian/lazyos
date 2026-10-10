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

TCP transport (a box running `dbgd`, QEMU with user networking or a real PC;
docs/dbgd-plan.md):

    python tools/mcp/debug_bridge.py --connect 192.168.1.50 [--key HEX]

The same two tools are answered by `dbgd`, and more appear: `log_tail`,
`devices`, `drivers`, `usb_dump`, `hwreport`, `messenger_registry`,
`messenger_services`, `messenger_topics`, `fs_read` and a generic
`dbgd_call`. The key defaults to `target/dbgd.key`.
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


def _make_session(args) -> "DebugBridgeSession":
    """The QEMU/serial session, or the TCP one for `--connect`."""
    if args.connect:
        from dbgd_session import DbgdSession

        return DbgdSession(args.connect, args.key)
    return DebugBridgeSession(args.image, Path(args.out), args.qemu, args.accel)


def _self_test(session) -> None:
    try:
        print("fabric_stats:")
        print(json.dumps(session.fabric_stats(), indent=2))
        print("\nlist_tasks:")
        print(json.dumps(session.list_tasks(), indent=2))
    finally:
        session.close()


def _run_mcp_server(session) -> None:
    try:
        try:
            from mcp.server.fastmcp import FastMCP  # mcp 1.x
        except ImportError:
            # mcp 2.x renamed it (FastMCP -> MCPServer).
            from mcp.server.mcpserver import MCPServer as FastMCP
    except ImportError as exc:
        raise SystemExit(
            f"cannot import the MCP server library ({type(exc).__name__}: {exc}) "
            f"with {sys.executable}.\n"
            "Install it into that interpreter: "
            f'"{sys.executable}" -m pip install mcp\n'
            "(--self-test exercises the transport without it)"
        ) from exc

    server = FastMCP("lazyos-debug-bridge")

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

    if hasattr(session, "call"):
        _register_dbgd_tools(server, session)

    try:
        server.run()
    finally:
        session.close()


def _register_dbgd_tools(server, session) -> None:
    """The tools only a `dbgd` transport can answer (read-only)."""

    @server.tool()
    def log_tail(lines: int = 100, source: str = "kernel") -> dict:
        """The newest log lines of the box: source "kernel" (the boot log),
        "programs" (what the services printed: USBD:, NETDRV:, ...) or a
        logd journal. Lines are split into tag, key=value fields and text."""
        return session.call("log.tail", lines=lines, source=source)

    @server.tool()
    def devices() -> dict:
        """The PCI inventory: class, ids, owning driver uid, rights."""
        return session.call("devices.list")

    @server.tool()
    def drivers() -> dict:
        """devd's view: each device's matched driver, model and state."""
        return session.call("drivers.list")

    @server.tool()
    def usb_dump() -> dict:
        """usbd's last controller and device snapshot (xHCI registers,
        slots, endpoint rings), or unavailable without an xHCI."""
        return session.call("usb.dump")

    @server.tool()
    def hwreport() -> dict:
        """The HW:* verdict lines of the boot log, as fields."""
        return session.call("hwreport")

    @server.tool()
    def messenger_registry() -> dict:
        """Messenger's registered service names, owners and interfaces."""
        return session.call("msg.registry")

    @server.tool()
    def messenger_services() -> dict:
        """The services init supervises: state, pid, restarts, health."""
        return session.call("msg.services")

    @server.tool()
    def messenger_topics() -> dict:
        """The topics the Messenger broker has seen."""
        return session.call("msg.topics")

    @server.tool()
    def fs_read(path: str, offset: int = 0, length: int = 4096) -> dict:
        """Read a file under an allowlisted root (/transient, /tmp, /logs,
        /system/etc, /system/share, /docs)."""
        return session.call("fs.read", path=path, offset=offset, len=length)

    @server.tool()
    def dbgd_call(method: str, params: dict | None = None) -> dict:
        """Any dbgd method by name (see `methods`)."""
        return session.call(method, **(params or {}))

    _register_control_tools(server, session)


def _register_control_tools(server, session) -> None:
    """The control tier (docs/dbgd-plan.md, v2): these change the box, and
    answer -32002 unless it was built with LAZYOS_DBGD_CONTROL=1."""
    import hotreload  # tools/dbg, on the path once dbgd_session is loaded

    root = Path(__file__).resolve().parents[2]

    @server.tool()
    def service_reload(name: str, elf_path: str = "", trial_ms: int = 10000) -> dict:
        """Hot-reload the service `name` from a rebuilt binary: `elf_path` on
        this machine, or (empty) /system/bin/<name> of target/lazyos.img,
        so rebuild the image with the box's switches first. init on the box
        restarts the service from it and rolls back to the image's binary if
        it exits within `trial_ms`. Returns the final state: "committed" or
        "rolled-back" with the reason. Lasts until service_revert or reboot."""
        if elf_path:
            data = Path(elf_path).read_bytes()
        else:
            data = hotreload.image_binary(root / "target" / "lazyos.img", name)
        return session.reload(name, data, trial_ms)

    @server.tool()
    def service_restart(name: str) -> dict:
        """Restart a supervised service as it is (not messengerd or dbgd)."""
        return session.control("service.restart", name=name)

    @server.tool()
    def service_revert(name: str) -> dict:
        """Put the image's binary back for a hot-reloaded service."""
        return session.control("service.revert", name=name)

    @server.tool()
    def app_install(lzp_path: str, relaunch: bool = True) -> dict:
        """Install an app package (`.lzp` on this machine, e.g. from
        tools/pkg/build.py or target/pkg/) on the box, core apps included,
        and relaunch its running instances in their sessions so the new
        build shows at once. Lasts until replaced (it is a real install)."""
        return session.install_app(Path(lzp_path).read_bytes(), relaunch)

    @server.tool()
    def app_relaunch(app: str) -> dict:
        """Stop every running instance of an app (system_name or built-in
        id) and start it again in the same session."""
        return session.control("app.relaunch", app=app)

    @server.tool()
    def service_reloads() -> dict:
        """Every hot reload since boot: trial, committed, rolled-back, reverted."""
        return session.call("service.reloads")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", help="LazyOS disk image (the QEMU transport)")
    parser.add_argument("--connect", metavar="HOST[:PORT]",
                        help="use a running box's dbgd over TCP instead of booting QEMU")
    parser.add_argument("--key", help="with --connect: the hex key (default: target/dbgd.key)")
    parser.add_argument("--out", default="shots/mcp", help="scratch dir for the serial log")
    parser.add_argument("--qemu", default=None, help="path to qemu-system-x86_64")
    parser.add_argument("--accel", default="auto", help="auto|kvm|whpx|none")
    parser.add_argument(
        "--self-test", action="store_true",
        help="boot, query each tool once, print JSON, exit (no MCP client needed)",
    )
    args = parser.parse_args()
    if not args.image and not args.connect:
        parser.error("give --image (boot QEMU) or --connect HOST[:PORT] (a box running dbgd)")
    session = _make_session(args)
    if args.self_test:
        try:
            _self_test(session)
        finally:
            session.close()
    else:
        _run_mcp_server(session)


if __name__ == "__main__":
    main()
