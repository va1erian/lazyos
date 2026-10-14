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
#: Hang-and-Reload cycles after the first (tools/webplus/hang_session.py).
CYCLES = 3
READY = "hang:ready"
SPINNING = "hang:spinning"


def pids(text: str, kind: str, what: str) -> list[str]:
    """The pids of every `WEBPLUS:<kind>:<what>:<pid>...` marker, in order."""
    return re.findall(rf"{P}:{kind}:{what}:(\d+)", text)


def in_order(text: str, patterns: list[str]) -> list[str]:
    """Problems for the markers (regular expressions, each a whole marker) that are missing or out of
    order: each is looked for after the end of the one before."""
    problems = []
    at = 0
    for pattern in patterns:
        found = re.compile(pattern).search(text, at)
        if not found:
            problems.append(f"no {pattern!r}" + (f" after {patterns[patterns.index(pattern) - 1]!r}" if at else ""))
            continue
        at = found.end()
    return problems


def judge_hang(text: str) -> list[str]:
    problems: list[str] = []
    if f"{P}:HEARTBEAT:UP" not in text:
        problems.append(f"the heartbeat never started: no {P}:HEARTBEAT:UP")
    pid = r"\d+"
    problems += in_order(text, [
        rf"{P}:LOAD:{re.escape(PAGE)}\b", rf"{P}:TITLE:{re.escape(READY)}\b", rf"{P}:TITLE:{re.escape(SPINNING)}\b",
        rf"{P}:WEBPROCESS:UNRESPONSIVE:{pid}\b", rf"{P}:HANG:SHOW\b",
        rf"{P}:HANG:ANSWER:wait\b", rf"{P}:HANG:HIDE\b",
        rf"{P}:HANG:ANSWER:reload\b", rf"{P}:WEBPROCESS:RECOVER:{pid}:reload\b",
        rf"{P}:WEBPROCESS:GONE:{pid}:requested\b", rf"{P}:WEBPROCESS:UP:{pid}\b",
        rf"{P}:TITLE:{re.escape(READY)}\b",
        rf"{P}:WEBPROCESS:UNRESPONSIVE:{pid}\b", rf"{P}:HANG:SHOW\b",
        rf"{P}:HANG:ANSWER:stop\b", rf"{P}:WEBPROCESS:RECOVER:{pid}:stop\b",
        rf"{P}:FAIL:webprocess:stopped\b",
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
    if len(gone) < 2 + CYCLES:
        problems.append(f"expected {2 + CYCLES} page processes ended on request (Reload, {CYCLES} more, Stop), saw {gone}")
    # Every web process that ended has its proxy kept (patch 0021) and counted, in order.
    kept = [int(count) for count in re.findall(rf"{P}:PROCESS:KEPT:\d+:(\d+)", text)]
    if kept != list(range(1, len(kept) + 1)) or len(kept) < len(gone):
        problems.append(f"the kept-proxy count {kept} does not follow the {len(gone)} web processes that ended")
    # The UI process does not grow by more than a few cycles' worth while it keeps them: the quota reading
    # after each load (a mapping's length counts, so a dead proxy's kilobytes are lost in it; what shows is a
    # leak of whole mappings).
    used = [int(mib) for mib in re.findall(rf"{P}:INFO:QUOTA:user_memory_mib=(\d+)", text)]
    if used and max(used) > min(used) + 400:
        problems.append(f"the user memory charge grew from {min(used)} to {max(used)} MiB over the run")
    # The process that hung is the one that was ended: the UNRESPONSIVE and the GONE name the same pid.
    for stuck in pids(text, "WEBPROCESS", "UNRESPONSIVE"):
        if stuck not in [pid for pid, _ in gone]:
            problems.append(f"the unresponsive process {stuck} was never reported gone")
    # Wait ends nothing: no RECOVER between its answer and the Reload's.
    wait_at = text.find(f"{P}:HANG:ANSWER:wait")
    reload_at = text.find(f"{P}:HANG:ANSWER:reload")
    if 0 <= wait_at < reload_at and f"{P}:WEBPROCESS:RECOVER:" in text[wait_at:reload_at]:
        problems.append("Wait ended the page's process")
    problems += j.judge_quit(text)
    return problems
