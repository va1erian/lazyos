#!/usr/bin/env python3
"""Where a LazyWeb scroll frame goes at 2560x1440 (docs/kabylake-gpu-plan.md, "Measure first").

Builds the LazyWeb desktop image at 2560x1440 (the Kaby Lake box's native size: a 1280x720
layout at scale 2), loads WebKit's tall test page from the host's stand-in server, then turns the
wheel without a pause for ``--seconds`` and reads what the guest printed: webd's ``INFO:PAINT`` lines
(the paint stages of WebKit's UI process, relayed as ``WEBD:NOTE:INFO:PAINT``) and the kernel's
``PERF:CPU`` lines (every busy process, split into user and kernel time). LazyWeb is a client of webd,
so the browser is four processes, reported apart (``procs.py``): the client (``lazyweb.elf``: the
window, the frame copies, the draw), webd's UI process (``webd.elf``: WebKit's paint and the mailbox),
the web process and the network process. The kernel half of a process with the display grant is the
present copy into the framebuffer. It is a baseline for deciding which stage a GPU
could remove, not a benchmark: QEMU's framebuffer is ordinary RAM, so the write-combining cost
of the box's aperture is not in it (``DISPLAY:PRESENT`` lines carry that on the box).

    python tools/lazyweb/frame_run.py                # build, boot, scroll 20 s, print the table
    python tools/lazyweb/frame_run.py --no-build     # the image and certificates of the last run
    python tools/lazyweb/frame_run.py --judge-only shots/lazyweb-frame/serial.log
    python tools/lazyweb/frame_run.py --mode 1280x720   # the same run at 1x, for the ratio

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
for sub in ("tools/webtest", "tools/abi", "tools"):
    sys.path.insert(0, str(ROOT / sub))
sys.path.insert(0, str(HERE))

#: `run.py` by path: other directories on the path have a module of that name too.
_spec = importlib.util.spec_from_file_location("lazyweb_run", HERE / "run.py")
lazyweb_run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(lazyweb_run)

import certs  # noqa: E402
import judge  # noqa: E402
import session  # noqa: E402
import sites  # noqa: E402

import procs  # noqa: E402
import scroll_page  # noqa: E402

TALL_URL = "https://theoldnet.com/lazyweb/scroll.html"
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
    caption = f"{TALL_TITLE} - LazyWeb"
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


def paint_fields(text: str) -> dict[str, list[float]]:
    """The engine's `INFO:PAINT` seconds (webd relays them as `WEBD:NOTE:INFO:PAINT:...`), field by field."""
    paint: dict[str, list[float]] = defaultdict(list)
    for line in judge.engine_markers(text, "INFO:PAINT"):
        for field in (line.split() or [""])[0].split(":"):
            key, _, value = field.partition("=")
            if key == "w2d":
                average, _, worst = value.partition("/")
                paint["w2d_avg_us"].append(float(average or 0))
                paint["w2d_max_us"].append(float(worst or 0))
            elif key in ("n", "needs", "px", "clear", "wk", "swz", "post", "avg", "max", "wheel", "mouse", "full", "delta", "scroll", "checks", "bad", "ver"):
                if value.lstrip("-").isdigit():
                    paint[key].append(float(value))
    return paint


def frame_cost(cpu: dict[str, list[tuple[int, int, int]]], rate: float) -> list[str]:
    """CPU per painted frame, in total and by process. The four programs of the browser (the client, webd's UI
    process, the web process, the network process) are reported apart; every other busy process (the
    compositor, say) is the rest. A process prints a look only above 20% of the core, so one that never did
    counts nothing here."""
    pct = {name: procs.average_pct(rows) for name, rows in cpu.items()}
    busy = sum(pct.values())
    ms = lambda p: p * 10 / rate  # noqa: E731  (percent of the core over one second, per paint)
    lines = [f"CPU per painted frame: {ms(busy):.1f} ms ({busy:.0f}% of the core over {rate:.1f} paints a second); "
             f"sustainable ceiling about {1000 / ms(busy):.0f} frames a second"]
    browser = {role: pct.get(role, 0.0) for role in procs.BROWSER_ROLES}
    other = busy - sum(browser.values())
    parts = ", ".join(f"{role} {ms(p):.1f}" for role, p in browser.items())
    lines.append(f"  by process, ms a painted frame: {parts}; everything else {ms(other):.1f}")
    return lines


def summarise(text: str, pids: dict[int, str] | None = None) -> tuple[str, bool]:
    """The table for a serial log, and whether it holds a scroll's worth of lines. `pids` names the engine's
    processes when `text` is only a part of the log (`procs.engine_pids` of all of it)."""
    # The processes' UP lines come before the page's title.
    pids = procs.engine_pids(text) if pids is None else pids
    start = text.find(f"{judge.P}:TITLE:{TALL_TITLE}")
    text = text[start:] if start >= 0 else text
    cpu = procs.cpu_rows(text, pids)
    paint = paint_fields(text)
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
    compose = [tuple(int(v) for v in row) for row in re.findall(
        r"XUID:COMPOSE repaints=(\d+) kpx=(\d+) compose_us=(\d+) present_us=(\d+) .*?worst_us=(\d+)", text)]
    if compose:
        kpx = sum(row[1] for row in compose)
        if kpx:
            lines.append(f"Compositor (XUID:COMPOSE): {sum(row[0] for row in compose)} repaints, {kpx / 1000:.0f} Mpx; "
                         f"compose {sum(row[2] for row in compose) / kpx * 1000:.0f} us per Mpx, "
                         f"present {sum(row[3] for row in compose) / kpx * 1000:.0f} us per Mpx, "
                         f"worst repaint {max(row[4] for row in compose)} us")
    window = defaultdict(list)
    for line in judge.markers(text, "INFO:WINDOW"):
        for field in (line.split() or [""])[0].split(":"):
            key, _, value = field.partition("=")
            if value.isdigit():
                window[key].append(int(value))
    updates = sum(window.get("updates", []))
    if updates:
        draws = max(1, sum(window["draws"]))
        lines.append(f"Window thread (INFO:WINDOW): {updates} updates, {sum(window['full']):.0f} whole; "
                     f"apply {sum(window['apply']) / updates:.0f} us an update, "
                     f"draw {sum(window['draw']) / draws:.0f} us a draw, worst {max(window['max']):.0f} us, "
                     f"up to {max(window['parts'])} parts")
    seconds_painting = len(paint.get("n", []))
    paints_total = sum(paint.get("n", []))
    if cpu and paints_total and seconds_painting:
        lines += frame_cost(cpu, paints_total / seconds_painting)
    lines.append("webd's UI process paint stages, per second while painting (INFO:PAINT, relayed as WEBD:NOTE:INFO:PAINT; "
                 "wall time, see paint.cpp: other tasks run inside it)")
    seconds = len(paint.get("n", []))
    paints = sum(paint.get("n", []))
    if paints:
        px = sum(paint["px"])
        lines.append(f"  {seconds} s, {paints:.0f} paints ({paints / seconds:.1f}/s), {px / paints / 1e6:.2f} Mpx a paint, "
                     f"{sum(paint.get('wheel', [])) / seconds:.1f} wheel notches/s offered")
        for key in ("clear", "wk", "swz", "post"):
            lines.append(f"  {key:<6}{sum(paint[key]) / paints:>9.0f} us a paint")
        lines.append(f"  total {sum(paint['avg']) / seconds:>9.0f} us a paint (average of the per-second averages)")
        if paint.get("ver"):
            # Probe images check every delta against a whole paint (paint.cpp, "the check"); a production
            # image does none of it, so it is shown apart from the stages above.
            lines.append(f"  check {sum(paint['ver']) / paints:>8.0f} us a paint, not in the stages: the probe image's "
                         f"picture check, {sum(paint['ver']) / 1000 / seconds:.1f} ms a second a production image does not spend")
        if paint.get("checks"):
            lines.append(f"  engine check against a whole paint: {sum(paint['checks']):.0f} checks, {sum(paint.get('bad', [])):.0f} differed")
        if paint.get("delta") or paint.get("full"):
            lines.append(f"  {sum(paint.get('full', [])):.0f} whole frames, {sum(paint.get('delta', [])):.0f} deltas, "
                         f"{sum(paint.get('scroll', [])):.0f} of them scrolls")
        if paint.get("w2d_avg_us"):
            lines.append(f"  wheel to first damage report: {sum(paint['w2d_avg_us']) / len(paint['w2d_avg_us']) / 1000:.1f} ms "
                         f"average, {max(paint['w2d_max_us']) / 1000:.1f} ms worst")
    else:
        lines.append("  (no INFO:PAINT line)")
    mismatches = sum(1 for line in judge.engine_lines(text) if line.startswith("FAIL:PAINT:MISMATCH"))
    if mismatches:
        lines.append(f"  FAIL: {mismatches} delta picture(s) differed from a whole paint (FAIL:PAINT:MISMATCH)")
    return "\n".join(lines), bool(paints) and not mismatches


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/lazyweb-frame")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=lazyweb_run.limits.BIG_APP_MEMORY)
    parser.add_argument("--seconds", type=float, default=20.0, help="how long the wheel turns")
    parser.add_argument("--notches", type=int, default=NOTCHES, help="wheel notches per QMP step")
    parser.add_argument("--pause", type=float, default=PAUSE, help="seconds between steps")
    parser.add_argument("--mode", default=DEFAULT_MODE, help="the display mode (LAZYOS_DISPLAY_MODE)")
    parser.add_argument("--step-timeout", type=float, default=300.0)
    parser.add_argument("--sample-port", type=int, help="record CR3 and RIP at 30 Hz over this QMP port to <out>/samples.tsv (tools/lazyweb/sample.py report)")
    parser.add_argument("--sample-stacks", action="store_true",
                        help="with --sample-port, keep each user-mode sample's stack (cpu_profile.py --callers)")
    parser.add_argument("--perf", action="store_true", help="build with LAZYOS_PERF=1 for the kernel's present_mpx metric")
    parser.add_argument("--judge-only", metavar="SERIAL_LOG")
    args = parser.parse_args(argv)
    if args.judge_only:
        table, ok = summarise(Path(args.judge_only).read_text(errors="replace"))
        print(table)
        return 0 if ok else 1
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    problem = lazyweb_run.built()
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
        error = lazyweb_run.build_image({"LAZYOS_TLS_TEST_CA": str(files.ca), "LAZYOS_TLS_TEST_HOSTS": str(files.hosts),
                                         "LAZYOS_DISPLAY_MODE": args.mode,
                                         **({"LAZYOS_PERF": "1"} if args.perf else {})}, False)
        if error:
            print(f"{judge.P}:FRAME:FAIL {error}")
            return 1
    served = sites.Sites(files.leaf_cert, files.leaf_key, closer_port=judge.CLOSER_PORT)
    qemu_args: tuple[str, ...] = ()
    sampler = None
    if args.sample_port:
        qemu_args = ("-qmp", f"tcp:127.0.0.1:{args.sample_port},server=on,wait=off")
        sampler = subprocess.Popen([sys.executable, str(HERE / "sample.py"), "record", "--port", str(args.sample_port),
                                    "--out", str(out / "samples.tsv"), "--seconds", "900",
                                    *(["--stacks"] if args.sample_stacks else [])], cwd=ROOT)
    try:
        ok, text = lazyweb_run.run_session(frame_session(args.seconds, args.step_timeout, args.notches, args.pause), out, args,
                                           qemu_args=qemu_args)
    finally:
        served.close()
        if sampler:
            sampler.terminate()
    table, enough = summarise(text)
    print(f"mode {args.mode}, wheel {args.seconds:.0f} s, session {'ok' if ok else 'FAILED'}")
    print(table)
    (out / "frame.txt").write_text(table + "\n", encoding="utf-8")
    verdict = ok and enough
    print(f"{judge.P}:FRAME:" + ("PASS" if verdict else "FAIL"))
    return 0 if verdict else 1


if __name__ == "__main__":
    sys.exit(main())
