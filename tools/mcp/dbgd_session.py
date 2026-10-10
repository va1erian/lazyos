"""The TCP transport of the MCP debug bridge: the same tools, answered by a
box's `dbgd` (docs/dbgd-plan.md) instead of QMP and serial scraping, so they
work against QEMU with user networking and against a real PC on the LAN.

`DbgdSession` has the `fabric_stats()` and `list_tasks()` of the QEMU
`DebugBridgeSession`, plus `call()` for every `dbgd` method and
`control()`/`reload()` for the control tier (docs/dbgd-plan.md, v2).
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "dbg"))

from dbgclient import DEFAULT_PORT, DbgClient, DbgError  # noqa: E402,F401
import hotreload  # noqa: E402


def parse_target(text: str) -> tuple[str, int]:
    """`HOST` or `HOST:PORT`."""
    host, _, port = text.partition(":")
    return host, int(port) if port else DEFAULT_PORT


class DbgdSession:
    def __init__(self, target: str, key_hex: str | None = None):
        host, port = parse_target(target)
        self.target = (host, port, key_hex)
        self.client = DbgClient(host, port, key_hex)
        self.client.connect()

    def _fresh(self) -> DbgClient:
        self.client.close()
        self.client = DbgClient(*self.target)
        self.client.connect()
        return self.client

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

    def control(self, method: str, **params) -> dict:
        """A control-tier method: opens control on the connection first (it
        does not survive a reconnect)."""
        hotreload.begin(self.client)
        return self.client.call(method, **params)

    def install_app(self, package: bytes, relaunch: bool) -> dict:
        """Install an `.lzp` and relaunch its running instances."""
        return hotreload.install_app(self.client, package, relaunch)

    def reload(self, name: str, data: bytes, trial_ms: int) -> dict:
        """Hot-reload `name` from `data` and wait for init's verdict."""
        return hotreload.reload(self.client, name, data, trial_ms, reconnect=self._fresh)
