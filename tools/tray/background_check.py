#!/usr/bin/env python3
"""Judge the background-start session (`tools/screenshot/examples/tray_background.json`,
issue #928): a resident app the session's autostart opens starts with its
tray icon only, and the Start menu still opens its window.

The image autostarts the Tray Demo (`LAZYOS_XUI_AUTOSTART=term,traydemo`).
From the serial log it checks:

* `init` started the demo and the Volume applet (both resident, both opened
  with the session) in the background: `INIT:LAUNCH:BACKGROUND app=<id>`;
* the demo knew it (`TRAYDEMO:BACKGROUND:PASS`) and put its icon on the tray
  (`SHELL:TRAY:SET app=os.lazy.traydemo`);
* until the menu launched it again (`INIT:LAUNCH:EXISTING app=os.lazy.traydemo`)
  it drew no frame (`TRAYDEMO:UP:PASS`) and the compositor had no window
  for it (`UI:RECT ... name=window:Tray Demo`);
* the menu launch reopened it (`TRAYDEMO:REOPEN:PASS via=reopen`) and its
  window then came up, and the Terminal (not resident) opened as usual.

    python tools/tray/background_check.py shots/tray_background/serial.log

Prints `TRAY:BACKGROUND:PASS` and exits 0 when everything holds.
"""

from __future__ import annotations

import sys
from pathlib import Path

DEMO = "os.lazy.traydemo"
VOLUME = "os.lazy.volume"
WINDOW = "name=window:Tray Demo"


def judge(log: str) -> list[str]:
    """Every failure the log shows; empty when the session passed."""
    failures = []
    for app in (DEMO, VOLUME):
        if f"INIT:LAUNCH:BACKGROUND app={app} " not in log:
            failures.append(f"init did not start {app} in the background")
    background = log.find("TRAYDEMO:BACKGROUND:PASS")
    if background < 0:
        failures.append("the demo did not report a background start")
    if f"SHELL:TRAY:SET app={DEMO}" not in log:
        failures.append("the demo's tray icon did not appear")
    if "TERM:UP:PASS" not in log:
        failures.append("the Terminal (not resident) did not open")
    reopen = log.find(f"INIT:LAUNCH:EXISTING app={DEMO}")
    if reopen < 0:
        return failures + ["the menu did not launch the running demo again"]
    before, after = log[:reopen], log[reopen:]
    if "TRAYDEMO:UP:PASS" in before:
        failures.append("the demo drew a window before it was opened from the menu")
    if WINDOW in before:
        failures.append("the compositor had a Tray Demo window before the menu opened it")
    if "TRAYDEMO:REOPEN:PASS via=reopen" not in after:
        failures.append("the menu launch did not reach the demo as a Reopen")
    if "TRAYDEMO:UP:PASS" not in after or WINDOW not in after:
        failures.append("the demo's window did not open from the menu")
    return failures


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__.strip().splitlines()[0])
        print("usage: background_check.py SERIAL_LOG")
        return 2
    log = Path(argv[1]).read_text(encoding="utf-8", errors="replace")
    failures = judge(log)
    for failure in failures:
        print(f"TRAY:BACKGROUND:FAIL {failure}")
    if failures:
        return 1
    print("TRAY:BACKGROUND:PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
