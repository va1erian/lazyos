#!/usr/bin/env python3
"""How a heavy live page (Ars Technica) loads and sits while the pointer sweeps it, as LazyWeb+ over webd.

One image (the browser, webd, the UI probe, the real Mozilla roots: no test CA), several boots. Each boot
opens https://arstechnica.com/ through ``mimed`` from the Terminal, waits for the load, then reloads it (F5)
while the pointer sweeps the page like a 200 Hz mouse, and sweeps it again once the page has settled. The
first boot only warms the HTTP cache and the ad blocker's lists, and is not counted.

LazyWeb+ is a client of the resident daemon webd, which owns WebKit: the client always merges pointer moves
(at most one every 8 ms), so the old comparison of forwarding every move against merging them is gone with
its switch (the ``pointer-mode`` file is not read any more, the client prints no ``INFO:POINTER``,
``INFO:CPU`` or ``INFO:MAINLAG``). What the bench measures now, with the pointer merged, from what exists:

* webd's ``INFO:PAINT`` once a second, relayed as ``WEBD:NOTE:INFO:PAINT`` (``mouse=``: pointer events
  WebKit handled; ``n=``: paints; ``wk=``: time in ``WKPagePaint``);
* the kernel's ``PERF:CPU`` look every 5 s at a process above 20% of the core, by program (``procs.py``):
  the web process, webd's UI process and the client apart (a process below 20% prints nothing and counts 0);
  build with ``--perf`` for ``LAZYOS_PERF=1``'s further kernel meters;
* ``UI:STALL`` (the window's thread, the client's).

    python tools/webplus/pointer_bench.py                 # build, boot five times, write the table
    python tools/webplus/pointer_bench.py --no-build --boots 3
    python tools/webplus/pointer_bench.py --judge-only shots/webplus-pointer

Needs the internet (the live site), and what ``tools/webplus/run.py`` needs.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
for sub in ("tools/abi", "tools"):
    sys.path.insert(0, str(ROOT / sub))
sys.path.insert(0, str(HERE))
import busybox  # noqa: E402
import judge  # noqa: E402
import procs  # noqa: E402
from lazygui import limits  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
URL = "https://arstechnica.com/"
STALE = ("LAZYOS_TLS_TEST_CA", "LAZYOS_TLS_TEST_HOSTS", "LAZYOS_CLI", "LAZYOS_XUI_APPS",
         "LAZYOS_WEBKIT_PORT", "LAZYOS_WEBKIT_DEPS", "LAZYOS_JSC", "LAZYOS_LINUXAPPS", "LAZYOS_WEBD")
#: A real mouse reports at 200 Hz: one PS/2 packet every 5 ms.
PACKET_DELAY = 0.005
#: Packets per sweep (about 12 s at 200 Hz, a little more with QMP's own time).
SWEEP_PACKETS = 2400
SWEEP_STEP = 6
#: Screen point inside the page area of the browser's window at 1280x720.
PAGE_POINT = (640, 400)
TYPE_DELAY = 0.05


def build_image(perf: bool = False) -> str | None:
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the shell (see tools/abi/busybox.py)"
    env = {k: v for k, v in os.environ.items() if k not in STALE}
    # webd comes with the browser (LAZYOS_LAZYWEBPLUS=1 requires it).
    env.update(LAZYOS_DESKTOP="1", LAZYOS_LAZYWEBPLUS="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_TLS="1", LAZYOS_XUI_AUTOSTART="term", LAZYOS_UI_PROBE="1", LAZYOS_RESET_OS="1",
               LAZYOS_BUSYBOX=str(shell),
               **limits.limit_env(limits.with_defaults(list(limits.WEBKIT_LIMITS), limits.BIG_APP_LIMITS)))
    if perf:
        env["LAZYOS_PERF"] = "1"
    print("pointer: cargo build (LazyWeb+ and webd, UI probe, Terminal at login)", flush=True)
    done = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if done.returncode != 0:
        return "cargo build failed:\n" + done.stderr[-4000:]
    return None


def sweep() -> list[dict]:
    """Left and right across the page in small steps, one packet each, at a mouse's rate."""
    steps = []
    for n in range(SWEEP_PACKETS):
        dx = SWEEP_STEP if (n // 40) % 2 == 0 else -SWEEP_STEP
        dy = 1 if (n // 200) % 2 == 0 else -1
        steps.append({"mouse_move": [dx, dy], "delay": PACKET_DELAY})
    return steps


def session(timeout: float) -> list[dict]:
    """One boot: the site, a sweep while it reloads, a sweep once it has settled."""
    setup = f"rhai -e 'sys::mimed::open(\"{URL}\", \"open\")' >/dev/console 2>&1"
    first_load = {"wait_for": r"WEBPLUS:LOAD:https://arstechnica\.com", "regex": True, "timeout": timeout}
    return [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 600},
        {"wait_for": "TERM:UP:PASS", "timeout": 300},
        {"wait_for": "NETD:RESOLV wrote", "timeout": 300},
        {"at": 2.0, "type": setup, "delay": TYPE_DELAY},
        {"key": "enter"},
        {"wait_for": "WEBPLUS:UP:PASS", "timeout": timeout},
        first_load,
        {"wait": 8.0},
        # The pointer to the corner, then onto the page (relative moves: the PS/2 mouse).
        {"mouse_move": [-2000, -2000]},
        {"mouse_move": list(PAGE_POINT)},
        {"wait": 1.0},
        {"shot": "01_loaded"},
        {"key": "f5"},
        *sweep(),
        {"wait_for": r"WEBPLUS:LOAD:https://arstechnica\.com", "regex": True, "occurrence": 2,
         "timeout": timeout},
        {"wait": 10.0},
        {"key_down": "ctrl"}, {"key": "h"}, {"key_up": "ctrl"},
        {"wait_for": "WEBPLUS:LOAD:about:history", "timeout": timeout},
        {"wait": 2.0},
        *sweep(),
        {"key_down": "ctrl"}, {"key": "j"}, {"key_up": "ctrl"},
        {"wait_for": "WEBPLUS:LOAD:about:downloads", "timeout": timeout},
        {"shot": "02_end"},
        {"quit": True},
    ]


def run_boot(steps: list[dict], out: Path, args) -> str:
    out.mkdir(parents=True, exist_ok=True)
    (out / "serial.log").unlink(missing_ok=True)
    script = out / "session.json"
    script.write_text(json.dumps(steps), encoding="utf-8")
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(script), "--accel", args.accel, "--memory", args.memory,
               "--net", "--net-forward", "none", "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    code = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    log = out / "serial.log"
    text = log.read_text(errors="replace") if log.is_file() else ""
    if code != 0:
        print(f"pointer: the session in {out} ended with {code}", flush=True)
    return text


#: webd's paint meter, after its relay prefix (``judge.engine_line``).
PAINT = re.compile(r"INFO:PAINT:\S*?:n=(\d+):\S*?:wk=(\d+):\S*?:mouse=(\d+):")
STALL = re.compile(r"UI:STALL kind=\S+ ms=(\d+)")
#: The columns of the table, in order.
KEYS = ["mouse_per_s", "paints_per_s", "paint_ms_per_s", "web_cpu", "webd_cpu", "client_cpu", "ui_stalls"]


def phases(serial: str) -> dict[str, list[str]]:
    """The serial lines of the sweep while the page reloads (F5 to its load, `loading`) and the sweep over the
    settled page (`settled`: about:history's load to about:downloads')."""
    lines = serial.splitlines()
    loads = [i for i, line in enumerate(lines) if re.search(r"WEBPLUS:LOAD:https://arstechnica\.com", line)]
    navs = [i for i, line in enumerate(lines) if re.search(r"WEBPLUS:NAV:https://arstechnica\.com", line)]
    history = next((i for i, line in enumerate(lines) if "WEBPLUS:LOAD:about:history" in line), None)
    end = next((i for i, line in enumerate(lines) if "WEBPLUS:LOAD:about:downloads" in line), None)
    if len(loads) < 2 or history is None or end is None:
        return {}
    start = next((i for i in navs if i > loads[0]), loads[0])
    return {"loading": lines[start:loads[1]], "settled": lines[history:end]}


def measure(lines: list[str], pids: dict[int, str] | None = None) -> dict[str, float]:
    """The numbers of a span of the log. `pids` names the engine's processes (`procs.engine_pids` of the
    whole log: their UP lines come before the span)."""
    engine = [judge.engine_line(line) for line in lines]
    paint = [tuple(map(int, m.groups())) for m in (PAINT.match(e) for e in engine if e) if m]
    cpu = procs.cpu_rows("\n".join(lines), pids)
    stalls = [int(m.group(1)) for m in map(STALL.search, lines) if m]
    seconds = max(len(paint), 1)
    return {
        "seconds": len(paint),
        "mouse_per_s": sum(p[2] for p in paint) / seconds,
        "paints_per_s": sum(p[0] for p in paint) / seconds,
        "paint_ms_per_s": sum(p[1] for p in paint) / seconds / 1000,
        "web_cpu": procs.average_pct(cpu.get(procs.WEB, [])),
        "webd_cpu": procs.average_pct(cpu.get(procs.WEBD, [])),
        "client_cpu": procs.average_pct(cpu.get(procs.CLIENT, [])),
        "ui_stalls": len(stalls),
    }


def table(results: list[dict[str, dict[str, float]]]) -> str:
    rows = ["| phase | boots | " + " | ".join(KEYS) + " |", "|---" * (len(KEYS) + 2) + "|"]
    for phase in ("loading", "settled"):
        runs = [r[phase] for r in results if phase in r]
        if not runs:
            continue
        cells = [f"{statistics.mean(run[k] for run in runs):.1f}" for k in KEYS]
        rows.append(f"| {phase} | {len(runs)} | " + " | ".join(cells) + " |")
    return "\n".join(rows)


def judge_run(out: Path, boots: int) -> str:
    results: list[dict[str, dict[str, float]]] = []
    for index in range(boots):
        log = out / f"boot{index}" / "serial.log"
        if index == 0 or not log.is_file():  # the first boot is the warm-up
            continue
        serial = log.read_text(errors="replace")
        if f"{judge.P}:UP:PASS" not in serial:
            print(f"pointer: boot {index} never opened the browser; left out", flush=True)
            continue
        found = phases(serial)
        pids = procs.engine_pids(serial)
        if found:
            results.append({name: measure(lines, pids) for name, lines in found.items()})
    return table(results)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--boots", type=int, default=5, help="boots, the first a warm-up (default 5)")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--perf", action="store_true", help="build with LAZYOS_PERF=1 (the kernel's extra meters)")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default=limits.BIG_APP_MEMORY)
    parser.add_argument("--timeout", type=float, default=240.0)
    parser.add_argument("--out", default=str(ROOT / "shots" / "webplus-pointer"))
    parser.add_argument("--judge-only", metavar="DIR", help="tabulate a saved run (DIR/boot<n>/serial.log)")
    args = parser.parse_args()
    out = Path(args.judge_only or args.out)
    if not args.judge_only:
        if not args.no_build:
            error = build_image(args.perf)
            if error:
                print(f"pointer: {error}")
                return 1
        for index in range(args.boots):
            print(f"pointer: boot {index}{' (warm-up)' if index == 0 else ''}", flush=True)
            run_boot(session(args.timeout), out / f"boot{index}", args)
    report = judge_run(out, args.boots)
    print(report)
    (out / "table.md").write_text(report + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
