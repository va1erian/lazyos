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
#: The most the user memory charge may grow per Reload cycle, in MiB. The leak patch 0021 works around
#: is of a proxy's kilobytes (a mapping's length counts, so they hide in the noise); what this bound
#: catches is a whole process that was killed and never collected (120 MiB a cycle).
QUOTA_SLOPE_MIB = 16
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


def quota_after_exits(text: str) -> list[int]:
    """The user memory charge (MiB) of each `INFO:QUOTA` reading taken after a web process ended, one
    per ending: the reading after a load that follows the process's collection, where an uncollected
    process still shows. (Several readings after one ending count once, the first.)"""
    readings = []
    ended = False
    for line in text.splitlines():
        if re.search(rf"{P}:PROCESS:EXIT:web:", line):
            ended = True
        found = re.search(rf"{P}:INFO:QUOTA:user_memory_mib=(\d+)", line)
        if found and ended:
            readings.append(int(found.group(1)))
            ended = False
    return readings


def judge_quota(text: str) -> list[str]:
    """The charge must not climb by a process per Reload: at least CYCLES + 1 readings (one per Reload,
    each after the process it replaced was collected), and the growth from the first to the last at most
    QUOTA_SLOPE_MIB a cycle."""
    used = quota_after_exits(text)
    if len(used) < CYCLES + 1:
        return [f"expected {CYCLES + 1} user memory readings taken after a web process ended, saw {used}"]
    growth = used[-1] - used[0]
    if growth > QUOTA_SLOPE_MIB * (len(used) - 1):
        return [f"the user memory charge grew {growth} MiB over {len(used) - 1} Reloads ({used}): a process is not freed"]
    return []


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
    # Every relaunch is announced, whatever id the kernel gave it (the lowest free slot is often the dead
    # process's own): the first process, the Reload's, the cycles' (Stop starts none).
    ups = pids(text, "WEBPROCESS", "UP")
    if len(ups) < 2 + CYCLES:
        problems.append(f"expected {2 + CYCLES} WEBPROCESS:UP (the first process, then one per Reload), saw {ups}")
    gone = re.findall(rf"{P}:WEBPROCESS:GONE:(\d+):(\w+)", text)
    for pid, why in gone:
        if why != "requested":
            problems.append(f"the page's process {pid} ended for a reason other than the window's: {why}")
    if len(gone) < 2 + CYCLES:
        problems.append(f"expected {2 + CYCLES} page processes ended on request (Reload, {CYCLES} more, Stop), saw {gone}")
    # Every web process that ended has its proxy kept (patch 0021) and counted, in order.
    kept_lines = re.findall(rf"{P}:PROCESS:KEPT:(\d+):(\d+)", text)
    kept = [int(count) for _, count in kept_lines]
    if kept != list(range(1, len(kept) + 1)) or len(kept) < len(gone):
        problems.append(f"the kept-proxy count {kept} does not follow the {len(gone)} web processes that ended")
    # ...and names the process that ended (not 0, not another one).
    kept_pids = [pid for pid, _ in kept_lines]
    if kept_pids != [pid for pid, _ in gone][:len(kept_pids)]:
        problems.append(f"the kept proxies name processes {kept_pids}, the ones that ended were {[pid for pid, _ in gone]}")
    problems += judge_quota(text)
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
