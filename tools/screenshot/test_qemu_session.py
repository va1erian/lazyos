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
import session_pointer  # noqa: E402


def write_log(directory: Path, text: str) -> Path:
    path = directory / "serial.log"
    path.write_text(text, encoding="utf-8")
    return path


class ClickAtTests(unittest.TestCase):
    def test_pixels_map_onto_the_tablet_axes(self):
        r = qemu_session.resolve_click_at
        self.assertEqual(r([0, 0], (1280, 720), {}), (0, 0))
        self.assertEqual(r([1279, 719], (1280, 720), {}), (32767, 32767))

    def test_names_resolve_and_unknown_or_offscreen_fail(self):
        r = qemu_session.resolve_click_at
        self.assertEqual(r("a", (101, 101), {"a": [50, 100]}), (16383, 32767))
        with self.assertRaises(qemu_session.StepFailed):
            r("b", (1280, 720), {})
        with self.assertRaises(qemu_session.StepFailed):
            r([1280, 0], (1280, 720), {})

    def test_malformed_targets_fail_the_step(self):
        # A StepFailed (not a ValueError/TypeError) is what keeps the failure
        # screenshot and summary.json.
        r = qemu_session.resolve_click_at
        for target, targets in [([10], {}), ("a", {"a": None}), ("a", {"a": [1, "2"]}),
                                ([True, 3], {}), (7, {}), ({"window": "W", "menu": "M"}, {}),
                                ({"widget": "w"}, {}), ({"window": "W", "offset": [1]}, {})]:
            with self.subTest(target=target), self.assertRaises(qemu_session.StepFailed):
                r(target, (1280, 720), targets, "UI:RECT x=0 y=0 w=10 h=10 name=window:W\n")

    def test_a_malformed_target_ends_the_session_as_a_failed_step(self):
        class FakeQmp:
            pass
        with tempfile.TemporaryDirectory() as work:
            path = Path(work)
            write_log(path, "")
            serial = qemu_session.SerialLog(path / "serial.log", [])
            with self.assertRaises(qemu_session.StepFailed):
                qemu_session.run_steps(FakeQmp(), [{"click_at": [10]}], path, time.time(), serial)

    def test_the_screen_must_be_at_least_two_pixels_each_way(self):
        parse = session_pointer.parse_screen
        self.assertEqual(parse("1280x720"), (1280, 720))
        for text in ("1x720", "1280x1", "0x0", "1280", "axb"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                parse(text)


PROBE = (
    "UI:RECT x=100 y=50 w=724 h=458 name=window:MOD Player\n"
    "noise UI:WIDGET x=68 y=142 w=52 h=28 name=play_button window=MOD Player\r\n"
    "UI:RECT x=28 y=500 w=212 h=24 name=menu:Accessories\n"
    "UI:RECT x=4 y=690 w=80 h=28 name=taskbar:start\n"
    "UI:RECT x=28 y=476 w=212 h=24 name=menu:Accessories\n"
)


class ProbeTargetTests(unittest.TestCase):
    def pixel(self, target, probe=PROBE, targets=None):
        return session_pointer.resolve_pixel(target, targets or {}, probe)

    def test_a_widget_is_offset_by_its_window(self):
        self.assertEqual(self.pixel({"window": "MOD Player", "widget": "play_button"}),
                         (100 + 68 + 26, 50 + 142 + 14))
        self.assertEqual(self.pixel({"window": "MOD Player"}), (100 + 362, 50 + 229))

    def test_menus_targets_names_and_offsets(self):
        self.assertEqual(self.pixel({"menu": "Accessories"}), (134, 488), "the newest line wins")
        self.assertEqual(self.pixel({"target": "taskbar:start"}), (44, 704))
        self.assertEqual(self.pixel("taskbar:start"), (44, 704))
        self.assertEqual(self.pixel({"target": "taskbar:start", "offset": [-10, 2]}), (34, 706))
        self.assertEqual(self.pixel("taskbar:start", targets={"taskbar:start": [1, 2]}), (1, 2))

    def test_an_unseen_name_is_none_until_printed(self):
        self.assertIsNone(self.pixel({"window": "MOD Player", "widget": "stop"}))
        self.assertIsNone(self.pixel({"menu": "Games"}))
        self.assertIsNone(self.pixel("nothing"))

    def test_waiting_times_out_as_a_failed_step(self):
        with self.assertRaises(qemu_session.StepFailed) as caught:
            session_pointer.wait_pixel({"menu": "Games"}, lambda: PROBE, 0.3)
        self.assertIn("LAZYOS_UI_PROBE", str(caught.exception))

    def test_click_at_moves_then_clicks_with_or_without_a_tablet(self):
        class FakeQmp:
            def __init__(self):
                self.calls = []

            def __getattr__(self, name):
                return lambda *args: self.calls.append((name, *args))

        step = {"click_at": {"menu": "Accessories"}}
        for tablet, expected in [
            (True, [("mouse_abs", 134 * 32767 // 1279, 488 * 32767 // 719),
                    ("mouse_click", "left")]),
            (False, [("mouse_move", -300, -300)] * 6 + [("mouse_move", 134, 488),
                                                        ("mouse_click", "left")]),
        ]:
            qmp = FakeQmp()
            session_pointer.POINTER.update(tablet=tablet, screen=(1280, 720), targets={})
            try:
                session_pointer.point(qmp, step, "click_at", lambda: PROBE, 1)
            finally:
                session_pointer.POINTER["tablet"] = False
            self.assertEqual(qmp.calls, expected)
        qmp = FakeQmp()
        session_pointer.POINTER["tablet"] = True
        try:
            session_pointer.point(qmp, {"move_to": [0, 0]}, "move_to", lambda: "", 1)
        finally:
            session_pointer.POINTER["tablet"] = False
        self.assertEqual(qmp.calls, [("mouse_abs", 0, 0)])


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
            def type_text(self, text, delay=0.01):
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
