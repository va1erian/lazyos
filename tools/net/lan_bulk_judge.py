"""The verdict and the table of `tools/net/lan_bulk.py`: what the host server
saw next to what the box's own `netbulk` printed.

The server's record (`bulkpeers.Transfer`) and the guest's marker
(`lanbox.GuestResult`) describe the same transfer from its two ends, so they
must agree on the verdict, the byte count and, within the guest's 10 ms
clock tick plus a round trip, the time. The server's address for the
connection must be the address of the interface under test, which is how a
run that went out of the wrong card is caught.
"""

from __future__ import annotations

import statistics
from dataclasses import asdict, dataclass

from bulkpeers import Transfer
from lanbox import GuestResult

#: Milliseconds the two clocks may differ regardless of size: the guest's
#: 10 ms tick on each end and the round trip of the close.
SLACK_MS = 50.0


@dataclass
class Row:
    round: int
    kind: str
    seed: int
    bytes: int
    host_mbps: float | None = None
    host_ms: float | None = None
    guest_mbps: float | None = None
    guest_ms: int | None = None
    header_ms: float | None = None
    peer: str = ""
    ok: bool = False


def expected_plan(rounds: int, seed: int) -> list[tuple[int, str, int]]:
    """`(round, kind, seed)` in the order `netbulk` runs them."""
    plan = []
    for r in range(rounds):
        plan.append((r, "PUT", seed + 2 * r))
        plan.append((r, "GET", seed + 2 * r + 1))
    return plan


def cross_check(guest: list[GuestResult], transfers: list[Transfer], *, size: int, rounds: int,
                seed: int, peer: str, tolerance: float = 0.25, server_only: bool = False,
                path: str = "native") -> tuple[list[Row], list[str], list[str]]:
    """`(rows, problems, warnings)` for a run of `rounds` rounds of `size` bytes."""
    problems: list[str] = []
    warnings: list[str] = []
    plan = expected_plan(rounds, seed)
    mine = [g for g in guest if g.path == path]
    early = [g for g in mine if not g.kind]
    problems += [f"guest: {g.detail}" for g in early]
    mine = [g for g in mine if g.kind]
    if server_only:
        mine = []
    elif not mine and not early:
        problems.append("the box's NETBULK lines were not found (the output file and dbgd's programs log "
                        "are empty); --server-only accepts the server's record alone")
    elif len(mine) < len(plan) and not early:
        warnings.append(f"only {len(mine)} of {len(plan)} NETBULK lines reached dbgd; a GET the box did not "
                        f"report is not counted (only the box can check the bytes it received)")
    elif len(mine) > len(plan):
        problems.append(f"the box printed {len(mine)} transfer lines, expected {len(plan)}")
    by_kind: dict[str, list[GuestResult]] = {"PUT": [], "GET": []}
    for g in mine:
        by_kind[g.kind].append(g)

    wanted = {(kind, s) for _, kind, s in plan}
    strangers = [t for t in transfers if (t.kind, t.seed) in wanted and t.peer != peer]
    for t in strangers:
        problems.append(f"{t.kind} seed {t.seed} came from {t.peer}, not {peer} "
                        f"(the address of the interface under test)")
    rows: list[Row] = []
    for r, kind, s in plan:
        row = Row(r, kind, s, size)
        rows.append(row)
        record = next((t for t in transfers if (t.kind, t.seed) == (kind, s) and t.peer == peer), None)
        label = f"round {r} {kind}"
        if record is None:
            problems.append(f"{label}: the server never saw seed {s} from {peer}")
            continue
        if not record.done:
            problems.append(f"{label}: the transfer did not finish")
        row.peer = record.peer
        row.host_mbps, row.host_ms = round(record.mbps, 2), round(record.seconds * 1e3, 1)
        row.header_ms = round(record.header_s * 1e3, 1)
        if not record.ok:
            problems.append(f"{label}: server: {record.problem or 'not complete'}")
        if record.size != size or (record.ok and record.received != size):
            problems.append(f"{label}: server holds {record.received} of {record.size} bytes, expected {size}")
        queue = by_kind[kind]
        if server_only:
            row.ok = record.ok and record.done
            continue
        if not queue:
            # The server checks every byte of a PUT; only the guest can check a GET.
            row.ok = record.ok and record.done and kind == "PUT"
            continue
        g = queue.pop(0)
        row.guest_mbps, row.guest_ms = g.mbps, g.ms
        row.ok = record.ok and record.done and g.verdict == "PASS"
        if g.verdict != "PASS":
            problems.append(f"{label}: guest: {g.detail}")
            continue
        if g.bytes != size:
            problems.append(f"{label}: guest reports {g.bytes} bytes, expected {size}")
            row.ok = False
        gap = abs(g.ms - record.seconds * 1e3)
        if gap > max(SLACK_MS, tolerance * max(g.ms, record.seconds * 1e3)):
            problems.append(f"{label}: timing disagrees: host {record.seconds * 1e3:.0f} ms, "
                            f"guest {g.ms} ms")
            row.ok = False
    if server_only:
        warnings.append("server side only: the box's own NETBULK lines were not used, so the bytes of a GET "
                        "(which only the box can check) are unverified")
    return rows, problems, warnings


def summarise(rows: list[Row]) -> dict[str, dict[str, float]]:
    """Per kind: how many good transfers and their host MB/s."""
    out: dict[str, dict[str, float]] = {}
    for kind in ("PUT", "GET"):
        values = [r.host_mbps for r in rows if r.kind == kind and r.ok and r.host_mbps is not None]
        if values:
            out[kind] = {"n": len(values), "median": round(statistics.median(values), 2),
                         "mean": round(statistics.fmean(values), 2),
                         "min": min(values), "max": max(values)}
    return out


def table(rows: list[Row]) -> str:
    head = ["round", "kind", "bytes", "host MB/s", "guest MB/s", "host ms", "guest ms", "from", "ok"]
    body = [[str(r.round), r.kind, str(r.bytes), _f(r.host_mbps), _f(r.guest_mbps), _f(r.host_ms),
             _f(r.guest_ms), r.peer or "-", "yes" if r.ok else "NO"] for r in rows]
    widths = [max(len(head[i]), *(len(line[i]) for line in body)) if body else len(head[i])
              for i in range(len(head))]
    fmt = lambda cells: "  ".join(c.rjust(w) for c, w in zip(cells, widths))  # noqa: E731
    return "\n".join([fmt(head), fmt(["-" * w for w in widths])] + [fmt(line) for line in body])


def _f(value) -> str:
    return "-" if value is None else str(value)


def rows_json(rows: list[Row]) -> list[dict]:
    return [asdict(r) for r in rows]
