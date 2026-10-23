#!/usr/bin/env python3
"""Quit and start LazyWeb+ again and again: does a start-up leave the page waiting for good? (issue #865)

A WebProcess asks the UI for its NetworkProcess connection with a synchronous message; the NetworkProcess makes
a socket pair, keeps one end and sends the other to the UI. A start-up whose reply is lost hangs the start page
until WebKit gives up on the NetworkProcess (`WEBPROCESS:UNRESPONSIVE`, then `NETWORKPROCESS:GONE` about 15 s
later; webd relays both as `WEBD:NOTE:...`). LazyWeb+ is a client of the resident daemon webd, so the
NetworkProcess belongs to webd and outlives each close of the window: only the page's process and the view
are made again at each start. The kernel refusing that `sendmsg` (`ETOOMANYREFS`: musl prints "No error information") was such a loss:
the new connection's service-worker store handle could land in the pair before its end was sent.

Boots a plain image (the real sites over QEMU's user network), opens each `--url` once so the profile holds what
a person's does (a site with a service worker: its registration makes the NetworkProcess send a descriptor to
every new connection), then closes the window and starts it from the menu `--cycles` times, waiting on the start
page each time. Judged from the serial log: no `WEBPROCESS:UNRESPONSIVE`, no `NETWORKPROCESS:GONE`, no lost
view (`WEBD:LOST`), a view made and destroyed by webd for each start, no refused
`sendmsg` (`PROC:SCM:REFUSE`), no `Error sending IPC message`, and the start page loaded every time.

    python tools/webplus/relaunch_run.py --build                  # build, squoosh.app, six relaunches
    python tools/webplus/relaunch_run.py --cycles 10 --url https://www.youtube.com/
    python tools/webplus/relaunch_run.py --judge-only shots/webplus-relaunch/serial.log --cycles 6
"""

from __future__ import annotations

import argparse
import importlib.util
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
for sub in ("tools/webtest", "tools/abi", "tools"):
    sys.path.insert(0, str(ROOT / sub))
sys.path.insert(0, str(HERE))

_spec = importlib.util.spec_from_file_location("webplus_run", HERE / "run.py")
webplus_run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(webplus_run)

import judge  # noqa: E402
import live_run  # noqa: E402
import session  # noqa: E402

#: A small site that installs a service worker on its first visit.
DEFAULT_URLS = ["https://squoosh.app/"]
#: The ad blocker's list is compiled and in use (`xui-app/webplus/src/blocker/`).
BLOCKER_READY = "WEBPLUS:BLOCK:READY:"
#: How long a start page may take to load after a start (the first execve of three 110 MB programs).
START_TIMEOUT = 180.0


def relaunch_session(urls: list[str], cycles: int, wait: float, settle: float) -> list[dict]:
    """Visit `urls`, then `cycles` times: close the window, start it from the menu, wait on the start page."""
    loaded = f"{judge.P}:LOAD:{judge.START}"
    steps = [*session.boot_gates(300), {"wait_for": loaded, "timeout": 300},
             {"wait_for": BLOCKER_READY, "timeout": 300}]
    for index, url in enumerate(urls):
        steps += [*session.navigate(url, f"{judge.P}:NAV:", timeout=60), {"wait": wait}, {"shot": f"site{index}"}]
    for cycle in range(1, cycles + 1):
        steps += [
            # Alt+F4 asks the focused window to close, like its X (xuid `keys.rs`).
            {"key_down": "alt"}, {"key": "f4", "until": f"{judge.P}:QUIT:", "timeout": 60}, {"key_up": "alt"},
            {"wait_for": "INIT:LAUNCH:EXIT app=os.lazy.lazywebplus", "occurrence": cycle, "timeout": 60},
            {"wait": 2.0},
            {"click_at": {"target": "taskbar:start"}, "until": "SHELL:MENU:OPEN", "timeout": 30, "retries": 2},
            {"move_to": {"menu": "Internet"}, "until": "SHELL:SUBMENU:OPEN category=internet", "timeout": 30,
             "retries": 1},
            {"wait": 0.5},
            # The keyboard from here (issue #648): Right moves into the submenu with its first app lit
            # (LazyWeb+), Enter starts it. A pointer path to the row can cross another category and switch
            # the submenu under it.
            {"key": "right"},
            {"wait": 0.5},
            {"key": "enter", "until": "SHELL:LAUNCH:PASS app=os.lazy.lazywebplus", "timeout": 60},
            {"wait_for": f"{judge.P}:UP:PASS", "occurrence": cycle + 1, "timeout": START_TIMEOUT},
            {"wait_for": loaded, "occurrence": cycle + 1, "timeout": START_TIMEOUT},
            {"wait": settle},
            {"shot": f"start{cycle}"},
        ]
    return steps


def verdict(text: str, cycles: int) -> list[str]:
    """What went wrong, from a serial log of `relaunch_session` (empty: nothing): `live_run`'s hang signs (an
    unresponsive page, a refused sendmsg, a lost IPC message, a crash, a stuck call, an `ENOSYS 285`: the HTTP cache
    writing its bodies inline), a NetworkProcess WebKit
    ended, and a start that did not reach its start page.

    A fresh profile's first start downloads and compiles the ad blocker's list (about 60 000 rules), which keeps
    the UI process busy for seconds: the signs that are only slowness (an unresponsive page, a stuck call, a lock
    wait) are judged from the list's `BLOCK:READY` on. A lost or refused message counts anywhere."""
    warm = text.find(BLOCKER_READY)
    found, _ = live_run.verdict(text[warm:] if warm >= 0 else text)
    if warm > 0:
        early, _ = live_run.verdict(text[:warm])
        found += [sign for sign in early if "SCM:REFUSE" in sign or "IPC message" in sign or "signal" in sign]
    for pid, reason in re.findall(r"^NETWORKPROCESS:GONE:(-?\d+):(\S+)", judge.engine_text(text), re.M):
        found.append(f"the NetworkProcess (pid {pid}) was ended ({reason})")
    views, live = judge.client_views(text)
    if len(views) < cycles + 1:
        found.append(f"webd made {len(views)} view(s) for the window, not {cycles + 1}")
    # The last start is still open at the end of the run; every earlier view went with its window.
    found += [f"webd's view {view} of a closed window was never destroyed" for view in live if view != views[-1]]
    starts = text.count(f"{judge.P}:LOAD:{judge.START}")
    if starts < cycles + 1:
        found.append(f"the start page loaded {starts} times, not {cycles + 1}")
    return found


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/webplus-relaunch")
    parser.add_argument("--url", action="append", help="a site to visit first (repeat); default: squoosh.app")
    parser.add_argument("--cycles", type=int, default=6, help="how many times to close and start the browser")
    parser.add_argument("--wait", type=float, default=30.0, help="seconds on each site (its service worker installs)")
    parser.add_argument("--settle", type=float, default=20.0, help="seconds on each start page")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=webplus_run.limits.BIG_APP_MEMORY)
    parser.add_argument("--build", action="store_true", help="build the image first (no test host mappings)")
    parser.add_argument("--mode", default="2560x1440", help="the display mode of a built image")
    parser.add_argument("--judge-only", metavar="SERIAL_LOG")
    args = parser.parse_args(argv)
    if args.judge_only:
        text = Path(args.judge_only).read_text(errors="replace")
    else:
        out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
        if args.build:
            error = webplus_run.build_image({"LAZYOS_DISPLAY_MODE": args.mode}, False)
            if error:
                print(f"{judge.P}:RELAUNCH:FAIL {error}")
                return 1
        if not webplus_run.IMAGE.is_file():
            print(f"{judge.P}:RELAUNCH:FAIL no image: build one first (--build)")
            return 1
        steps = relaunch_session(args.url or DEFAULT_URLS, args.cycles, args.wait, args.settle)
        ok, text = webplus_run.run_session(steps, out, args, screen=args.mode)
        print(f"session {'ok' if ok else 'FAILED (it may have stopped on a marker; read the log)'}")
    found = verdict(text, args.cycles)
    if not args.judge_only and not ok:
        found.append(f"the session did not finish ({out / 'summary.json'} says why)")
    for finding in found:
        print(f"  HANG: {finding}")
    print(f"{judge.P}:RELAUNCH:" + ("FAIL" if found else "PASS"))
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
