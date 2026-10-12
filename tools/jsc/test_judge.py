#!/usr/bin/env python3
"""The judge must fail when it should (tools/jsc/judge.py)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge  # noqa: E402

W1 = "JSC:W1:UP\nJSC:W1:PASS:cxx\nJSC:W1:PASS:icu\nJSC:W1:PASS\n"
W2 = ("JSC:UP:PASS\nJSC:START:1234\nJSC:RUN:PASS:arith\n"
      "JSC:RUN:PASS:gc\nJSC:RUN:PASS:promise\n")
GOOD = W1 + W2 + "ABI:jsc:PASS\n"


def failing(serial: str, stages=("w1", "w2")) -> list[str]:
    return [v.name for v in judge.judge(serial, stages) if not v.ok]


class JudgeTests(unittest.TestCase):
    def test_a_complete_log_passes(self):
        self.assertTrue(judge.passed(judge.judge(GOOD)))

    def test_an_empty_log_fails_everything(self):
        self.assertEqual(len(failing("")), 8)  # every marker; "no failure lines" holds

    def test_the_w1_pass_line_is_not_its_prefix_forms(self):
        # `JSC:W1:PASS:cxx` alone must not stand in for `JSC:W1:PASS`.
        serial = GOOD.replace("JSC:W1:PASS\n", "")
        self.assertEqual(failing(serial), ["JSC:W1:PASS"])

    def test_each_missing_run_marker_is_named(self):
        for name in judge.RUNS:
            serial = GOOD.replace(f"JSC:RUN:PASS:{name}\n", "")
            self.assertEqual(failing(serial), [f"JSC:RUN:PASS:{name}"], name)

    def test_a_failure_line_fails_even_with_every_pass_marker(self):
        serial = GOOD + "JSC:RUN:FAIL:gc:got gc-bad\n"
        self.assertEqual(failing(serial), ["no failure lines"])
        self.assertIn("gc-bad", judge.judge(serial)[-1].detail)

    def test_a_killed_task_fails(self):
        serial = GOOD + "user: task 12 killed by SIGSEGV at 0x0\n"
        self.assertEqual(failing(serial), ["no failure lines"])

    def test_the_fixture_must_finish(self):
        self.assertEqual(failing(GOOD.replace("ABI:jsc:PASS\n", "")), ["ABI:jsc:PASS"])

    def test_stage_w1_alone_ignores_the_shell(self):
        serial = W1 + "JSC:UP:FAIL:spawn: No such file\nABI:jsc:FAIL:see\n"
        self.assertEqual(failing(serial, ("w1",)), [])
        self.assertTrue(failing(serial, ("w1", "w2")))

    def test_startup_time(self):
        self.assertEqual(judge.startup_ms(GOOD), 1234)
        self.assertIsNone(judge.startup_ms(W1))


if __name__ == "__main__":
    unittest.main()
