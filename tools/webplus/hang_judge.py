"""The verdict of LazyWeb+'s hung-page harness (`hang_run.py`, issue #862), as pure functions with
their own tests (`test_hang_judge.py`).

The page (`fixtures/theoldnet.com/webplus/hang.html`) runs a loop that never ends when its button is
clicked. The session then checks, in the one boot, that every step of the way is *said*:

1. WebKit's ping goes unanswered: `WEBPROCESS:UNRESPONSIVE:<pid>`, and the window puts its bar up
   (`HANG:SHOW`) with the page's process still running;
2. **Wait** puts the bar away (`HANG:ANSWER:wait`, `HANG:HIDE`) and ends nothing;
3. the toolbar's **Reload** still gets the page out (`HANG:ANSWER:reload`, `WEBPROCESS:RECOVER:<pid>:reload`,
   the process ends: `WEBPROCESS:GONE:<pid>:requested`, a new `WEBPROCESS:UP:<other pid>`) and the page
   loads again in that new process (a second `LOAD` and `TITLE:hang:ready`);
4. hung again, the bar's **Stop** ends that process and leaves the page stopped
   (`WEBPROCESS:RECOVER:<pid>:stop`, `GONE:<pid>:requested`, `FAIL:webprocess:stopped`);
5. the window closes and every process the launcher started is accounted for (`judge.judge_quit`).

`INFO:MAINLAG`, `STALL:main` and `STALL:window` are not failures here (a loaded host can be slow), but
the heartbeat must have started (`HEARTBEAT:UP`).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import judge as j  # noqa: E402

P = j.P
PAGE = j.SITE + "/webplus/hang.html"
READY = "hang:ready"
SPINNING = "hang:spinning"


def pids(text: str, kind: str, what: str) -> list[str]:
    """The pids of every `WEBPLUS:<kind>:<what>:<pid>...` marker, in order."""
    return re.findall(rf"{P}:{kind}:{what}:(\d+)", text)


def index(text: str, marker: str, start: int = 0) -> int:
    """Where `marker` first appears at or after `start`, or -1."""
    return text.find(marker, start)


def in_order(text: str, markers: list[str]) -> list[str]:
    """Problems for the markers that are missing or out of order (each after the one before)."""
    problems = []
    at = 0
    for marker in markers:
        found = index(text, marker, at)
        if found < 0:
            problems.append(f"no {marker!r}" + (f" after {markers[markers.index(marker) - 1]!r}" if at else ""))
            continue
        at = found + len(marker)
    return problems


def judge_hang(text: str) -> list[str]:
    problems: list[str] = []
    if f"{P}:HEARTBEAT:UP" not in text:
        problems.append(f"the heartbeat never started: no {P}:HEARTBEAT:UP")
    problems += in_order(text, [
        f"{P}:LOAD:{PAGE}", f"{P}:TITLE:{READY}", f"{P}:TITLE:{SPINNING}",
        f"{P}:WEBPROCESS:UNRESPONSIVE:", f"{P}:HANG:SHOW",
        f"{P}:HANG:ANSWER:wait", f"{P}:HANG:HIDE",
        f"{P}:HANG:ANSWER:reload", f"{P}:WEBPROCESS:RECOVER:", ":reload",
        f"{P}:WEBPROCESS:GONE:", ":requested", f"{P}:WEBPROCESS:UP:",
        f"{P}:TITLE:{READY}",
        f"{P}:WEBPROCESS:UNRESPONSIVE:", f"{P}:HANG:SHOW",
        f"{P}:HANG:ANSWER:stop", f"{P}:WEBPROCESS:RECOVER:", ":stop",
        f"{P}:FAIL:webprocess:stopped",
    ])
    if sum(1 for _ in re.finditer(rf"{P}:TITLE:{re.escape(READY)}", text)) < 2:
        problems.append(f"the page did not load (and title itself {READY!r}) a second time after Reload")
    ups = pids(text, "WEBPROCESS", "UP")
    if len(set(ups)) < 2:
        problems.append(f"Reload did not start a new process for the page: WEBPROCESS:UP pids {ups}")
    gone = re.findall(rf"{P}:WEBPROCESS:GONE:(\d+):(\w+)", text)
    for pid, why in gone:
        if why != "requested":
            problems.append(f"the page's process {pid} ended for a reason other than the window's: {why}")
    if len(gone) < 2:
        problems.append(f"expected two page processes ended on request (Reload, Stop), saw {gone}")
    # The process that hung is the one that was ended: the UNRESPONSIVE and the GONE name the same pid.
    for stuck in pids(text, "WEBPROCESS", "UNRESPONSIVE"):
        if stuck not in [pid for pid, _ in gone]:
            problems.append(f"the unresponsive process {stuck} was never reported gone")
    # Wait ends nothing: no RECOVER between its answer and the Reload's.
    wait_at = index(text, f"{P}:HANG:ANSWER:wait")
    reload_at = index(text, f"{P}:HANG:ANSWER:reload")
    if 0 <= wait_at < reload_at and f"{P}:WEBPROCESS:RECOVER:" in text[wait_at:reload_at]:
        problems.append("Wait ended the page's process")
    problems += j.judge_quit(text)
    return problems
