#!/usr/bin/env python3
"""Scrolling a real page in a maximized LazyWeb+ at 2560x1440: is it smooth? (docs/lazywebplus-performance-plan.md §11)

Boots a plain image (no test host mappings: the real CA bundle and the real sites over QEMU's user network),
maximizes the window on the start page, opens each address, lets it settle, puts the pointer in the page and
turns the wheel for ``--seconds`` (the same continuous pattern as ``frame_run.py``), then prints one block per
page from the serial log: the window's frames (``INFO:SMOOTH``: frames drawn a second while scrolling, the
longest gap, how much of what was shown was checkerboard), the page's answer (``INFO:PAINT``), CPU by process
(``PERF:CPU``) and the window thread (``INFO:WINDOW``).

    python tools/webplus/scroll_bench.py --build                     # build, then ars technica and webkit.org
    python tools/webplus/scroll_bench.py --url https://webkit.org/ --seconds 15
    python tools/webplus/scroll_bench.py --judge-only shots/webplus-bench/serial.log

Live sites change from run to run: compare runs made close together, and more than one.
"""

from __future__ import annotations

import argparse
import importlib.util
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
for sub in ("tools/web", "tools/abi", "tools"):
    sys.path.insert(0, str(ROOT / sub))
sys.path.insert(0, str(HERE))

_spec = importlib.util.spec_from_file_location("webplus_run", HERE / "run.py")
webplus_run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(webplus_run)

import frame_run  # noqa: E402
import judge  # noqa: E402
import scroll_stats  # noqa: E402
import session  # noqa: E402

DEFAULT_URLS = ["https://arstechnica.com/", "https://webkit.org/"]
#: The start page's caption (`xui-app/webplus/src/start.html`'s title, `app.rs` `caption`).
START_CAPTION = judge.caption("LazyWeb+")
#: Where the pointer rests for the wheel, in page pixels from the page's top-left corner: the middle of a
#: maximized page area at 2560x1440, away from the edges.
WHEEL_AT = (1200, 600)
MAXIMIZE_AT = (945, 0)
#: A consent dialog a person would answer before reading, by site: where its accept button is, in page pixels of
#: the maximized window (Ars Technica's "I accept", the dialog centred in the view).
CONSENT = {"arstechnica.com": (1082, 761)}


#: Notches the wheel turns one way before it turns back: 2400 px, inside a short page (webkit.org's front page is
#: about three views tall at 2560x1440), so the page always has somewhere to go.
RUN_NOTCHES = 60


def scroll_steps(seconds: float, notches: int, pause: float, run_notches: int = RUN_NOTCHES) -> list[dict]:
    """Wheel steps for about `seconds`: `notches` a step, `run_notches` down, then as many up, and again."""
    count = max(1, int(seconds / (pause + frame_run.STEP_COST)))
    run = max(1, run_notches // max(1, notches))
    steps: list[dict] = []
    for index in range(count):
        sign = -1 if (index // run) % 2 == 0 else 1
        steps.append({"mouse_scroll": sign * notches})
        if pause:
            steps.append({"wait": pause})
    return steps


def bench_session(urls: list[str], settle: float, seconds: float, timeout: float, notches: int, pause: float) -> list[dict]:
    steps = [*session.boot_gates(timeout), {"wait_for": f"{judge.P}:LOAD:{judge.START}", "timeout": timeout},
             {"wait": 1.0}]
    # The title bar's maximize button (at 2560x1440 the window opens 2008 px wide; the button is 945 px right
    # of the title bar's middle); the page is laid out again at the new size.
    steps += [{"click_at": {"target": f"title:{START_CAPTION}", "offset": list(MAXIMIZE_AT)}, "timeout": 60,
               "until": f"{judge.P}:INFO:RESIZE:", "retries": 2},
              {"wait": 2.0},
              # The pointer is put in the page while the start page's caption names it: a live page's own
              # caption is not known in advance, and navigating from the keyboard leaves the pointer there.
              {"move_to": {"window": START_CAPTION, "widget": "page_origin", "offset": list(WHEEL_AT)}, "timeout": 60},
              {"wait": 1.0}]
    for index, url in enumerate(urls):
        steps += [
            *session.navigate(url, f"{judge.P}:NAV:", timeout=60),
            {"wait": settle},
            *consent(url),
            {"shot": f"site{index}"},
            *scroll_steps(seconds, notches, pause),
            {"shot": f"site{index}_scrolled"},
            {"wait": 6.0},
        ]
    return steps


def consent(url: str) -> list[dict]:
    """A click on the site's consent button, then the pointer back where the wheel turns."""
    for host, at in CONSENT.items():
        if host in url:
            page = {"window": START_CAPTION, "widget": "page_origin"}
            return [{"click_at": {**page, "offset": list(at)}, "timeout": 60}, {"wait": 4.0},
                    {"move_to": {**page, "offset": list(WHEEL_AT)}, "timeout": 60}, {"wait": 1.0}]
    return []


SMOOTH = re.compile(r"INFO:SMOOTH:(\S+)")


def smooth_block(text: str) -> list[str]:
    rows = [scroll_stats.fields(m.group(1)) for m in SMOOTH.finditer(text)]
    rows = [r for r in rows if int(r.get("wheel", "0") or 0) > 0]
    if not rows:
        return ["  (no INFO:SMOOTH line with scrolling)"]
    seconds = len(rows)
    frames = sum(int(r.get("frames", 0)) for r in rows)
    worst = max(int(r.get("gapmax", 0)) for r in rows)
    over = sum(int(r.get("over33", 0)) for r in rows)
    checker = sum(int(r.get("checker", 0)) for r in rows)
    shown = sum(int(r.get("shown", 0)) for r in rows) or 1
    return [f"  window: {frames / seconds:.1f} frames/s over {seconds} s of scrolling, longest gap {worst / 1000:.0f} ms, "
            f"{over} gaps over 33 ms, checkerboard {100 * checker / shown:.1f}% of the rows shown"]


def report(text: str) -> str:
    lines = text.splitlines()
    out: list[str] = []
    marks = [i for i, line in enumerate(lines) if f"{judge.P}:NAV:" in line]
    bounds = marks + [len(lines)]
    for n, start in enumerate(marks):
        chunk = "\n".join(lines[start:bounds[n + 1]])
        url = lines[start].split(f"{judge.P}:NAV:", 1)[1].strip()
        out.append(f"== {url}")
        out += smooth_block(chunk)
        result = scroll_stats.stats(chunk.splitlines())
        if "error" in result:
            out.append(f"  {result['error']}")
        else:
            out.append("  " + scroll_stats.render(url, result).replace("\n", "\n  "))
        table, _ = frame_run.summarise(chunk)
        out.append("  " + table.replace("\n", "\n  "))
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/webplus-bench")
    parser.add_argument("--url", action="append", help="an address to scroll (repeat); default: ars technica, webkit.org")
    parser.add_argument("--settle", type=float, default=25.0, help="seconds after the navigation before the wheel")
    parser.add_argument("--seconds", type=float, default=20.0, help="how long the wheel turns on each page")
    parser.add_argument("--notches", type=int, default=frame_run.NOTCHES)
    parser.add_argument("--pause", type=float, default=frame_run.PAUSE)
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=webplus_run.limits.BIG_APP_MEMORY)
    parser.add_argument("--step-timeout", type=float, default=300.0)
    parser.add_argument("--build", action="store_true", help="build the image first (2560x1440, LAZYOS_PERF=1)")
    parser.add_argument("--mode", default="2560x1440")
    parser.add_argument("--judge-only", metavar="SERIAL_LOG")
    args = parser.parse_args(argv)
    if args.judge_only:
        print(report(Path(args.judge_only).read_text(errors="replace")))
        return 0
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    if args.build:
        error = webplus_run.build_image({"LAZYOS_DISPLAY_MODE": args.mode, "LAZYOS_PERF": "1"}, False)
        if error:
            print(f"{judge.P}:BENCH:FAIL {error}")
            return 1
    if not webplus_run.IMAGE.is_file():
        print(f"{judge.P}:BENCH:FAIL no image: --build first")
        return 1
    steps = bench_session(args.url or DEFAULT_URLS, args.settle, args.seconds, args.step_timeout, args.notches, args.pause)
    ok, text = webplus_run.run_session(steps, out, args, screen=args.mode)
    print(f"session {'ok' if ok else 'FAILED (read the log)'}")
    result = report(text)
    (out / "bench.txt").write_text(result + "\n", encoding="utf-8")
    print(result)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
