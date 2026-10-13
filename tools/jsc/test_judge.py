#!/usr/bin/env python3
"""The judge must fail when it should (tools/jsc/judge.py)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge  # noqa: E402

W1 = "JSC:W1:UP\nJSC:W1:PASS:cxx\nJSC:W1:PASS:icu\nJSC:W1:PASS\n"


def shell(prefix: str, passed: int = 255, total: int = 255, signals: int = 32) -> str:
    lines = [f"{prefix}UP:PASS", f"{prefix}START:1234"]
    lines += [f"{prefix}RUN:PASS:{name}" for name in judge.RUNS]
    lines += [f"{prefix}TIME:hot:900", f"{prefix}SIGNALS:suspend:{signals}",
              f"{prefix}STRESS:{passed}/{total}", f"{prefix}STRESS:TIME:90000"]
    return "\n".join(lines) + "\n"


W2 = shell("JSC:", 250) + shell("JSC:JIT:", 252)
GOOD = W1 + W2 + "ABI:jsc:PASS\n"
#: Every verdict a log needs: W1 (3), per shell UP + RUNS + SIGNALS + STRESS, the fixture.
EVERYTHING = 3 + 2 * (1 + len(judge.RUNS) + len(judge.SIGNAL_RUNS) + 1) + 1


def failing(serial: str, stages=("w1", "w2")) -> list[str]:
    return [v.name for v in judge.judge(serial, stages) if not v.ok]


class JudgeTests(unittest.TestCase):
    def test_a_complete_log_passes(self):
        self.assertTrue(judge.passed(judge.judge(GOOD)))

    def test_an_empty_log_fails_everything(self):
        # Every marker; "no failure lines" holds on an empty log.
        self.assertEqual(len(failing("")), EVERYTHING)

    def test_the_w1_pass_line_is_not_its_prefix_forms(self):
        # `JSC:W1:PASS:cxx` alone must not stand in for `JSC:W1:PASS`.
        serial = GOOD.replace("JSC:W1:PASS\n", "")
        self.assertEqual(failing(serial), ["JSC:W1:PASS"])

    def test_each_missing_run_marker_is_named_for_each_shell(self):
        for prefix in judge.SHELLS:
            for name in judge.RUNS:
                marker = f"{prefix}RUN:PASS:{name}"
                serial = GOOD.replace(f"{marker}\n", "")
                self.assertEqual(failing(serial), [marker], marker)

    def test_a_shell_prefix_is_not_the_other_shells(self):
        # The JIT shell's markers must not satisfy the interpreter's checks.
        serial = W1 + shell("JSC:JIT:") + "ABI:jsc:PASS\n"
        self.assertEqual(sorted(failing(serial)),
                         sorted(["JSC:UP:PASS", "JSC:STRESS", "JSC:SIGNALS:suspend",
                                 *[f"JSC:RUN:PASS:{n}" for n in judge.RUNS]]))

    def test_a_failure_line_fails_even_with_every_pass_marker(self):
        for line in ("JSC:RUN:FAIL:gc:got gc-bad", "JSC:JIT:RUN:FAIL:agents:got agents-bad",
                     "JSC:JIT:UP:FAIL:spawn"):
            self.assertEqual(failing(GOOD + line + "\n"), ["no failure lines"], line)
        self.assertIn("gc-bad", judge.judge(GOOD + "JSC:RUN:FAIL:gc:got gc-bad\n")[-1].detail)

    def test_a_killed_task_is_the_fixtures_to_report(self):
        # A stress file the kernel kills is counted by the stress rate; a crash
        # anywhere else makes the fixture print a failure line or not finish.
        killed = "user: task 12 killed by SIGSEGV at 0x0\n"
        self.assertEqual(failing(GOOD + killed), [])
        self.assertEqual(failing(GOOD.replace("JSC:RUN:PASS:gc\n", "") + killed),
                         ["JSC:RUN:PASS:gc"])

    def test_the_fixture_must_finish(self):
        self.assertEqual(failing(GOOD.replace("ABI:jsc:PASS\n", "")), ["ABI:jsc:PASS"])

    def test_stage_w1_alone_ignores_the_shell(self):
        serial = W1 + "JSC:UP:FAIL:spawn: No such file\nABI:jsc:FAIL:see\n"
        self.assertEqual(failing(serial, ("w1",)), [])
        self.assertTrue(failing(serial, ("w1", "w2")))

    def test_script_times_per_shell(self):
        serial = GOOD + "JSC:TIME:gc:2500\nJSC:JIT:TIME:gc:2400\nJSC:TIME:promise:90\n"
        self.assertEqual(judge.times(serial), {"hot": 900, "gc": 2500, "promise": 90})
        self.assertEqual(judge.times(serial, "JSC:JIT:"), {"hot": 900, "gc": 2400})
        self.assertEqual(judge.times(W1), {})

    def test_startup_time(self):
        self.assertEqual(judge.startup_ms(GOOD), 1234)
        self.assertEqual(judge.startup_ms(GOOD, "JSC:JIT:"), 1234)
        self.assertIsNone(judge.startup_ms(W1))

    def test_stress_summary_is_required(self):
        serial = GOOD.replace("JSC:JIT:STRESS:252/255\n", "")
        self.assertEqual(failing(serial), ["JSC:JIT:STRESS"])

    def test_too_few_stress_files_fail(self):
        serial = W1 + shell("JSC:", 10, 10) + shell("JSC:JIT:") + "ABI:jsc:PASS\n"
        self.assertEqual(failing(serial), ["JSC:STRESS"])
        detail = {v.name: v.detail for v in judge.judge(serial)}["JSC:STRESS"]
        self.assertIn("only 10", detail)

    def test_a_low_stress_rate_fails(self):
        floor = judge.STRESS_MIN_RATE["JSC:JIT:"]
        if floor == 0:
            self.skipTest("no stress floor set yet")
        low = int(255 * floor) - 5
        serial = W1 + shell("JSC:") + shell("JSC:JIT:", low) + "ABI:jsc:PASS\n"
        self.assertEqual(failing(serial), ["JSC:JIT:STRESS"])

    def test_stress_failures_are_listed_with_their_reason(self):
        serial = GOOD + ("JSC:STRESS:FAIL:nope.js:timeout\n"
                         "JSC:JIT:STRESS:FAIL:x.js:exit status: 3 Exception; bad\n")
        self.assertEqual(judge.stress_failures(serial, "JSC:"), {"nope.js": "timeout"})
        self.assertEqual(judge.stress_failures(serial, "JSC:JIT:"),
                         {"x.js": "exit status: 3 Exception; bad"})
        # They are not "no failure lines": the stress verdict names them.
        self.assertNotIn("no failure lines", failing(serial))

    def test_an_unexpected_stress_failure_fails_even_above_the_rate(self):
        # 254/255 clears the 98% floor, but an unknown name is a regression.
        for prefix in judge.SHELLS:
            serial = (W1 + shell("JSC:", 254) + shell("JSC:JIT:", 254) + "ABI:jsc:PASS\n"
                      + f"{prefix}STRESS:FAIL:new-regression.js:timeout\n")
            self.assertEqual(failing(serial), [f"{prefix}STRESS"], prefix)
            detail = {v.name: v.detail for v in judge.judge(serial)}[f"{prefix}STRESS"]
            self.assertIn("new-regression.js", detail)

    def test_the_known_stress_failure_alone_passes(self):
        (known,) = judge.KNOWN_STRESS_FAILURES
        serial = (W1 + shell("JSC:", 254) + shell("JSC:JIT:", 254) + "ABI:jsc:PASS\n"
                  + f"JSC:STRESS:FAIL:{known}:signal 11\n"
                  + f"JSC:JIT:STRESS:FAIL:{known}:signal 11\n")
        self.assertEqual(failing(serial), [])
        # One known name plus one unknown is still a failure.
        self.assertEqual(failing(serial + "JSC:STRESS:FAIL:other.js:timeout\n"), ["JSC:STRESS"])

    def test_thread_signals_must_be_counted_for_suspend(self):
        for prefix in judge.SHELLS:
            marker = f"{prefix}SIGNALS:suspend"
            zero = GOOD.replace(f"{marker}:32\n", f"{marker}:0\n")
            self.assertEqual(failing(zero), [marker], prefix)
            missing = "".join(line + "\n" for line in GOOD.splitlines()
                              if not line.startswith(marker))
            self.assertEqual(failing(missing), [marker], prefix)
        self.assertEqual(judge.signals(GOOD), {"suspend": 32})
        self.assertEqual(judge.signals(GOOD + "JSC:JIT:SIGNALS:gc:5\n", "JSC:JIT:"),
                         {"suspend": 32, "gc": 5})

    def test_stress_time(self):
        self.assertEqual(judge.stress_ms(GOOD), 90000)
        self.assertIsNone(judge.stress_ms(W1))


if __name__ == "__main__":
    unittest.main()
