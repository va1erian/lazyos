"""Unit tests for qemu_session.py's serial gating (no QEMU needed):
python tools/screenshot/test_qemu_session.py"""

from __future__ import annotations

import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qemu_session  # noqa: E402


def write_log(directory: Path, text: str) -> Path:
    path = directory / "serial.log"
    path.write_text(text, encoding="utf-8")
    return path


class SerialOccurrenceTests(unittest.TestCase):
    def test_first_occurrence_is_the_default(self):
        with tempfile.TemporaryDirectory() as work:
            serial = qemu_session.SerialLog(write_log(Path(work), "UP:PASS\n"), [])
            self.assertGreaterEqual(serial.wait_for("UP:PASS", timeout=1), 0)

    def test_a_second_match_is_not_satisfied_by_the_first(self):
        with tempfile.TemporaryDirectory() as work:
            serial = qemu_session.SerialLog(write_log(Path(work), "UP:PASS\n"), [])
            with self.assertRaises(qemu_session.StepFailed):
                serial.wait_for("UP:PASS", timeout=0.3, occurrence=2)

    def test_waits_until_the_nth_match_arrives(self):
        with tempfile.TemporaryDirectory() as work:
            path = write_log(Path(work), "UP:PASS\n")
            serial = qemu_session.SerialLog(path, [])

            def append():
                time.sleep(0.4)
                with open(path, "a", encoding="utf-8") as handle:
                    handle.write("UP:PASS\n")

            thread = threading.Thread(target=append)
            thread.start()
            self.addCleanup(thread.join)
            serial.wait_for("UP:PASS", timeout=3, occurrence=2)

    def test_all_matches_are_counted(self):
        with tempfile.TemporaryDirectory() as work:
            serial = qemu_session.SerialLog(
                write_log(Path(work), "UP:PASS\nUP:PASS\nUP:PASS\n"), []
            )
            serial.wait_for("UP:PASS", timeout=1, occurrence=3)
            with self.assertRaises(qemu_session.StepFailed):
                serial.wait_for("UP:PASS", timeout=0.3, occurrence=4)

    def test_regex_markers_honour_occurrence(self):
        with tempfile.TemporaryDirectory() as work:
            serial = qemu_session.SerialLog(write_log(Path(work), "id=1\nid=2\n"), [])
            serial.wait_for(r"id=\d", timeout=1, regex=True, occurrence=2)

    def test_since_scopes_the_count_so_until_is_unaffected(self):
        with tempfile.TemporaryDirectory() as work:
            text = "UP:PASS\n"
            serial = qemu_session.SerialLog(write_log(Path(work), text), [])
            # The only match is before `since`, so occurrence 1 must still time
            # out: `until` (which passes `since`) keeps its old meaning.
            with self.assertRaises(qemu_session.StepFailed):
                serial.wait_for("UP:PASS", timeout=0.3, since=len(text))

    def test_fail_on_is_checked_while_waiting_for_the_nth(self):
        with tempfile.TemporaryDirectory() as work:
            serial = qemu_session.SerialLog(
                write_log(Path(work), "UP:PASS\nFAIL:boom\n"), [r"FAIL:boom"]
            )
            with self.assertRaises(qemu_session.StepFailed) as caught:
                serial.wait_for("UP:PASS", timeout=2, occurrence=2)
            self.assertIn("FAIL:boom", str(caught.exception))

    def test_an_empty_log_just_times_out(self):
        with tempfile.TemporaryDirectory() as work:
            serial = qemu_session.SerialLog(write_log(Path(work), ""), [])
            with self.assertRaises(qemu_session.StepFailed):
                serial.wait_for("UP:PASS", timeout=0.3)


class OccurrenceValidationTests(unittest.TestCase):
    def run_step(self, occurrence):
        with tempfile.TemporaryDirectory() as work:
            path = Path(work)
            serial = qemu_session.SerialLog(path / "serial.log", [])
            steps = [{"wait_for": "UP:PASS", "occurrence": occurrence}]
            return qemu_session.run_steps(None, steps, path, time.time(), serial)

    def test_a_string_occurrence_is_rejected_naming_the_step(self):
        with self.assertRaises(SystemExit) as caught:
            self.run_step("2")
        self.assertIn("step 0", str(caught.exception))
        self.assertIn("occurrence", str(caught.exception))

    def test_a_float_occurrence_is_rejected(self):
        with self.assertRaises(SystemExit):
            self.run_step(2.0)

    def test_zero_and_negative_occurrences_are_rejected(self):
        for value in (0, -1):
            with self.assertRaises(SystemExit, msg=value):
                self.run_step(value)

    def test_a_boolean_occurrence_is_rejected(self):
        with self.assertRaises(SystemExit):
            self.run_step(True)

    def test_a_valid_occurrence_is_accepted(self):
        with tempfile.TemporaryDirectory() as work:
            path = Path(work)
            (path / "serial.log").write_text("UP:PASS\nUP:PASS\n", encoding="utf-8")
            serial = qemu_session.SerialLog(path / "serial.log", [])
            steps = [{"wait_for": "UP:PASS", "occurrence": 2}]
            # No qmp call happens for a wait_for gate, so a None client is fine.
            qemu_session.run_steps(None, steps, path, time.time(), serial)


class OriginResetTests(unittest.TestCase):
    def test_wait_for_resets_the_at_origin_with_an_occurrence(self):
        with tempfile.TemporaryDirectory() as work:
            path = Path(work)
            (path / "serial.log").write_text("UP:PASS\nUP:PASS\n", encoding="utf-8")
            serial = qemu_session.SerialLog(path / "serial.log", [])
            # `started` is 5 s in the past; if the gate did not reset the origin
            # the `at` would already be in the past and no time would pass.
            started = time.time() - 5
            steps = [
                {"wait_for": "UP:PASS", "occurrence": 2},
                {"at": 0.3, "wait": 0},
            ]
            begun = time.time()
            qemu_session.run_steps(None, steps, path, started, serial)
            self.assertGreaterEqual(time.time() - begun, 0.25)


class CaptureTests(unittest.TestCase):
    """`capture` on a wait_for, `${name}` in a later `type` (the shell_demo kill)."""

    def run_script(self, log: str, steps: list[dict]) -> list[str]:
        typed: list[str] = []

        class FakeQmp:
            def type_text(self, text):
                typed.append(text)

        with tempfile.TemporaryDirectory() as work:
            path = Path(work)
            write_log(path, log)
            serial = qemu_session.SerialLog(path / "serial.log", [])
            qemu_session.run_steps(FakeQmp(), steps, path, time.time(), serial)
        return typed

    def test_the_first_group_is_typed(self):
        typed = self.run_script(
            "INIT:LAUNCH:PASS app=terminal pid=17 session=0\n"
            "INIT:LAUNCH:PASS app=lazyshell pid=16 session=0\n",
            [{"wait_for": r"INIT:LAUNCH:PASS app=lazyshell pid=(\d+)", "regex": True,
              "capture": "shell_pid", "timeout": 1},
             {"type": "kill -9 ${shell_pid}"}])
        self.assertEqual(typed, ["kill -9 16"])

    def test_the_occurrence_picks_the_match(self):
        typed = self.run_script(
            "PID=3\nPID=9\n",
            [{"wait_for": r"PID=(\d+)", "regex": True, "occurrence": 2, "capture": "p",
              "timeout": 1},
             {"type": "${p}${p}"}])
        self.assertEqual(typed, ["99"])

    def test_without_a_group_the_whole_match_is_kept(self):
        typed = self.run_script("ready 42\n", [
            {"wait_for": "ready 42", "capture": "line", "timeout": 1},
            {"type": "[${line}]"}])
        self.assertEqual(typed, ["[ready 42]"])

    def test_a_later_gate_uses_the_value(self):
        typed = self.run_script(
            "SHELL:TASKBAR:ADD id=7 title=sysmon\nSHELL:TASKBAR:REMOVE id=7\n",
            [{"wait_for": r"SHELL:TASKBAR:ADD id=(\d+) title=sysmon", "regex": True,
              "capture": "sid", "timeout": 1},
             {"wait_for": "SHELL:TASKBAR:REMOVE id=${sid}", "timeout": 1},
             {"type": "ok"}])
        self.assertEqual(typed, ["ok"])

    def test_an_unknown_variable_fails_the_step(self):
        with self.assertRaises(SystemExit) as caught:
            self.run_script("", [{"type": "kill ${nope}"}])
        self.assertIn("nope", str(caught.exception))

    def test_capture_only_on_wait_for(self):
        with self.assertRaises(SystemExit):
            self.run_script("", [{"type": "x", "capture": "v"}])
        with self.assertRaises(SystemExit):
            self.run_script("A\n", [{"wait_for": "A", "capture": "1bad", "timeout": 1}])


if __name__ == "__main__":
    unittest.main()
