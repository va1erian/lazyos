#!/usr/bin/env python3
"""A page that hangs, through LazyWeb+ (docs/lazywebplus.md, issue #862): build the desktop image,
browse a page whose button runs a loop that never ends, and judge that the browser says so and lets
the person out. LazyWeb+ is a client of the resident daemon webd, which owns the page's process: the
engine's markers are webd's (``WEBD:NOTE:...``, ``WEBD:LOST``).

The image is `run.py`'s (the apps built first: ``python tools/webkit/deps.py``, ``python
tools/webkit/port.py --only WebProcess NetworkProcess webd-app``, ``python tools/webkit/webd.py``, then
``python tools/xui/build.py``). The page is
``tools/webtest/fixtures/theoldnet.com/webplus/hang.html``, served over HTTPS from the host with the run's
throwaway CA like the rest of LazyWeb+'s pages.

One boot (``hang_session.hang``): the click starts the loop, WebKit's ping goes unanswered and the
window's bar comes up; Wait puts it away; the toolbar's Reload ends the stuck process and loads the
page in a new one; hung again, the bar's Stop ends that process and leaves the page stopped (webd
reports the view lost); the next load makes a new view and the page loads in it; the window closes and
every view and page process is accounted for (``hang_judge.py``).

    python tools/webplus/hang_run.py                # build, boot, judge
    python tools/webplus/hang_run.py --no-build     # the image and certificates of the last run
    python tools/webplus/hang_run.py --judge-only DIR

Needs the ports ``run.py`` needs (80 and 443 on 127.0.0.1). Screenshots and logs go to
``shots/webplus-hang``; read the ``h*`` pictures: the bar over the page, the page again, the stopped
page, the page loaded again. Every run recreates ``target/lazyos.img``'s OS volume.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import run as base  # noqa: E402  (also puts tools/webtest, tools/abi and this directory on the path)
import certs  # noqa: E402
import hang_judge as h  # noqa: E402
import hang_session  # noqa: E402
import sites  # noqa: E402
from lazygui import limits  # noqa: E402

ROOT = base.ROOT


def verdict(text: str, ran: bool) -> int:
    ok = base.report("SESSION", "the boot ran to its end", [] if ran else ["the session did not finish (see summary.json)"])
    ok = base.report("HANG", "the stuck page was said to be stuck, Wait put the bar away, Reload and Stop ended "
                     "its process, the next load made a new view and every view and page process is accounted for",
                     h.judge_hang(text)) and ok
    print(f"{base.judge.P}:HANG:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/webplus-hang")
    parser.add_argument("--no-build", action="store_true", help="reuse the image and certificates of the last run")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=limits.BIG_APP_MEMORY, help="guest RAM (default: the big-app profile's 4G)")
    parser.add_argument("--step-timeout", type=float, default=300.0, help="seconds per gate")
    parser.add_argument("--judge-only", metavar="DIR", help="judge a saved run (DIR/hang/serial.log)")
    args = parser.parse_args(argv)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    if args.judge_only:
        saved = Path(args.judge_only)
        return verdict((saved / "hang" / "serial.log").read_text(errors="replace"), True)
    out.mkdir(parents=True, exist_ok=True)
    problem = base.built()
    if problem:
        print(f"{base.judge.P}:HANG:HARNESS:FAIL {problem}")
        return 1
    busy = sites.ports_free((sites.HTTP_PORT, sites.HTTPS_PORT, base.judge.CLOSER_PORT))
    if busy:
        print(f"{base.judge.P}:HANG:HARNESS:FAIL host port(s) {busy} in use: {sites.port_hint(OSError('busy'))}")
        return 1
    files = certs.load(out / "certs") if args.no_build else certs.generate(out / "certs")
    if files is None or (args.no_build and not base.IMAGE.is_file()):
        print(f"{base.judge.P}:HANG:HARNESS:FAIL --no-build needs the image and certificates of an earlier run")
        return 1
    if not args.no_build:
        error = base.build_image({"LAZYOS_TLS_TEST_CA": str(files.ca), "LAZYOS_TLS_TEST_HOSTS": str(files.hosts)},
                                 False)
        if error:
            print(f"{base.judge.P}:HANG:HARNESS:FAIL {error}")
            return 1
    try:
        served = sites.Sites(files.leaf_cert, files.leaf_key, closer_port=base.judge.CLOSER_PORT)
    except OSError as error:
        print(f"{base.judge.P}:HANG:HARNESS:FAIL cannot start the host servers: {sites.port_hint(error)}")
        return 1
    try:
        ran, text = base.run_session(hang_session.hang(args.step_timeout), out / "hang", args,
                                    expected_failures=("webprocess:stopped", "The page was stopped",
                                                       "The page's process ended"))
    finally:
        served.close()
    return verdict(text, ran)


if __name__ == "__main__":
    sys.exit(main())
