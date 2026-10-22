"""The verdict of LazyWeb+'s hung-page harness (`hang_run.py`, issue #862), as pure functions with
their own tests (`test_hang_judge.py`).

The page (`fixtures/theoldnet.com/webplus/hang.html`) runs a loop that never ends when its button is
clicked. LazyWeb+ is a client of `webd`, which owns the engine, so the evidence comes in two kinds: the
window's own markers (`WEBPLUS:HANG:*`, `LOAD`, `TITLE`) and the engine's, which webd relays as
`WEBD:NOTE:<line>` (`judge.engine_lines`), beside webd's `WEBD:VIEW:*` and `WEBD:LOST`. The session
then checks, in the one boot, that every step of the way is *said*:

1. WebKit's ping goes unanswered: `WEBPROCESS:UNRESPONSIVE:<pid>` (engine), and the window puts its bar
   up (`HANG:SHOW`) with the page's process still running;
2. **Wait** puts the bar away (`HANG:ANSWER:wait`, `HANG:HIDE`) and ends nothing;
3. the toolbar's **Reload** still gets the page out (`HANG:ANSWER:reload`, `WEBPROCESS:RECOVER:<pid>:reload`,
   the process ends: `PROCESS:EXIT:web:<pid>`, a new `PROCESS:LAUNCH:web:<other pid>`) and the page
   loads again in that new process (a second `LOAD` and `TITLE:hang:ready`); the view lives on;
4. hung again, the bar's **Stop** ends that process and leaves the page stopped
   (`WEBPROCESS:RECOVER:<pid>:stop`, `PROCESS:EXIT:web:<pid>`, and webd reports the view lost:
   `WEBD:LOST view=<id> reason=other`, the window blanks the page); the next load makes a new view
   (`WEBD:VIEW:CREATE` again) with a new web process, and the page loads in it;
5. the window closes and every view webd made for it is destroyed and every web process it launched
   has ended (`judge.judge_quit`; the network process is webd's and stays).

The old `FAIL:webprocess:stopped` is not required (webd may or may not relay it): the stop is proved by
the `RECOVER`, the process's end and the lost view. `INFO:*` lines (webd rates them: 120 per 10 s) and
`UI:STALL` are not failures here (a loaded host can be slow).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import judge as j  # noqa: E402

P = j.P
#: An engine marker in the raw log: webd's relay prefix, then the marker.
E = re.escape(j.NOTE)
PAGE = j.SITE + "/webplus/hang.html"
#: Hang-and-Reload cycles after the first (tools/webplus/hang_session.py).
CYCLES = 3
#: The most the user memory charge may grow per Reload cycle, in MiB. The leak patch 0021 works around
#: is of a proxy's kilobytes (a mapping's length counts, so they hide in the noise); what this bound
#: catches is a whole process that was killed and never collected (120 MiB a cycle).
QUOTA_SLOPE_MIB = 16
READY = "hang:ready"
SPINNING = "hang:spinning"
#: The loss reason webd names for a page process the window itself ended (the bar's Stop).
STOP_REASON = "other"


def pids(text: str, kind: str, what: str) -> list[str]:
    """The pids of every engine `<kind>:<what>:<pid>...` marker (behind webd's `WEBD:NOTE:`), in order."""
    return j.engine_pids(text, kind, what)


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
    """The user memory charge (MiB) of each engine `INFO:QUOTA` reading taken after a web process ended, one
    per ending: the reading after a load that follows the process's collection, where an uncollected
    process still shows. (Several readings after one ending count once, the first.)"""
    readings = []
    ended = False
    for line in j.engine_lines(text):
        if line.startswith("PROCESS:EXIT:web:"):
            ended = True
        found = re.match(r"INFO:QUOTA:user_memory_mib=(\d+)", line)
        if found and ended:
            readings.append(int(found.group(1)))
            ended = False
    return readings


def judge_quota(text: str) -> list[str]:
    """The charge must not climb by a process per Reload. webd prints the readings (the same user's quota
    as the window's) under a rate budget, so as many as there are count: at least two, taken after a web
    process ended, and the growth from the first to the last at most QUOTA_SLOPE_MIB a reading."""
    used = quota_after_exits(text)
    if len(used) < 2:
        return [f"expected at least two user memory readings taken after a web process ended, saw {used} "
                "(a reading is skipped when its process's PROCESS:EXIT came after it: a process ended after its reading)"]
    growth = used[-1] - used[0]
    if growth > QUOTA_SLOPE_MIB * (len(used) - 1):
        return [f"the user memory charge grew {growth} MiB over {len(used) - 1} readings ({used}): a process is not freed"]
    return []


def recoveries(text: str) -> list[tuple[str, str]]:
    """(pid, how) of every engine `WEBPROCESS:RECOVER:<pid>:<reload|stop>`, in order."""
    return re.findall(r"^WEBPROCESS:RECOVER:(\d+):(\w+)", j.engine_text(text), re.M)


def judge_ended(text: str) -> list[str]:
    """What was hung was ended: after every `UNRESPONSIVE:<pid>` and every `RECOVER:<pid>` the same process
    has its `PROCESS:EXIT:web:<pid>`, and the window's Wait ended nothing."""
    problems = []
    engine = j.engine_text(text)
    for kind in ("WEBPROCESS:UNRESPONSIVE", "WEBPROCESS:RECOVER"):
        for found in re.finditer(rf"^{kind}:(\d+)", engine, re.M):
            pid = found.group(1)
            if not re.search(rf"^PROCESS:EXIT:web:{pid}\b", engine[found.end():], re.M):
                problems.append(f"the page's process {pid} ({kind.split(':')[-1].lower()}) never ended: "
                                f"no {j.NOTE}PROCESS:EXIT:web:{pid} after it")
    # Wait ends nothing: no RECOVER between its answer and the Reload's.
    wait_at = text.find(f"{P}:HANG:ANSWER:wait")
    reload_at = text.find(f"{P}:HANG:ANSWER:reload")
    if 0 <= wait_at < reload_at and re.search(rf"{E}WEBPROCESS:RECOVER:", text[wait_at:reload_at]):
        problems.append("Wait ended the page's process")
    return problems


def judge_kept(text: str, ended: list[str]) -> list[str]:
    """The proxy of every web process that ended is kept (patch 0021) and counted, in order; webd may not
    relay the lines, so they are judged only when there are some."""
    kept_lines = re.findall(r"^PROCESS:KEPT:(\d+):(\d+)", j.engine_text(text), re.M)
    if not kept_lines:
        return []
    problems = []
    kept = [int(count) for _, count in kept_lines]
    if kept != list(range(1, len(kept) + 1)):
        problems.append(f"the kept-proxy count {kept} does not count up from 1")
    # ...and names a process that ended (not 0, not another one).
    stray = [pid for pid, _ in kept_lines if pid not in ended]
    if stray:
        problems.append(f"the kept proxies name processes {stray}, the ones that ended were {ended}")
    return problems


def judge_hang(text: str) -> list[str]:
    problems: list[str] = []
    if j.WEBD_UP not in text:
        problems.append(f"no {j.WEBD_UP}: the web engine daemon never came up")
    if not pids(text, "NETWORKPROCESS", "UP"):
        problems.append(f"no {j.NOTE}NETWORKPROCESS:UP:<pid>")
    problems += j.judge_no_fault(text)
    pid = r"\d+"
    problems += in_order(text, [
        rf"{P}:LOAD:{re.escape(PAGE)}\b", rf"{P}:TITLE:{re.escape(READY)}\b", rf"{P}:TITLE:{re.escape(SPINNING)}\b",
        rf"{E}WEBPROCESS:UNRESPONSIVE:{pid}\b", rf"{P}:HANG:SHOW\b",
        rf"{P}:HANG:ANSWER:wait\b", rf"{P}:HANG:HIDE\b",
        rf"{P}:HANG:ANSWER:reload\b", rf"{E}WEBPROCESS:RECOVER:{pid}:reload\b",
        rf"{E}PROCESS:EXIT:web:{pid}\b", rf"{E}PROCESS:LAUNCH:web:{pid}\b",
        rf"{P}:TITLE:{re.escape(READY)}\b",
        rf"{E}WEBPROCESS:UNRESPONSIVE:{pid}\b", rf"{P}:HANG:SHOW\b",
        rf"{P}:HANG:ANSWER:stop\b", rf"{E}WEBPROCESS:RECOVER:{pid}:stop\b",
        rf"{E}PROCESS:EXIT:web:{pid}\b", rf"WEBD:LOST view={pid} reason={STOP_REASON}\b",
        # The next load makes a new view, with a new page process, and the page loads in it.
        rf"WEBD:VIEW:CREATE client={re.escape(j.CLIENT_LABEL)} view={pid}\b", rf"{E}PROCESS:LAUNCH:web:{pid}\b",
        rf"{P}:TITLE:{re.escape(READY)}\b",
    ])
    if sum(1 for _ in re.finditer(rf"{P}:TITLE:{re.escape(READY)}", text)) < 2:
        problems.append(f"the page did not load (and title itself {READY!r}) a second time after Reload")
    # Every relaunch is announced, whatever id the kernel gave it (the lowest free slot is often the dead
    # process's own): the first process, the Reload's, the cycles', and the one after Stop.
    launches = pids(text, "PROCESS:LAUNCH", "web")
    if len(launches) < 3 + CYCLES:
        problems.append(f"expected {3 + CYCLES} page processes launched (the first, one per Reload, the one after "
                        f"Stop), saw {launches}")
    exits = pids(text, "PROCESS:EXIT", "web")
    if len(exits) < 3 + CYCLES:
        problems.append(f"expected {3 + CYCLES} page processes ended (Reload, {CYCLES} more, Stop, then the "
                        f"window's close), saw {exits}")
    recovered = recoveries(text)
    if [how for _, how in recovered] != ["reload"] * (1 + CYCLES) + ["stop"]:
        problems.append(f"expected {1 + CYCLES} recoveries by Reload then one by Stop, saw {recovered}")
    # The window's Stop is the one reason a view is lost; a crash or memory loss is not its doing.
    lost = [(int(view), reason) for view, reason in j.VIEW_LOST.findall(text)]
    for view, reason in lost:
        if reason != STOP_REASON:
            problems.append(f"webd lost view {view} for a reason other than the window's Stop: {reason}")
    if len(lost) != 1:
        problems.append(f"expected one lost view (the Stop's), saw {lost}")
    created = [m for m in j.VIEW_CREATE.finditer(text) if m["client"] == j.CLIENT_LABEL]
    if len(created) != 2:
        problems.append(f"expected two views made for the window (the first and the one after Stop), saw "
                        f"{[m['view'] for m in created]}")
    # Anything but a request of the window that ended a page process is a fault.
    for why in re.findall(r"^WEBPROCESS:GONE:\d+:(\w+)", j.engine_text(text), re.M):
        if why != "requested":
            problems.append(f"a page process ended for a reason other than the window's: {why}")
    problems += judge_kept(text, exits)
    problems += judge_quota(text)
    problems += judge_ended(text)
    problems += j.judge_quit(text)
    return problems
