#!/usr/bin/env python3
"""Where a LazyWeb+ scroll frame goes at 2560x1440 (docs/kabylake-gpu-plan.md, "Measure first").

Builds the LazyWeb+ desktop image at 2560x1440 (the Kaby Lake box's native size: a 1280x720
layout at scale 2), loads WebKit's tall test page from the host's stand-in server, then turns the
wheel without a pause for ``--seconds`` and reads what the guest printed: the UI process's
``INFO:PAINT`` lines (its own paint stages) and the kernel's ``PERF:CPU`` lines (every busy
process, split into user and kernel time). The kernel half of a process with the display grant
is the present copy into the framebuffer; the UI process's user half is WebKit's paint, the
window thread's frame and the mailbox copies. It is a baseline for deciding which stage a GPU
could remove, not a benchmark: QEMU's framebuffer is ordinary RAM, so the write-combining cost
of the box's aperture is not in it (``DISPLAY:PRESENT`` lines carry that on the box).

    python tools/webplus/frame_run.py                # build, boot, scroll 20 s, print the table
    python tools/webplus/frame_run.py --no-build     # the image and certificates of the last run
    python tools/webplus/frame_run.py --judge-only shots/webplus-frame/serial.log
    python tools/webplus/frame_run.py --mode 1280x720   # the same run at 1x, for the ratio

Needs the same host ports and built artifacts as ``run.py``.
"""

from __future__ import annotations

import argparse
import importlib.util
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
for sub in ("tools/web", "tools/abi", "tools"):
    sys.path.insert(0, str(ROOT / sub))
sys.path.insert(0, str(HERE))

#: `run.py` by path: tools/web has a module of that name too.
_spec = importlib.util.spec_from_file_location("webplus_run", HERE / "run.py")
webplus_run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(webplus_run)

import certs  # noqa: E402
import judge  # noqa: E402
import session  # noqa: E402
import sites  # noqa: E402

import scroll_page  # noqa: E402

TALL_URL = "https://theoldnet.com/webplus/scroll.html"
TALL_TITLE = scroll_page.TITLE
#: Notches per wheel step and the pause after each. A step costs about 10 ms of QMP on top. The page
#: has some 580 notches of travel; a run turns round before `RUN_NOTCHES`, because a turn that moves
#: nothing paints nothing and its wait is not a frame.
NOTCHES = 3
PAUSE = 0.05
STEP_COST = 0.01
RUN_NOTCHES = 450
DEFAULT_MODE = "2560x1440"


def scroll_steps(seconds: float, notches: int = NOTCHES, pause: float = PAUSE) -> list[dict]:
    """Wheel steps for about `seconds`, down then up so the page never runs out. The guest's `wheel=`
    count says what really arrived."""
    count = max(1, int(seconds / (pause + STEP_COST)))
    run = max(1, RUN_NOTCHES // max(1, notches))
    steps: list[dict] = []
    for index in range(count):
        sign = -1 if (index // run) % 2 == 0 else 1
        steps.append({"mouse_scroll": sign * notches})
        if pause:
            steps.append({"wait": pause})
    return steps


def frame_session(seconds: float, timeout: float, notches: int = NOTCHES, pause: float = PAUSE) -> list[dict]:
    caption = f"{TALL_TITLE} - LazyWeb+"
    return [
        *session.boot_gates(timeout),
        {"wait_for": f"{judge.P}:LOAD:{judge.START}", "timeout": timeout},
        *session.navigate(TALL_URL, f"{judge.P}:LOAD:{TALL_URL}", timeout),
        {"wait_for": f"{judge.P}:TITLE:{TALL_TITLE}", "timeout": timeout},
        {"wait": 5.0},
        {"shot": "page"},
        {"move_to": {"window": caption, "widget": "page_origin", "offset": list(session.WHEEL_AT)}, "timeout": 60},
        {"wait": 1.0},
        *scroll_steps(seconds, notches, pause),
        {"shot": "scrolled"},
        {"wait": 6.0},  # the last PERF:CPU look
    ]


def summarise(text: str) -> tuple[str, bool]:
    """The table for a serial log, and whether it holds a scroll's worth of lines."""
    start = text.find(f"{judge.P}:TITLE:{TALL_TITLE}")
    text = text[start:] if start >= 0 else text
    roles = {int(pid): "WebProcess" for pid in re.findall(r"WEBPROCESS:UP:(\d+)", text)}
    roles.update({int(pid): "NetworkProcess" for pid in re.findall(r"NETWORKPROCESS:UP:(\d+)", text)})
    cpu: dict[str, list[tuple[int, int, int]]] = defaultdict(list)
    for tgid, pct, user, kernel, comm in re.findall(
            r"PERF:CPU tgid=(\d+) pct=(\d+) user=(\d+) kernel=(\d+) class=\S+ demotions=\d+ comm=(\S*)", text):
        cpu[roles.get(int(tgid), comm)].append((int(pct), int(user), int(kernel)))
    paint: dict[str, list[float]] = defaultdict(list)
    for line in re.findall(r"INFO:PAINT:(\S+)", text):
        for field in line.split(":"):
            key, _, value = field.partition("=")
            if key == "w2d":
                average, _, worst = value.partition("/")
                paint["w2d_avg_us"].append(float(average or 0))
                paint["w2d_max_us"].append(float(worst or 0))
            elif key in ("n", "needs", "px", "clear", "wk", "swz", "post", "avg", "max", "wheel", "mouse"):
                if value.lstrip("-").isdigit():
                    paint[key].append(float(value))
    lines = []
    lines.append("CPU per 5 s look, processes busy at the time (percent of the one CPU)")
    lines.append(f"  {'process':<22}{'looks':>6}{'pct avg':>9}{'user':>7}{'kernel':>8}")
    for name, rows in sorted(cpu.items(), key=lambda kv: -sum(r[0] for r in kv[1]) / len(kv[1])):
        n = len(rows)
        lines.append(f"  {name:<22}{n:>6}{sum(r[0] for r in rows) / n:>9.0f}"
                     f"{sum(r[1] for r in rows) / n:>7.0f}{sum(r[2] for r in rows) / n:>8.0f}")
    if not cpu:
        lines.append("  (no PERF:CPU line: nothing was above 20%)")
    mpx = re.findall(r"PERF:present_mpx:n=(\d+) p50_us=([\d.]+) p90_us=([\d.]+) p99_us=([\d.]+) max_us=([\d.]+)", text)
    if mpx:
        n, p50, p90, p99, worst = mpx[-1]
        lines.append(f"Kernel present copy, microseconds per megapixel (PERF:present_mpx, an image built with --perf): "
                     f"n={n} p50={p50} p90={p90} p99={p99} max={worst}")
    lines.append("UI process paint stages, per second while painting (INFO:PAINT)")
    seconds = len(paint.get("n", []))
    paints = sum(paint.get("n", []))
    if paints:
        px = sum(paint["px"])
        lines.append(f"  {seconds} s, {paints:.0f} paints ({paints / seconds:.1f}/s), {px / paints / 1e6:.2f} Mpx a paint, "
                     f"{sum(paint.get('wheel', [])) / seconds:.1f} wheel notches/s offered")
        for key in ("clear", "wk", "swz", "post"):
            lines.append(f"  {key:<6}{sum(paint[key]) / paints:>9.0f} us a paint")
        lines.append(f"  total {sum(paint['avg']) / seconds:>9.0f} us a paint (average of the per-second averages)")
        if paint.get("w2d_avg_us"):
            lines.append(f"  wheel to first damage report: {sum(paint['w2d_avg_us']) / len(paint['w2d_avg_us']) / 1000:.1f} ms "
                         f"average, {max(paint['w2d_max_us']) / 1000:.1f} ms worst")
    else:
        lines.append("  (no INFO:PAINT line)")
    return "\n".join(lines), bool(paints)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/webplus-frame")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=webplus_run.limits.BIG_APP_MEMORY)
    parser.add_argument("--seconds", type=float, default=20.0, help="how long the wheel turns")
    parser.add_argument("--notches", type=int, default=NOTCHES, help="wheel notches per QMP step")
    parser.add_argument("--pause", type=float, default=PAUSE, help="seconds between steps")
    parser.add_argument("--mode", default=DEFAULT_MODE, help="the display mode (LAZYOS_DISPLAY_MODE)")
    parser.add_argument("--step-timeout", type=float, default=300.0)
    parser.add_argument("--perf", action="store_true", help="build with LAZYOS_PERF=1 for the kernel's present_mpx metric")
    parser.add_argument("--judge-only", metavar="SERIAL_LOG")
    args = parser.parse_args(argv)
    if args.judge_only:
        table, ok = summarise(Path(args.judge_only).read_text(errors="replace"))
        print(table)
        return 0 if ok else 1
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    problem = webplus_run.built()
    if problem:
        print(f"{judge.P}:FRAME:FAIL {problem}")
        return 1
    busy = sites.ports_free((sites.HTTP_PORT, sites.HTTPS_PORT, judge.CLOSER_PORT))
    if busy:
        print(f"{judge.P}:FRAME:FAIL host port(s) {busy} in use: {sites.port_hint(OSError('busy'))}")
        return 1
    files = certs.load(out / "certs") if args.no_build else certs.generate(out / "certs")
    if files is None:
        print(f"{judge.P}:FRAME:FAIL --no-build needs the certificates of an earlier run")
        return 1
    if not args.no_build:
        error = webplus_run.build_image({"LAZYOS_TLS_TEST_CA": str(files.ca), "LAZYOS_TLS_TEST_HOSTS": str(files.hosts),
                                         "LAZYOS_DISPLAY_MODE": args.mode,
                                         **({"LAZYOS_PERF": "1"} if args.perf else {})}, False)
        if error:
            print(f"{judge.P}:FRAME:FAIL {error}")
            return 1
    served = sites.Sites(files.leaf_cert, files.leaf_key, closer_port=judge.CLOSER_PORT)
    try:
        ok, text = webplus_run.run_session(frame_session(args.seconds, args.step_timeout, args.notches, args.pause), out, args)
    finally:
        served.close()
    table, enough = summarise(text)
    print(f"mode {args.mode}, wheel {args.seconds:.0f} s, session {'ok' if ok else 'FAILED'}")
    print(table)
    (out / "frame.txt").write_text(table + "\n", encoding="utf-8")
    verdict = ok and enough
    print(f"{judge.P}:FRAME:" + ("PASS" if verdict else "FAIL"))
    return 0 if verdict else 1


if __name__ == "__main__":
    sys.exit(main())
