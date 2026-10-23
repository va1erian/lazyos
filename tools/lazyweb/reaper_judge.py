"""The launcher's reaper threads sleep (issue #887), judged from a boot's serial log.

NOT PART OF THE HARNESS ANY MORE: LazyWeb no longer links WebKit's UI process or prints the heartbeat's
`INFO:REAPER` line. The engine's processes are webd's (os.lazy.webd), and `run.py` does not call this
judge. The module and its test stay only until CI (`.github/workflows/mkdisk.yml`) stops running
`test_reaper_judge.py`; delete both together then.

Each engine process the launcher starts has a reaper thread. It used to poll `waitpid(WNOHANG)` every
50 ms for as long as its child lived: twenty wakeups a second per process, on an idle desktop too. It now
blocks in `waitid(P_PID, pid, WEXITED | WNOWAIT)`, so it wakes once, when its child ends. The UI process's
heartbeat prints the number of wakeups (`LAZYWEB:INFO:REAPER:wakeups=<n>`) at 10 s,
at 30 s and whenever it has changed since; the boot is judged on the last one: at most a few wakeups per
process launched, where a poll would have made hundreds by then.

The line is printed at heartbeat tick 10 (10 s after the window starts), so a boot shorter than that has
no line and fails: the harness's boots are far longer.

Pure text in, problems out: `test_reaper_judge.py` shows it fails when it should.
"""

from __future__ import annotations

import re

P = "LAZYWEB"
#: Wakeups a launched process may cost: the end of its child, a handful for an id reused by a newer
#: launch (a reaper that finds the child gone and goes back to sleep), none of them a poll.
PER_LAUNCH = 6
SLACK = 4


def wakeups(serial: str) -> list[int]:
    """The wake counts of the log's `INFO:REAPER` lines, in order."""
    # Anywhere on the line: the console may interleave (as `judge.lines` does).
    return [int(n) for n in re.findall(rf"{P}:INFO:REAPER:wakeups=(\d+)", serial)]


def launches(serial: str) -> int:
    """Engine processes the launcher started in the boot."""
    return len(re.findall(rf"{P}:PROCESS:LAUNCH:", serial))


def judge_reaper(serial: str) -> list[str]:
    """Problems with the reaper threads' wakeups in one boot."""
    counts = wakeups(serial)
    if not counts:
        return [f"no {P}:INFO:REAPER line: the heartbeat prints one at 10 s (or the launcher's counter is missing)"]
    started = launches(serial)
    allowed = PER_LAUNCH * started + SLACK
    problems = []
    if not started:
        problems.append(f"no {P}:PROCESS:LAUNCH line: no engine process to judge the reapers by")
    if counts[-1] > allowed:
        problems.append(
            f"the reaper threads woke {counts[-1]} times with {started} engine processes launched "
            f"(at most {allowed}): a reaper polls instead of blocking"
        )
    return problems
