#!/usr/bin/env python3
"""Tests for the stall report of the kernel test runner (issue #787).

    python tools/test/test_serialwatch.py
"""

from __future__ import annotations

import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import serialwatch  # noqa: E402

RIP = "RIP=ffff800000123456 RFL=00000002"


class FakeQmp:
    def __init__(self, fail: bool = False):
        self.calls = 0
        self.fail = fail

    def execute(self, command: str, **arguments):
        self.calls += 1
        if self.fail:
            raise RuntimeError("gone")
        if command == "inject-nmi":
            return {}
        assert command == "human-monitor-command"
        assert arguments == {"command-line": "info registers -a"}
        return f"RAX=0000 {RIP}\nRIP junk"


class FakeProc:
    def poll(self):
        return None


class RunningTest(unittest.TestCase):
    def test_none_before_any_test(self):
        self.assertIsNone(serialwatch.running_test("LazyOS: kernel test mode\n"))

    def test_finished_tests_are_not_running(self):
        log = "TEST:a:START\nTEST:a:PASS\nTEST:b:START\nTEST:b:FAIL:oops\n"
        self.assertIsNone(serialwatch.running_test(log))

    def test_last_started_without_result(self):
        log = "TEST:a:START\nTEST:a:PASS\nTEST:virtio_roundtrip_unaligned:START\nTEST:x:INFO:y\n"
        self.assertEqual(serialwatch.running_test(log), "virtio_roundtrip_unaligned")


class StallReport(unittest.TestCase):
    def test_names_test_tail_and_rips(self):
        qmp = FakeQmp()
        report = serialwatch.stall_report("TEST:a:START\nsome line\n", 200.0, qmp)
        self.assertIn("test in flight: a", report)
        self.assertIn("some line", report)
        self.assertIn("ffff800000123456", report)
        self.assertEqual(qmp.calls, 4)
        self.assertIn("NMI sent", report)

    def test_without_qmp_and_with_a_dead_qmp(self):
        self.assertIn("no QMP", serialwatch.stall_report("", 1.0, None))
        self.assertIn("unavailable", serialwatch.stall_report("", 1.0, FakeQmp(fail=True)))


class EmitStall(unittest.TestCase):
    def test_a_failing_report_does_not_raise(self):
        # Unwritable path and bytes a console cannot encode: both only reported.
        bad = Path(tempfile.gettempdir(), "no-such-dir-787", "stall.txt")
        serialwatch.emit_stall("TEST:a:START\n�\x81\n", 1.0, FakeQmp(), bad)
        serialwatch.emit_stall("x", 1.0, FakeQmp(fail=True), None)


class FollowSerial(unittest.TestCase):
    def test_silent_log_is_reported_once_and_the_wait_continues(self):
        with tempfile.TemporaryDirectory() as tmp:
            log, stall = Path(tmp, "serial.log"), Path(tmp, "stall.txt")
            log.write_text("TEST:slow_one:START\n")
            text = serialwatch.follow_serial(
                log, FakeProc(), time.time() + 1.6, qmp=FakeQmp(),
                stall_after=0.4, stall_file=stall,
            )
            self.assertIn("slow_one", text)
            report = stall.read_text()
            self.assertEqual(report.count("STALL: no serial output"), 1)
            self.assertIn("test in flight: slow_one", report)

    def test_no_report_when_off_or_when_the_summary_arrives(self):
        with tempfile.TemporaryDirectory() as tmp:
            log, stall = Path(tmp, "serial.log"), Path(tmp, "stall.txt")
            log.write_text("TEST:a:START\nTEST:a:PASS\nTEST:SUMMARY:PASS=1 FAIL=0\n")
            serialwatch.follow_serial(
                log, FakeProc(), time.time() + 5, qmp=FakeQmp(),
                stall_after=0.1, stall_file=stall,
            )
            self.assertFalse(stall.exists())


if __name__ == "__main__":
    unittest.main()
