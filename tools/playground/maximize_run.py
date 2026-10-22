#!/usr/bin/env python3
"""Maximize the Playground's window on a 1920x1080 display and judge how the panes take the room.

`run.py`'s image is a 1280x720 screen, where maximizing a 1180x660 window gains almost nothing. This
builds the same developer-tool desktop with `LAZYOS_DISPLAY_MODE=1920x1080` (the kernel switches the
display after boot; `xuid` keeps scale 1 below 2560x1440), double-clicks the title bar (maximize), opens
a page, double-clicks again (restore) and judges the probe lines: the doc pane and the editor grew by
what the window did and the Explorer kept its width (`judge.judge_maximize`). Read the screenshots in
`shots/playground/maximize/` (`max_before`, `max_after`, `max_page`, `max_restored`).

    python tools/playground/maximize_run.py                  # build the 1920x1080 image, boot, judge
    python tools/playground/maximize_run.py --no-build       # reuse target/lazyos.img (a 1920x1080 build)
    python tools/playground/maximize_run.py --image IMG      # another image

It overwrites `target/lazyos.img` like `run.py` does. Exit status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import judge  # noqa: E402
import run  # noqa: E402

MODE = "1920x1080"
SCRIPT = ROOT / "tools" / "screenshot" / "examples" / "playground_maximize.json"
SHOTS = ["max_before", "max_after", "max_page", "max_restored"]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--image", default=str(run.IMAGE))
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--qemu")
    args = parser.parse_args(argv)
    if not args.no_build:
        env = dict(os.environ, **run.BUILD_ENV, LAZYOS_DISPLAY_MODE=MODE)
        build = [run.PY, str(ROOT / "tools" / "run_demo.py"), "--devtools", "--autologin", "user", "--build-only"]
        if subprocess.run(build, cwd=ROOT, env=env, capture_output=True, text=True).returncode != 0:
            return run.fail("the build failed")
    out = ROOT / "shots" / "playground" / "maximize"
    command = [run.PY, str(run.SESSION), "--image", args.image, "--out", str(out), "--timeout", str(args.timeout),
               "--script", str(SCRIPT), "--screen", MODE, "--accel", args.accel,
               "--fail-on", r"PLAYGROUND:(BIND|RUN):FAIL", "--fail-on", "LazyOS PANIC"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    if subprocess.call(command, cwd=ROOT) != 0:
        return run.fail("the session did not finish")
    text = (out / "serial.log").read_text(errors="replace")
    problems = judge.judge_serial("maximize", text, judge.interface_names(), first_boot=not args.no_build)
    for shot in SHOTS:
        if not (out / f"shot_{shot}.png").is_file():
            problems.append(f"screenshot {shot} is missing")
    for problem in problems:
        print(f"PLAYGROUND:MAXIMIZE:FAIL: {problem}", file=sys.stderr)
    if not problems:
        print("PLAYGROUND:MAXIMIZE:PASS")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
