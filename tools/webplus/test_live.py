#!/usr/bin/env python3
"""live_run.py's verdict, on the lines of a real first-visit hang and on a healthy log."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import live_run  # noqa: E402

#: From a hang reported on the Kaby Lake stick (2026-10-17): the NetworkProcess in ext2 for whole periods,
#: the page's process unresponsive behind it.
HANG = """\
WEBPLUS:INFO:TIME:nav=2:start=11138:commit=11922:finish=17547:load=6409
LOCK:WAIT lock=ext2 holder=43/fork:networkprocess thread=51/thread tgid=43 abi=linux nr=74 waiters=1 ms=627 comm=persistent
LOCK:WAIT lock=ext2 holder=51/thread:persistent thread=43/fork tgid=43 abi=linux nr=18 waiters=1 ms=583 comm=networkprocess
PERF:CPU tgid=43 pct=38 user=6 kernel=32 class=normal demotions=16 comm=networkprocess
ENOSYS 285 fallocate count=39 comm=background
WEBPLUS:WEBPROCESS:UNRESPONSIVE:41
PERF:CPU tgid=43 pct=53 user=0 kernel=53 class=normal demotions=25 comm=networkprocess
PERF:CPU tgid=43 pct=59 user=0 kernel=59 class=normal demotions=46 comm=networkprocess
SCHED:STUCK tgid=43 thread=43/fork abi=linux nr=74 ms=5150 cpu_ms=550 krip=0xffff8000001621ce urip=0x79a28e7 comm=fork
"""

HEALTHY = """\
WEBPLUS:INFO:TIME:nav=2:start=11138:commit=11922:finish=17547:load=6409
PERF:CPU tgid=41 pct=33 user=32 kernel=1 class=normal demotions=34 comm=webprocess
PERF:CPU tgid=43 pct=11 user=6 kernel=5 class=normal demotions=0 comm=networkprocess
SCHED:STUCK tgid=36 thread=36/fork abi=linux nr=59 ms=2200 cpu_ms=490 krip=0xffff80000007e5e9 urip=0x79a28e7 comm=fork
LOCK:WAIT lock=ext2 holder=43/fork:networkprocess thread=51/thread tgid=43 abi=linux nr=74 waiters=1 ms=120 comm=persistent
"""


class Verdict(unittest.TestCase):
    def test_the_reported_hang_is_found_in_all_its_signs(self):
        hang, facts = live_run.verdict(HANG)
        text = "\n".join(hang)
        self.assertIn("unresponsive", text)
        self.assertIn("networkprocess (tgid 43) spent 59% of the core in the kernel", text)
        self.assertIn("persistent waited 627 ms for ext2", text)
        self.assertIn("SCHED:STUCK", text)
        self.assertEqual(facts, ["fallocate answered ENOSYS 39 times"])

    def test_a_browser_program_that_aborts_is_a_finding_and_a_clean_exit_or_a_kill_is_not(self):
        abort = "PROC:EXIT tgid=39 thread=39/fork status=134 sig=6 by=- parent=30 cpu_ms=900 comm=webprocess\n"
        self.assertEqual(live_run.verdict(abort)[0], ["webprocess (tgid 39) died of signal 6"])
        segv = "PROC:EXIT tgid=30 thread=30/main status=139 sig=11 by=- parent=1 cpu_ms=5 comm=lazywebplus.elf\n"
        self.assertEqual(live_run.verdict(segv)[0], ["lazywebplus.elf (tgid 30) died of signal 11"])
        killed = "PROC:EXIT tgid=39 thread=39/fork status=0 sig=15 by=1 parent=30 cpu_ms=900 comm=webprocess\n"
        clean = "PROC:EXIT tgid=39 thread=39/fork status=0 sig=- by=- parent=30 cpu_ms=900 comm=webprocess\n"
        other = "PROC:EXIT tgid=40 thread=40/fork status=134 sig=6 by=- parent=20 cpu_ms=9 comm=busybox\n"
        self.assertEqual(live_run.verdict(killed + clean + other)[0], [])

    def test_a_busy_but_healthy_log_is_clean(self):
        hang, facts = live_run.verdict(HEALTHY)
        self.assertEqual((hang, facts), ([], []))

    def test_a_session_visits_each_address_and_watches(self):
        steps = live_run.live_session(["https://a/", "https://b/"], 30.0, 10.0)
        self.assertEqual([s["shot"] for s in steps if "shot" in s], ["site0", "site1"])
        self.assertEqual(sum(1 for s in steps if s.get("wait") == 30.0), 2)


if __name__ == "__main__":
    unittest.main()
