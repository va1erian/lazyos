#!/usr/bin/env python3
"""Build, boot and judge the app-crash notice (issue #549).

Builds a desktop image carrying the test package ``crashload.lzp`` (a
LazyRAD app whose ``form_load`` always throws) in ``/system/share/samples``
(``LAZYOS_TEST_PACKAGES``), runs ``tools/screenshot/examples/app_crash_notice.json``
(install it, open it from the start menu, *Restart* on the notice, then
*Close*) and judges the serial log with ``judge.py``. Read the screenshots
too: ``03_notice.png`` must show one "crashload stopped" window with the
reason, and nothing must flicker in between.

Usage::

    python tools/crash/run.py               # build, run, judge
    python tools/crash/run.py --no-build    # reuse target/lazyos.img
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

PY = sys.executable
PACKAGE = ROOT / "target" / "pkg" / "crashload.lzp"
SCRIPT = ROOT / "tools" / "screenshot" / "examples" / "app_crash_notice.json"


def run(argv: list[str], env: dict[str, str] | None = None) -> None:
    print("+ " + " ".join(argv), flush=True)
    subprocess.run(argv, cwd=ROOT, env=env, check=True)


def image_env() -> dict[str, str]:
    """A desktop with the Terminal, the UI probe and the test package."""
    env = dict(os.environ)
    env.update({
        "LAZYOS_RESET_OS": "1",
        "LAZYOS_DESKTOP": "1",
        "LAZYOS_XUI_AUTOSTART": "term",
        "LAZYOS_UI_PROBE": "1",
        "LAZYOS_TEST_PACKAGES": str(PACKAGE),
    })
    return env


def build() -> None:
    run([PY, "tools/xui/build.py"])
    run([PY, "tools/lazyrad/build.py", "--bin", "lrplay"])
    run([PY, "tools/lazyrad/package.py", "--app", "crashload", "--no-build", "--require"])
    run(["cargo", "build"], env=image_env())


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--accel", default="auto", help="QEMU accelerator (default: auto)")
    parser.add_argument("--out", type=Path, default=ROOT / "shots" / "crash")
    args = parser.parse_args(argv)
    if not args.no_build:
        build()
    argv = [PY, "tools/screenshot/qemu_session.py", "--image", "target/lazyos.img",
            "--out", str(args.out), "--script", str(SCRIPT), "--accel", args.accel]
    print("+ " + " ".join(argv), flush=True)
    finished = subprocess.run(argv, cwd=ROOT).returncode == 0
    serial = args.out / "serial.log"
    log = serial.read_text(errors="replace") if serial.is_file() else ""
    problems = ([] if finished else ["the session failed"]) + judge.judge(log)
    for problem in problems:
        print("  " + problem, file=sys.stderr)
    print(f"CRASH:NOTICE:{'PASS' if not problems else 'FAIL'}", flush=True)
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
