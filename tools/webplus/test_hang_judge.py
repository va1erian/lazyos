#!/usr/bin/env python3
"""The hung-page judge fails when it should (`hang_judge.py`)."""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import hang_judge as h  # noqa: E402
import hang_session  # noqa: E402

P = h.P


def good() -> str:
    """The log of the session: Wait, the toolbar's Reload, CYCLES more bar Reloads, then Stop; every
    ended web process has its proxy kept and counted (patch 0021)."""
    lines = [
        f"{P}:HEARTBEAT:UP", f"{P}:PROCESS:LAUNCH:web:30", f"{P}:PROCESS:LAUNCH:network:32",
        f"{P}:WEBPROCESS:UP:30", f"{P}:NETWORKPROCESS:UP:32", f"{P}:INFO:QUOTA:user_memory_mib=300:limit_mib=8192",
        f"{P}:LOAD:{h.PAGE}", f"{P}:TITLE:{h.READY}",
    ]
    pid = 30
    kept = 0

    def cycle(answer: str, wait_first: bool) -> None:
        nonlocal pid, kept
        lines.extend([f"{P}:TITLE:{h.SPINNING}", f"{P}:WEBPROCESS:UNRESPONSIVE:{pid}", f"{P}:HANG:SHOW"])
        if wait_first:
            lines.extend([f"{P}:HANG:ANSWER:wait", f"{P}:HANG:HIDE"])
        lines.extend([
            f"{P}:HANG:ANSWER:{answer}", f"{P}:WEBPROCESS:RECOVER:{pid}:{answer}",
            f"{P}:PROCESS:TERMINATE:web:{pid}:group=0:pid=-1", f"{P}:PROCESS:KEPT:{pid}:{kept + 1}",
            f"{P}:WEBPROCESS:GONE:{pid}:requested", f"{P}:PROCESS:EXIT:web:{pid}:signal=9",
        ])
        kept += 1
        if answer == "reload":
            pid += 6
            lines.extend([f"{P}:PROCESS:LAUNCH:web:{pid}", f"{P}:WEBPROCESS:UP:{pid}", f"{P}:LOAD:{h.PAGE}",
                          f"{P}:TITLE:{h.READY}", f"{P}:INFO:QUOTA:user_memory_mib=310:limit_mib=8192"])
        else:
            lines.append(f"{P}:FAIL:webprocess:stopped")

    cycle("reload", True)
    for _ in range(h.CYCLES):
        cycle("reload", False)
    cycle("stop", False)
    lines.extend([f"{P}:CHILD:network:32:GONE:term=0:kill=0:wait=32", f"{P}:QUIT:PASS"])
    return "\n".join(lines) + "\n"


class HangJudge(unittest.TestCase):
    def test_a_good_run_passes(self) -> None:
        self.assertEqual(h.judge_hang(good()), [])

    def test_each_step_is_needed(self) -> None:
        for gone in (f"{P}:HEARTBEAT:UP\n", f"{P}:HANG:SHOW\n", f"{P}:HANG:ANSWER:wait\n",
                     f"{P}:HANG:ANSWER:stop\n", f"{P}:FAIL:webprocess:stopped\n",
                     f"{P}:PROCESS:KEPT:30:1\n"):
            self.assertTrue(h.judge_hang(good().replace(gone, "")), gone)

    def test_reload_must_start_another_process(self) -> None:
        text = re.sub(rf"{P}:WEBPROCESS:UP:\d+", f"{P}:WEBPROCESS:UP:30", good())
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
