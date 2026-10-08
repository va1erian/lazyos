#!/usr/bin/env python3
"""Judge the logout session (`tools/screenshot/examples/tray_logout.json`,
issue #651): a logout stops the session's apps through their lifecycle
channel, with the same fixed 3 s grace as `init.Stop`, before the login
screen comes back.

For each logged-out session in the serial log it checks:

* `init` began the logout (`INIT:LOGOUT:BEGIN`) and finished it
  (`INIT:LOGOUT:PASS ... ticks=<n>`) within the bound: the 3 s grace plus the
  0.5 s reap allowance (`svcpolicy::logout_deadline`);
* the Volume applet, which watches its lifecycle, quit on its own
  (`VOLUME:QUIT:PASS`) before the sweep;
* the login screen was launched only after the sweep (`LOGIN:GREETER:PASS`
  after `INIT:LOGOUT:PASS`), and never had to be held past the bound.

The session runs two logouts with the Tray Demo open: in the first the demo
ignores `Quit` (`/tmp/traydemo-ignore-quit`) and must be killed at the grace
(`INIT:APP:QUIT:TIMEOUT`); in the second it must quit (`TRAYDEMO:QUIT:PASS`)
before `INIT:LOGOUT:PASS`.

    python tools/tray/logout_check.py shots/tray_logout/serial.log

Prints `TRAY:LOGOUT:PASS` and exits 0 when everything holds.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

#: `svcpolicy::QUIT_GRACE_TICKS + LOGOUT_REAP_TICKS`, at 100 Hz.
BOUND_TICKS = 350
DEMO = "os.lazy.traydemo"


def logouts(log: str) -> list[tuple[int, str]]:
    """Each logout's session id and its stretch of the log (from its
    `INIT:LOGOUT:BEGIN` to the next one)."""
    starts = [(m.start(), int(m.group(1)))
              for m in re.finditer(r"INIT:LOGOUT:BEGIN session=(\d+)", log)]
    ends = [start for start, _ in starts[1:]] + [len(log)]
    return [(session, log[start:end]) for (start, session), end in zip(starts, ends)]


def judge_one(session: int, part: str, ignored: bool) -> list[str]:
    """Failures of one logout; `ignored` when the demo was told to ignore Quit."""
    name = f"session {session}"
    done = re.search(rf"INIT:LOGOUT:PASS session={session} rows=\d+ tasks=\d+ ticks=(\d+)", part)
    if done is None:
        return [f"{name}: no INIT:LOGOUT:PASS with ticks"]
    failures = []
    if int(done.group(1)) > BOUND_TICKS:
        failures.append(f"{name}: the logout took {done.group(1)} ticks (bound {BOUND_TICKS})")
    swept = done.start()
    volume = part.find("VOLUME:QUIT:PASS")
    if not 0 <= volume < swept:
        failures.append(f"{name}: the Volume applet did not quit before the sweep")
    if "INIT:APP:QUIT:SENT app=os.lazy.volume" not in part[:swept]:
        failures.append(f"{name}: the Volume applet was not sent Quit")
    greeter = part.find("LOGIN:GREETER:PASS")
    if greeter < 0:
        failures.append(f"{name}: the login screen did not come back")
    elif greeter < swept:
        failures.append(f"{name}: the login screen came back before the apps were stopped")
    timeout = part.find(f"INIT:APP:QUIT:TIMEOUT app={DEMO}")
    if ignored:
        if "TRAYDEMO:QUIT:IGNORED" not in part[:swept]:
            failures.append(f"{name}: the demo was not sent Quit (no TRAYDEMO:QUIT:IGNORED)")
        if not 0 <= timeout < swept:
            failures.append(f"{name}: the demo ignoring Quit was not killed at the grace")
    else:
        quit_at = part.find("TRAYDEMO:QUIT:PASS")
        if not 0 <= quit_at < swept:
            failures.append(f"{name}: TRAYDEMO:QUIT:PASS did not come before INIT:LOGOUT:PASS")
        if timeout >= 0:
            failures.append(f"{name}: the demo was killed although it quit")
    return failures


def judge(log: str) -> list[str]:
    """Every failure in `log`; empty when the session passed."""
    found = logouts(log)
    if len(found) != 2:
        return [f"expected two logouts, found {len(found)}"]
    return judge_one(*found[0], ignored=True) + judge_one(*found[1], ignored=False)


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    failures = judge(Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace"))
    for failure in failures:
        print(f"FAIL: {failure}")
    print("TRAY:LOGOUT:PASS" if not failures else f"TRAY:LOGOUT:FAIL {len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
