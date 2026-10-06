#!/usr/bin/env python3
"""The crash-notice judge passes a good run and fails each bad one."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import judge  # noqa: E402

REASON = "lazyrad-player: main_form.rhai:8:5: crash test: form_load always fails"


def launch(pid: int) -> list[str]:
    return [
        f"INIT:LAUNCH:PASS app=org.lazy.crashload pid={pid} session=0",
        f"INIT:APP:REASON app=org.lazy.crashload pid={pid} reason={REASON}",
        "INIT:LAUNCH:EXIT app=org.lazy.crashload status=2",
        f"INIT:APP:FAILED app=org.lazy.crashload status=2 startup=1 reason={REASON}",
        "INIT:APP:NOTICE:PASS app=org.lazy.crashload matched=1",
        "SHELL:FAILURE app=org.lazy.crashload status=2",
        "SHELL:NOTICE:OPEN app=org.lazy.crashload title=crashload stopped",
    ]


def good() -> list[str]:
    return (launch(40)
            + ["SHELL:NOTICE:CLOSE app=org.lazy.crashload",
               "SHELL:NOTICE:RESTART app=org.lazy.crashload"]
            + launch(41)
            + ["SHELL:NOTICE:CLOSE app=org.lazy.crashload"])


class JudgeTest(unittest.TestCase):
    def test_a_good_run_passes(self):
        self.assertEqual(judge.judge("\n".join(good())), [])

    def test_a_restart_loop_fails(self):
        lines = good() + [
            "INIT:RESTART:PASS name=org.lazy.crashload status=2 attempt=2 delay=10",
            "INIT:LAUNCH:EXIT app=org.lazy.crashload status=2",
        ]
        problems = judge.judge("\n".join(lines))
        self.assertTrue(any("restarted" in p for p in problems), problems)
        self.assertTrue(any("restart loop" in p for p in problems), problems)

    def test_a_missing_reason_fails(self):
        lines = [line for line in good() if "INIT:APP:REASON" not in line]
        self.assertTrue(any("reason" in p for p in judge.judge("\n".join(lines))))

    def test_no_notice_fails(self):
        lines = [line for line in good() if "SHELL:NOTICE:OPEN" not in line]
        self.assertTrue(any("showed" in p for p in judge.judge("\n".join(lines))))

    def test_a_notice_left_open_fails(self):
        lines = good()[:-1]
        self.assertTrue(any("closed" in p for p in judge.judge("\n".join(lines))))

    def test_restart_must_relaunch(self):
        lines = [line for line in good() if "NOTICE:RESTART" not in line]
        self.assertTrue(any("Restart" in p for p in judge.judge("\n".join(lines))))


if __name__ == "__main__":
    unittest.main()
