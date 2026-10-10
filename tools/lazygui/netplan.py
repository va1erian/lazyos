"""What an image's network features need from QEMU and the build: the `--net`
flags run_demo and the screenshot tools share. Split out of `catalog` (file-size
budget); `catalog` re-exports everything here.

Networking is asked for directly (`net`), or brought by a feature that cannot
work without it: the HTTPS clients (`tls`) and the LazyWeb browser
(`lazyweb`, which also brings the HTTPS clients and the desktop).
"""

from __future__ import annotations

import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "tools", "net"))
import qemu_net  # noqa: E402  (the QEMU network arguments run_demo and the tools share)


def wants_tls(cfg: dict) -> bool:
    """Whether the image carries the HTTPS clients (LAZYOS_TLS)."""
    return bool(cfg.get("tls") or cfg.get("lazyweb"))


def wants_net(cfg: dict) -> bool:
    """Whether the image has the network stack and QEMU a network card (the
    SMB client needs it too)."""
    return bool(cfg.get("net") or cfg.get("smb") or wants_dbgd(cfg) or wants_tls(cfg))


def wants_dbgd(cfg: dict) -> bool:
    """Whether the image carries `dbgd` (its control tier implies it)."""
    return bool(cfg.get("dbgd") or cfg.get("dbgd_control"))


def net_specs(cfg: dict) -> list[str]:
    """The port forwards typed in the launcher (space- or comma-separated);
    empty means run_demo's default (host 8080 to the Net Tools server)."""
    return [spec for spec in cfg.get("net_forwards", "").replace(",", " ").split() if spec]


def net_flags(cfg: dict) -> list[str]:
    """The `--net` flags run_demo and the screenshot tools share, or none.
    A malformed forward raises ValueError (shown as a plan error)."""
    if not wants_net(cfg):
        return []
    specs = net_specs(cfg)
    qemu_net.forwards_from(specs)  # validate now, not after a long build
    flags = ["--net"]
    if wants_dbgd(cfg) and not specs:
        # The default forwards plus dbgd's port (run_demo --dbgd does the same).
        specs = list(qemu_net.DEFAULT_FORWARDS) + ["9701:9701"]
    for spec in specs:
        flags += ["--net-forward", spec]
    if cfg.get("net_restrict"):
        flags.append("--net-restrict")
    return flags
