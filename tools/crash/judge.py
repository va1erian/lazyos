#!/usr/bin/env python3
"""Judge an app-crash-notice run (issue #549) from its serial log.

The session (``tools/screenshot/examples/app_crash_notice.json``) installs
``org.lazy.crashload``, a LazyRAD app whose ``form_load`` always throws, opens
it from the start menu, presses *Restart* on the notice, then *Close*. The
verdict:

* every launch of the app ran exactly once: as many ``INIT:LAUNCH:EXIT`` as
  ``INIT:LAUNCH:PASS``, and ``init`` never restarted it (no
  ``INIT:RESTART:PASS name=org.lazy.crashload``, no ``EXHAUSTED``);
* the player told ``init`` why (``INIT:APP:REASON`` with the script's text)
  and ``init`` published each failure as a start-up failure with that reason;
* the shell heard each failure once and showed one notice per launch;
* *Restart* launched the app again and *Close* closed the notice.

Usage::

    python tools/crash/judge.py shots/crash/serial.log
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

APP = "org.lazy.crashload"
#: The text the sample's form_load throws.
REASON = "crash test: form_load always fails"


def count(log: str, pattern: str) -> int:
    return len(re.findall(pattern, log, re.MULTILINE))


def judge(log: str, launches: int = 2) -> list[str]:
    """The problems found; empty when the run passes. ``launches`` is how
    many times the session opened the app (menu, then *Restart*)."""
    app = re.escape(APP)
    problems = []
    started = count(log, rf"INIT:LAUNCH:PASS app={app} ")
    exited = count(log, rf"INIT:LAUNCH:EXIT app={app} status=[1-9]")
    if started != launches:
        problems.append(f"the app was launched {started} time(s), expected {launches}")
    if exited != started:
        problems.append(f"{started} launch(es) but {exited} failed exit(s): a restart loop?")
    if count(log, rf"INIT:RESTART:(PASS|EXHAUSTED) name={app}\b"):
        problems.append("init restarted the app (it must not restart a start-up failure)")
    reasons = count(log, rf"INIT:APP:REASON app={app} .*{re.escape(REASON)}")
    if reasons != launches:
        problems.append(f"init got the player's reason {reasons} time(s), expected {launches}")
    failed = count(log, rf"INIT:APP:FAILED app={app} status=\d+ startup=1 reason=.*{re.escape(REASON)}")
    if failed != launches:
        problems.append(f"{failed} start-up failure event(s) with the reason, expected {launches}")
    published = count(log, rf"INIT:APP:NOTICE:PASS app={app} matched=[1-9]")
    if published != launches:
        problems.append(f"{published} notice(s) reached a subscriber, expected {launches}")
    heard = count(log, rf"SHELL:FAILURE app={app} ")
    shown = count(log, rf"SHELL:NOTICE:OPEN app={app} title=")
    if heard != launches or shown != launches:
        problems.append(f"the shell heard {heard} and showed {shown} notice(s), expected {launches}")
    if launches > 1 and not count(log, rf"SHELL:NOTICE:RESTART app={app}"):
        problems.append("Restart on the notice did not relaunch the app")
    closed = count(log, rf"SHELL:NOTICE:CLOSE app={app}")
    if closed != shown:
        problems.append(f"{shown} notice(s) opened but {closed} closed")
    return problems


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1:
        print(__doc__, file=sys.stderr)
        return 2
    log = Path(args[0]).read_text(errors="replace")
    problems = judge(log)
    for problem in problems:
        print("  " + problem, file=sys.stderr)
    print(f"CRASH:NOTICE:{'PASS' if not problems else 'FAIL'}")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
