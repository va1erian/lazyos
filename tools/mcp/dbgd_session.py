"""The TCP transport of the MCP debug bridge: the same tools, answered by a
box's `dbgd` (docs/dbgd-plan.md) instead of QMP and serial scraping, so they
work against QEMU with user networking and against a real PC on the LAN.

`DbgdSession` has the `fabric_stats()` and `list_tasks()` of the QEMU
`DebugBridgeSession`, plus `call()` for every `dbgd` method.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "dbg"))

from dbgclient import DEFAULT_PORT, DbgClient, DbgError  # noqa: E402,F401


def parse_target(text: str) -> tuple[str, int]:
    """`HOST` or `HOST:PORT`."""
    host, _, port = text.partition(":")
    return host, int(port) if port else DEFAULT_PORT


class DbgdSession:
    def __init__(self, target: str, key_hex: str | None = None):
        host, port = parse_target(target)
        self.client = DbgClient(host, port, key_hex)
        self.client.connect()

    def close(self) -> None:
        self.client.close()

    def call(self, method: str, **params) -> dict:
        """Call a `dbgd` method; reconnects once if the box dropped us."""
        try:
            return self.client.call(method, **params)
        except (DbgError, OSError) as error:
            if isinstance(error, DbgError) and error.code != -1:
                raise
            self.client.close()
            self.client.connect()
            return self.client.call(method, **params)

    def fabric_stats(self, timeout: float = 5.0) -> dict:
        return self.call("fabric.stats")

    def list_tasks(self, timeout: float = 5.0) -> dict:
        return self.call("tasks.list")
