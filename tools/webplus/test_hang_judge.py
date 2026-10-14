#!/usr/bin/env python3
"""The hung-page judge fails when it should (`hang_judge.py`)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import hang_judge as h  # noqa: E402
import hang_session  # noqa: E402

P = h.P


def good() -> str:
    lines = [
        f"{P}:HEARTBEAT:UP", f"{P}:WEBPROCESS:UP:30", f"{P}:NETWORKPROCESS:UP:32",
        f"{P}:LOAD:{h.PAGE}", f"{P}:TITLE:{h.READY}", f"{P}:TITLE:{h.SPINNING}",
        f"{P}:WEBPROCESS:UNRESPONSIVE:30", f"{P}:HANG:SHOW",
        f"{P}:HANG:ANSWER:wait", f"{P}:HANG:HIDE",
        f"{P}:HANG:ANSWER:reload", f"{P}:WEBPROCESS:RECOVER:30:reload",
        f"{P}:PROCESS:TERMINATE:web:30:group=0:pid=-1",
        f"{P}:WEBPROCESS:GONE:30:requested", f"{P}:PROCESS:EXIT:web:30:signal=9",
        f"{P}:PROCESS:LAUNCH:web:36", f"{P}:WEBPROCESS:UP:36",
        f"{P}:LOAD:{h.PAGE}", f"{P}:TITLE:{h.READY}", f"{P}:TITLE:{h.SPINNING}",
        f"{P}:WEBPROCESS:UNRESPONSIVE:36", f"{P}:HANG:SHOW",
        f"{P}:HANG:ANSWER:stop", f"{P}:WEBPROCESS:RECOVER:36:stop",
        f"{P}:WEBPROCESS:GONE:36:requested", f"{P}:PROCESS:EXIT:web:36:signal=9", f"{P}:FAIL:webprocess:stopped",
        f"{P}:CHILD:network:32:GONE:term=0:kill=0:wait=32", f"{P}:QUIT:PASS",
    ]
    return "\n".join(lines) + "\n"


class HangJudge(unittest.TestCase):
    def test_a_good_run_passes(self) -> None:
        self.assertEqual(h.judge_hang(good()), [])

    def test_each_step_is_needed(self) -> None:
        for gone in (f"{P}:HEARTBEAT:UP\n", f"{P}:HANG:SHOW\n", f"{P}:HANG:ANSWER:wait\n",
                     f"{P}:WEBPROCESS:RECOVER:30:reload\n", f"{P}:FAIL:webprocess:stopped\n"):
            self.assertTrue(h.judge_hang(good().replace(gone, "", 1)), gone)

    def test_reload_must_start_another_process(self) -> None:
        text = good().replace(f"{P}:WEBPROCESS:UP:36", f"{P}:WEBPROCESS:UP:30")
        self.assertTrue(any("new process" in p for p in h.judge_hang(text)))

    def test_a_process_ended_by_a_crash_is_not_the_windows_doing(self) -> None:
        text = good().replace(f"{P}:WEBPROCESS:GONE:36:requested", f"{P}:WEBPROCESS:GONE:36:crash")
        self.assertTrue(any("crash" in p for p in h.judge_hang(text)))

    def test_wait_must_not_end_the_process(self) -> None:
        text = good().replace(f"{P}:HANG:HIDE", f"{P}:HANG:HIDE\n{P}:WEBPROCESS:RECOVER:30:stop", 1)
        self.assertTrue(any("Wait ended" in p for p in h.judge_hang(text)))

    def test_a_process_left_behind_fails(self) -> None:
        text = good().replace(f"{P}:CHILD:network:32:GONE:term=0:kill=0:wait=32", f"{P}:CHILD:network:32:ALIVE:term=0:kill=0:wait=-10")
        self.assertTrue(h.judge_hang(text))

    def test_the_example_session_is_current(self) -> None:
        self.assertEqual(hang_session.EXAMPLE.read_text(encoding="utf-8"), hang_session.render(hang_session.hang()))


if __name__ == "__main__":
    unittest.main()
