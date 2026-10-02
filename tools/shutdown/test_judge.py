#!/usr/bin/env python3
"""The shutdown judge fails when it should (and passes a good log).

    python tools/shutdown/test_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from judge import judge  # noqa: E402

GOOD = """\
init: started confd (pid 4, attempt 1)
INIT:RESTART:PASS name=flaky status=1 attempt=1 delay=10
INIT:SHUTDOWN:BEGIN mode=poweroff uid=0 session=0 reason="x"
power: watchdog armed (power-off, deadline tick 4000)
INIT:SHUTDOWN:PHASE stopping
INIT:SHUTDOWN:PHASE apps
INIT:SHUTDOWN:APPS asked=3
INIT:SHUTDOWN:PHASE services
INIT:SHUTDOWN:SERVICES
init: stopping keyd (SIGTERM)
LOGD:STOP records=40 verified=true persisted=40 reason="x"
CONFD:STOP dir=/data/confd sync=ok reason="x"
init: userspace quiesced (killed=0)
INIT:SHUTDOWN:QUIESCED killed=0 ticks=120
INIT:SHUTDOWN:PHASE power
INIT:SHUTDOWN:POWER mode=poweroff
power: shutdown requested
power: filesystems synced
"""


class JudgeTest(unittest.TestCase):
    def test_a_good_log_passes(self):
        self.assertEqual(judge(GOOD, "poweroff"), [])

    def test_a_restart_before_the_request_is_fine(self):
        self.assertIn("INIT:RESTART:PASS", GOOD.split("INIT:SHUTDOWN:BEGIN")[0])
        self.assertEqual(judge(GOOD, "poweroff"), [])

    def test_no_shutdown_at_all(self):
        self.assertEqual(len(judge("init: started confd\n", "poweroff")), 1)

    def test_the_wrong_mode(self):
        self.assertTrue(judge(GOOD, "reboot"))

    def test_phases_out_of_order(self):
        swapped = GOOD.replace("INIT:SHUTDOWN:PHASE apps\n", "").replace(
            "INIT:SHUTDOWN:SERVICES\n", "INIT:SHUTDOWN:SERVICES\nINIT:SHUTDOWN:PHASE apps\n")
        self.assertTrue(any("in order" in f for f in judge(swapped, "poweroff")))

    def test_confd_must_stop_inside_the_services_phase(self):
        late = GOOD.replace('CONFD:STOP dir=/data/confd sync=ok reason="x"\n', "") + \
            'CONFD:STOP dir=/data/confd sync=ok reason="x"\n'
        self.assertTrue(any("outside" in f for f in judge(late, "poweroff")))

    def test_a_failed_sync_or_chain(self):
        self.assertTrue(judge(GOOD.replace("sync=ok", "sync=errno -5"), "poweroff"))
        self.assertTrue(judge(GOOD.replace("verified=true", "verified=false"), "poweroff"))

    def test_logd_must_persist_on_a_desktop_boot(self):
        none = GOOD.replace("persisted=40", "persisted=0")
        self.assertTrue(any("persisted no records" in f for f in judge(none, "poweroff")))
        old = GOOD.replace(" persisted=40", "")
        self.assertTrue(any("persisted=<n>" in f for f in judge(old, "poweroff")))
        # A console boot (no OS volume) is not held to it.
        self.assertEqual(judge(none, "poweroff", desktop=False), [])
        self.assertEqual(judge(old, "poweroff", desktop=False), [])

    def test_kills_and_restarts_during_the_stop(self):
        killed = GOOD.replace("QUIESCED killed=0", "QUIESCED killed=2")
        self.assertTrue(judge(killed, "poweroff"))
        restarted = GOOD.replace("INIT:SHUTDOWN:SERVICES\n",
                                 "INIT:SHUTDOWN:SERVICES\nINIT:RESTART:PASS name=keyd\n")
        self.assertTrue(judge(restarted, "poweroff"))

    def test_kernel_fallbacks(self):
        for line in ["power: watchdog expired; forcing power-off",
                     "power: no ACPI power-off; it is now safe to turn the machine off",
                     "power: sync failed: I/O error"]:
            self.assertTrue(judge(GOOD + line + "\n", "poweroff"), line)

    def test_a_missing_watchdog(self):
        self.assertTrue(judge(GOOD.replace("power: watchdog armed", "power: armed"),
                              "poweroff"))


if __name__ == "__main__":
    unittest.main()
