#!/usr/bin/env python3
"""relaunch_run.py's verdict, on the lines of the start-up hang of issue #865 and on a healthy run."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import live_run  # noqa: E402
import relaunch_run  # noqa: E402

#: The first start of a boot on 2026-10-19 (shots/webplus-relaunch): the UI forwarded the WebProcess's new
#: NetworkProcess connection, whose queue already held a handle, and the kernel refused it; the start page
#: never loaded.
REFUSED = """\
INIT:LAUNCH:PASS app=os.lazy.lazywebplus pid=25 session=1
WEBPLUS:UP:PASS
PROC:SCM:REFUSE why=cycle tgid=25 count=1 comm=ReceiveQueue
Error sending IPC message: No error information
WEBPLUS:WEBPROCESS:UNRESPONSIVE:36
"""

#: The user's report of the same day: the reply lost the other way, the NetworkProcess killed by WebKit
#: 6 s later and the page back.
KILLED = """\
WEBPLUS:LOAD:about:start
Error sending IPC message: No error information
WEBPLUS:WEBPROCESS:UNRESPONSIVE:39
WEBPLUS:PROCESS:TERMINATE:network:41:group=0:pid=-1
WEBPLUS:NETWORKPROCESS:GONE:0:crash
WEBPLUS:WEBPROCESS:RESPONSIVE:39
WEBPLUS:LOAD:about:start
"""

HEALTHY = "".join(f"WEBPLUS:UP:PASS\nWEBPLUS:LOAD:about:start\nINIT:LAUNCH:EXIT app=os.lazy.lazywebplus\n"
                  for _ in range(3))


class Verdict(unittest.TestCase):
    def test_a_refused_send_and_the_hang_behind_it_are_found(self):
        found = "\n".join(relaunch_run.verdict(REFUSED, 2))
        self.assertIn("PROC:SCM:REFUSE why=cycle", found)
        self.assertIn("WebKit lost an IPC message", found)
        self.assertIn("unresponsive", found)
        self.assertIn("loaded 0 times, not 3", found)

    def test_a_killed_network_process_fails_even_when_the_page_comes_back(self):
        found = "\n".join(relaunch_run.verdict(KILLED, 1))
        self.assertIn("NetworkProcess (pid 0) was ended (crash)", found)
        self.assertIn("WebKit lost an IPC message", found)

    def test_a_healthy_run_passes(self):
        self.assertEqual(relaunch_run.verdict(HEALTHY, 2), [])

    def test_a_slow_start_is_not_a_hang_but_a_crash_is(self):
        slow = HEALTHY + ("LOCK:WAIT lock=ext2 holder=39/fork:fork thread=15/logd tgid=15 abi=native nr=28 "
                          "held_in=linux:59 waiters=3 ms=510 comm=logd\n")
        self.assertEqual(relaunch_run.verdict(slow, 2), [])
        crash = HEALTHY + "PROC:EXIT tgid=39 thread=39/fork status=- sig=11 by=kernel parent=30 cpu_ms=9 comm=webprocess\n"
        self.assertEqual(relaunch_run.verdict(crash, 2), ["webprocess (tgid 39) died of signal 11"])

    def test_the_live_verdict_sees_a_refused_send_too(self):
        hang, _ = live_run.verdict(REFUSED)
        self.assertTrue(any("SCM:REFUSE" in finding for finding in hang), hang)
        self.assertTrue(any("Error sending IPC message" in finding for finding in hang), hang)

    def test_the_session_closes_and_starts_the_browser_each_cycle(self):
        steps = relaunch_run.relaunch_session(["https://squoosh.app/"], 3, 1.0, 1.0)
        starts = [step for step in steps if step.get("until") == "SHELL:LAUNCH:PASS app=os.lazy.lazywebplus"]
        self.assertEqual(len(starts), 3)
        loads = [step["occurrence"] for step in steps
                 if step.get("wait_for") == "WEBPLUS:LOAD:about:start" and "occurrence" in step]
        self.assertEqual(loads, [2, 3, 4])


if __name__ == "__main__":
    unittest.main()
