#!/usr/bin/env python3
"""What a scrolling run did, from its serial log: one comparable block per run.

    python tools/webplus/scroll_stats.py shots/webplus-live/serial.log [more logs]

The window is the span between the first and last `INFO:PAINT` second that saw wheel input. LazyWeb+ is a
client of `webd`, so the paint meter is the engine's, relayed by webd as `WEBD:NOTE:INFO:PAINT:...`, and the
browser is four processes (`procs.py`), each reported apart. In the window:

* the page's answer to the wheel (`w2d`, wheel to first damage report) and the paint rate;
* `INFO:SPEC` (the window's own wheel to drawn time, the client's), when the picture is shown ahead;
* the processes' CPU (`PERF:CPU`): the web process, webd's UI process, the client; and how often the CPU
  guard demoted the page's process (`SCHED:GUARD:SUMMARY`);
* with `LAZYOS_PERF=1`, how long the web process's main thread waited runnable (`PERF:runwait`): total,
  share of the window.

Runs are comparable when they scroll the same page the same way (`live_run.py --scroll N`); a live page
varies, so compare several runs, not one.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge  # noqa: E402
import procs  # noqa: E402

PAINT = re.compile(r"INFO:PAINT:(.*)")
RUNWAIT = re.compile(r"PERF:runwait:tick=(\d+)(.*)")
WAIT_ITEM = re.compile(r"(\S+)#(\d+)=(\d+)/(\d+)/(\d+)")
CPU = re.compile(procs.CPU_ROW.pattern)
GUARD = re.compile(r"SCHED:GUARD:SUMMARY tgid=(\d+) comm=(\S+) demotions=(\d+) threads=(\d+) window_s=(\d+)")
#: The client's own meter (the window's wheel to drawn time).
SPEC = re.compile(rf"{judge.P}:INFO:SPEC:(.*)")


def paint_of(line: str) -> re.Match | None:
    """The `INFO:PAINT` match of a serial line when it is the engine's (behind webd's relay prefix)."""
    engine = judge.engine_line(line)
    return PAINT.match(engine) if engine is not None else None


def fields(text: str) -> dict[str, str]:
    out = {}
    for part in text.split(":"):
        key, _, value = part.partition("=")
        out[key] = value
    return out


def pair(value: str) -> tuple[float, float]:
    avg, _, longest = value.partition("/")
    return float(avg or 0), float(longest or 0)


def stats(lines: list[str], pids: dict[int, str] | None = None) -> dict:
    """The block for the span of `lines` that saw wheel input. `pids` names the engine's processes
    (`procs.engine_pids` of the whole log) when `lines` is only a part of it."""
    first = last = None
    for index, line in enumerate(lines):
        match = paint_of(line)
        if match and int(fields(match.group(1)).get("wheel", "0") or 0) > 0:
            first = index if first is None else first
            last = index
    if first is None:
        return {"error": "no second with wheel input"}
    window = lines[first : last + 1]
    paints = [fields(m.group(1)) for line in window if (m := paint_of(line))]
    seconds = len(paints)
    result: dict = {"seconds": seconds}
    result["paints_per_s"] = sum(int(p["n"]) for p in paints) / seconds
    result["wheel_per_s"] = sum(int(p["wheel"]) for p in paints) / seconds
    result["full_frames"] = sum(int(p.get("full", 0)) for p in paints)
    answers = [pair(p["w2d"]) for p in paints if p.get("w2d", "0/0") != "0/0"]
    if answers:
        result["w2d_avg_ms"] = sum(a for a, _ in answers) / len(answers) / 1000
        result["w2d_worst_ms"] = max(m for _, m in answers) / 1000
    shown = [pair(fields(m.group(1))["w2s"]) for line in window if (m := SPEC.search(line))]
    if shown:
        result["w2s_avg_ms"] = sum(a for a, _ in shown) / len(shown) / 1000
    web_tgid = None
    pids = procs.engine_pids("\n".join(lines)) if pids is None else pids
    cpu: dict[str, list[int]] = {}
    for line in window:
        if m := CPU.search(line):
            role = procs.role_of(int(m.group(1)), m.group(5), pids)
            cpu.setdefault(role, []).append(int(m.group(2)))
            if role == procs.WEB:
                web_tgid = int(m.group(1))
    for role, key in ((procs.WEB, "web_cpu_pct"), (procs.WEBD, "webd_cpu_pct"), (procs.CLIENT, "client_cpu_pct"),
                      (procs.NETWORK, "network_cpu_pct")):
        if cpu.get(role):
            result[key] = sum(cpu[role]) / len(cpu[role])
    demotions = [int(m.group(3)) for line in window if (m := GUARD.search(line))
                 and procs.role_of(int(m.group(1)), m.group(2), pids) == procs.WEB]
    result["web_demotion_lines"] = len(demotions)
    result["web_demotions_total"] = sum(demotions)
    before = after = None
    for index, line in enumerate(lines):
        if m := RUNWAIT.search(line):
            if index <= first:
                before = m
            elif index >= last and after is None:
                after = m
    if before and after and web_tgid is not None:
        def items(match):
            return {int(s): (int(n), int(us), int(longest)) for _, s, n, us, longest in WAIT_ITEM.findall(match.group(2))}
        a, b = items(before), items(after)
        span_us = (int(after.group(1)) - int(before.group(1))) * 10_000
        waits, total, longest = b.get(web_tgid, (0, 0, 0))
        w0, t0, _ = a.get(web_tgid, (0, 0, 0))
        result["main_waits"] = waits - w0
        result["main_wait_ms"] = (total - t0) / 1000
        result["main_wait_share"] = (total - t0) / span_us if span_us else 0.0
        result["window_s"] = span_us / 1_000_000
    return result


def render(name: str, r: dict) -> str:
    if "error" in r:
        return f"{name}: {r['error']}"
    parts = [f"{name}: {r['seconds']} s of wheel input, {r['paints_per_s']:.1f} paints/s at {r['wheel_per_s']:.1f} wheel/s, {r['full_frames']} whole frames"]
    if "w2d_avg_ms" in r:
        parts.append(f"  page answer (w2d): {r['w2d_avg_ms']:.0f} ms average, {r['w2d_worst_ms']:.0f} ms worst")
    if "w2s_avg_ms" in r:
        parts.append(f"  drawn ahead (w2s): {r['w2s_avg_ms']:.1f} ms average")
    if "web_cpu_pct" in r:
        parts.append(f"  web process: {r['web_cpu_pct']:.0f}% of the core, {r['web_demotions_total']} guard demotions in {r['web_demotion_lines']} summaries")
    others = [f"{label} {r[key]:.0f}%" for label, key in (("webd UI process", "webd_cpu_pct"), ("client", "client_cpu_pct"),
                                                         ("network process", "network_cpu_pct")) if key in r]
    if others:
        parts.append("  others above 20%: " + ", ".join(others))
    if "main_wait_ms" in r:
        parts.append(f"  web main thread waited runnable {r['main_wait_ms'] / 1000:.1f} s of {r['window_s']:.0f} s "
                     f"({100 * r['main_wait_share']:.0f}%), {r['main_waits']} waits")
    return "\n".join(parts)


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__)
        return 2
    for name in argv:
        lines = Path(name).read_text(encoding="utf-8", errors="replace").splitlines()
        print(render(name, stats(lines)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
