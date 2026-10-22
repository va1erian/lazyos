"""The processes a LazyWeb+ run is made of, for the benches that read the kernel's `PERF:CPU` lines.

LazyWeb+ is a plain xui app over `webd` (os.lazy.webd), which owns WebKit: four programs share the work of a
page, and a bench reports them apart, because each is a different cost of one scroll frame:

* the **client** (`lazywebplus.elf`): the window, the copies of the frames webd hands it, the draw;
* **webd's UI process** (`webd.elf`): WebKit's UI half, its paint, the mailbox to the engine and the
  frames it passes on;
* the **web process** (`webprocess`): layout and script;
* the **network process** (`networkprocess`).

The kernel names a process by the basename of its program (`comm=`), but a process that has forked and not
yet run its program shows another name (`fork`), so the engine's own `WEBPROCESS:UP:<pid>` and
`NETWORKPROCESS:UP:<pid>` (relayed by webd, `judge.engine_lines`) name the web and the network process first.
"""

from __future__ import annotations

import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
import judge  # noqa: E402

CLIENT = "LazyWeb+ client"
WEBD = "webd UI process"
WEB = "WebProcess"
NETWORK = "NetworkProcess"
#: In the order a table lists them.
BROWSER_ROLES = (CLIENT, WEBD, WEB, NETWORK)

#: The programs' basenames, as the packages' manifests name them (`[entry] binary`, checked by
#: `test_procs.py`).
CLIENT_PROGRAM = "lazywebplus.elf"
WEBD_PROGRAM = "webd.elf"
#: A kernel `comm` may cut the name or the `.elf`: the stem decides.
COMM_ROLES = {"lazywebplus": CLIENT, "webd": WEBD, "webprocess": WEB, "networkprocess": NETWORK}


def comm_role(comm: str) -> str | None:
    """The browser role of a kernel `comm=` (`lazywebplus.elf`, `webd`, `WebProcess`...), or None."""
    stem = comm.rsplit(".elf", 1)[0].lower()
    return COMM_ROLES.get(stem)


def engine_pids(text: str) -> dict[int, str]:
    """pid -> role of the web and network processes the engine announced (`WEBPROCESS:UP:<pid>`,
    `NETWORKPROCESS:UP:<pid>`, behind webd's relay prefix)."""
    roles = {int(pid): WEB for pid in judge.engine_pids(text, "WEBPROCESS", "UP")}
    roles.update({int(pid): NETWORK for pid in judge.engine_pids(text, "NETWORKPROCESS", "UP")})
    return roles


def role_of(tgid: int, comm: str, pids: dict[int, str]) -> str:
    """What a `PERF:CPU` row is: the engine's own naming of a pid first, then the program's name, else `comm`."""
    return pids.get(tgid) or comm_role(comm) or comm


CPU_ROW = re.compile(r"PERF:CPU tgid=(\d+) pct=(\d+) user=(\d+) kernel=(\d+) class=\S+ demotions=\d+ comm=(\S*)")


def cpu_rows(text: str, pids: dict[int, str] | None = None) -> dict[str, list[tuple[int, int, int]]]:
    """role (or comm) -> the (percent, user, kernel) of each `PERF:CPU` look at it. A look is printed only for
    a process above 20% of the core, so a role with few rows was mostly idle. `pids` names the engine's
    processes (`engine_pids` of the whole log: their `UP` lines come before the part of the log being read)."""
    pids = engine_pids(text) if pids is None else pids
    rows: dict[str, list[tuple[int, int, int]]] = {}
    for tgid, pct, user, kernel, comm in CPU_ROW.findall(text):
        rows.setdefault(role_of(int(tgid), comm, pids), []).append((int(pct), int(user), int(kernel)))
    return rows


def average_pct(rows: list[tuple[int, int, int]]) -> float:
    return sum(r[0] for r in rows) / len(rows) if rows else 0.0
