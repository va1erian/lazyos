#!/usr/bin/env python3
"""Judge the keyboard-grab session (`docs/input-plan.md`, I3; issue #397).

`tools/screenshot/examples/doom_grab.json` opens Doom, presses Ctrl+Esc
(the start menu opens: no grab yet), maximizes the window (Doom asks for a
keyboard grab, the compositor approves it), presses Ctrl+Esc and Alt+Tab
(both must reach the game: the start menu stays shut), then the reserved
escape chord Ctrl+Alt+Esc (the grab ends and is not taken back), and
Ctrl+Esc once more (the start menu opens again). This script checks the
serial log says exactly that:

    python tools/input/verify_grab.py shots/doom_grab/serial.log

Exit status 0 on PASS, 1 on FAIL.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


def lines_of(log: str) -> list[str]:
    # Serial lines can be interleaved mid-line; judge each marker by itself.
    return [line[line.find(m):] for line in log.splitlines()
            for m in ("INPUTD:", "XUID:", "DOOM:", "SHELL:", "LABEL:") if m in line]


def first(lines: list[str], pattern: str, start: int = 0) -> int | None:
    rx = re.compile(pattern)
    return next((i for i in range(start, len(lines)) if rx.search(lines[i])), None)


def judge(log: str) -> list[str]:
    lines = lines_of(log)
    failures: list[str] = []
    # Each group follows the previous one; inside a group the processes
    # print in whatever order the scheduler runs them.
    order = [
        [("the key-state page", r"^DOOM:KEYSTATE:PASS")],
        [("the start menu without a grab", r"^SHELL:MENU:OPEN")],
        [("the grab request", r"^INPUTD:GRAB:REQUEST session=\d+ surface=\d+")],
        [("the compositor's approval", r"^XUID:GRAB:ASK surface=\d+ allow=1")],
        [("the grab", r"^INPUTD:GRAB:ON session=\d+ surface=\d+")],
        [("the client's grant event", r"^DOOM:GRAB:ON"),
         ("the compositor's view", r"^XUID:GRAB:HELD surface=\d+")],
        [("the escape chord", r"^INPUTD:GRAB:ESCAPE")],
        [("the end of the grab", r"^INPUTD:GRAB:OFF session=\d+ reason=4")],
        [("the client's release", r"^DOOM:GRAB:OFF reason=4"),
         ("the compositor's release", r"^XUID:GRAB:HELD surface=none"),
         ("the start menu after the escape", r"^SHELL:MENU:OPEN")],
    ]
    at = 0
    found: dict[str, int] = {}
    for group in order:
        latest = at
        for name, pattern in group:
            index = first(lines, pattern, at)
            if index is None:
                failures.append(f"missing {name} ({pattern}) after line {at}")
                return failures
            found[name] = index
            latest = max(latest, index)
        at = latest + 1
    grabbed = found.get("the grab")
    escaped = found.get("the escape chord")
    if grabbed is not None and escaped is not None:
        during = [line for line in lines[grabbed:escaped] if line.startswith("SHELL:MENU:OPEN")]
        if during:
            failures.append(f"the start menu opened {len(during)} time(s) under the grab")
    if escaped is not None and first(lines, r"^INPUTD:GRAB:ON", escaped) is not None:
        failures.append("the grab was taken back after the escape chord")
    denied = [line for line in lines if line.startswith("LABEL:DENY")]
    if denied:
        failures.append(f"{len(denied)} LABEL:DENY line(s), first: {denied[0]}")
    released = [line for line in lines if line.startswith("DOOM:KEYSTATE:FAIL")]
    if released:
        failures.append(released[0])
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    args = parser.parse_args()
    failures = judge(args.log.read_text(errors="replace"))
    for failure in failures:
        print("  -", failure)
    print("grab session:", "FAIL" if failures else "PASS")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
