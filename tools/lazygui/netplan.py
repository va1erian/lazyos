"""What an image's network features need from QEMU and the build: the `--net`
flags run_demo and the screenshot tools share. Split out of `catalog` (file-size
budget); `catalog` re-exports everything here.

Networking is asked for directly (`net`), or brought by a feature that cannot
work without it: the HTTPS clients (`tls`), the LazyWeb browser
(`lazyweb`, which also brings the HTTPS clients and the desktop) and Wi-Fi
(`wifi`, a wireless interface of the same stack).
"""

from __future__ import annotations

import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "tools", "net"))
import qemu_net  # noqa: E402  (the QEMU network arguments run_demo and the tools share)


def wants_tls(cfg: dict) -> bool:
    """Whether the image carries the HTTPS clients (LAZYOS_TLS)."""
    return bool(cfg.get("tls") or cfg.get("lazyweb") or cfg.get("lazywebplus"))


def wants_net(cfg: dict) -> bool:
    """Whether the image has the network stack and QEMU a network card (the
    SMB client, Wi-Fi, the WebKit dependency proof, whose curl check fetches
    over HTTPS, and the WebKit port and browser need it too)."""
    return bool(cfg.get("net") or cfg.get("smb") or cfg.get("lazysky") or cfg.get("agent") or wants_dbgd(cfg)
                or wants_tls(cfg) or wants_wifi(cfg) or cfg.get("webkit_deps")
                or cfg.get("webkit_port") or cfg.get("webkit") or cfg.get("lazywebplus"))


def wants_wifi(cfg: dict) -> bool:
    """Whether the image carries Wi-Fi (LAZYOS_WIFI: `wifid` and its firmware,
    docs/wifi-rtw88-plan.md); it needs the stack."""
    return bool(cfg.get("wifi"))


def wants_dbgd(cfg: dict) -> bool:
    """Whether the image carries `dbgd`: always, so a box built from the
    launcher can be inspected (and its services reloaded) without a re-flash."""
    return True


def dbgd_env(cfg: dict) -> dict[str, str]:
    """The `dbgd` build switches: the service, its control tier (restart and
    hot reload, v2) and, only when ticked, MMIO register reads of allowlisted
    ranges, a switch of its own because a register read can wedge hardware
    (docs/dbgd-plan.md, "Hardware reads")."""
    env = {"LAZYOS_DBGD": "1", "LAZYOS_DBGD_CONTROL": "1"}
    ranges = dbgd_mmio_ranges(cfg)
    if dbgd_mmio_on(cfg):
        env["LAZYOS_DBGD_MMIO"] = "1"
    if cfg.get("dbgd_mmio_claimed"):
        env["LAZYOS_DBGD_MMIO_CLAIMED"] = "1"
    if ranges:
        env["LAZYOS_DBGD_MMIO_ALLOW"] = ",".join(ranges)
    return env


def dbgd_mmio_ranges(cfg: dict) -> list[str]:
    """The extra readable ranges typed in the Advanced tab
    (`VVVV:DDDD:BAR:START-END`, space or comma separated)."""
    return [r for r in cfg.get("dbgd_mmio_allow", "").replace(",", " ").split() if r]


def dbgd_mmio_on(cfg: dict) -> bool:
    """Register reads: ticked, or implied by the claimed switch or a range
    (they mean nothing without it)."""
    return bool(cfg.get("dbgd_mmio") or cfg.get("dbgd_mmio_claimed") or dbgd_mmio_ranges(cfg))


def dbgd_argv(cfg: dict) -> list[str]:
    """The run_demo flags that make `dbgd_env` (it sets the variables itself)."""
    argv = ["--dbgd-control"]
    if dbgd_mmio_on(cfg):
        argv.append("--dbgd-mmio")
    if cfg.get("dbgd_mmio_claimed"):
        argv.append("--dbgd-mmio-claimed")
    for range_ in dbgd_mmio_ranges(cfg):
        argv += ["--dbgd-mmio-allow", range_]
    return argv


def net_specs(cfg: dict) -> list[str]:
    """The port forwards typed in the launcher (space- or comma-separated);
    empty means run_demo's default (host 8080 to the Net Tools server)."""
    return [spec for spec in cfg.get("net_forwards", "").replace(",", " ").split() if spec]


def net_cards(cfg: dict) -> int:
    """How many network cards QEMU attaches (`--nics`, each on its own user
    network); out of range raises ValueError (a plan error)."""
    message = f"Network cards: use a whole number from 1 to {qemu_net.MAX_NICS}"
    raw = cfg.get("nics", 1)
    try:
        cards = int(str(raw).strip())
    except ValueError:
        # A blank or non-numeric entry: say what to type, not int()'s text,
        # and never read a blank as one card.
        raise ValueError(message) from None
    if not 1 <= cards <= qemu_net.MAX_NICS:
        raise ValueError(message)
    return cards


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
    cards = net_cards(cfg)
    if cards != 1:
        flags += ["--nics", str(cards)]
    return flags
