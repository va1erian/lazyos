#!/usr/bin/env python3
"""The W4 judge (``portjudge.py``) passes a good ``webkit-smoke`` log and fails each way it can go wrong.

Run: python tools/webkit/test_port_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import portjudge  # noqa: E402

URL = "data:text/html,<title>hello lazyos</title><p>x</p>"
FDS = "WEBKIT:W4:FDS:held=1100:next=1103"
MONITOR = "WEBKIT:W4:MONITOR:PASS:fd=1103:messages=3:wakeups=4:close_ms=3:invalidate_ms=1"
CONTROL = "WEBKIT:W4:SPAWN:CONTROL:PASS"
SPAWN = "WEBKIT:W4:SPAWN:PASS:launches=2"
GOOD = f"""boot noise
{FDS}
{MONITOR}
{CONTROL}
WEBKIT:W4:UP
WEBKIT:W4:LOAD:about:blank
WEBKIT:W4:LOAD:{URL}
WEBKIT:W4:TITLE:hello lazyos
WEBKIT:W4:WEBPROCESS:UP:12
WEBKIT:W4:NETWORKPROCESS:UP:13
WEBKIT:W4:PAINT:800x600:0badf00d
WEBKIT:W4:INFO:PAINT:pixels=480000:displayCallbacks=3
WEBKIT:W4:INFO:INK:pixels=42:bbox=8,15,15,22
{SPAWN}
WEBKIT:W4:PASS
"""


#: What the build host's own run prints: the same page and pictures, other process ids.
HOST = GOOD.replace("WEBPROCESS:UP:12", "WEBPROCESS:UP:99").replace("NETWORKPROCESS:UP:13", "NETWORKPROCESS:UP:98")


def failed(serial: str, host: str | None = HOST) -> set[str]:
    return {v.name for v in portjudge.judge(serial, host) if not v.ok}


class PortJudgeTests(unittest.TestCase):
    def test_a_good_log_passes(self) -> None:
        self.assertEqual(failed(GOOD), set())
        self.assertTrue(portjudge.passed(portjudge.judge(GOOD, HOST)))

    def test_a_page_that_never_painted_fails(self) -> None:
        # What the program printed before it learned to fail: INFO and then PASS.
        log = GOOD.replace("WEBKIT:W4:PAINT:800x600:0badf00d\n", "").replace(
            "pixels=480000:displayCallbacks=3", "pixels=0:displayCallbacks=0")
        self.assertIn("WEBKIT:W4:PAINT with the pixels it counted", failed(log))
        # No paint, pixel or ink line at all (the judge must not skip the checks).
        bare = "".join(line + "\n" for line in GOOD.splitlines() if ":PAINT:" not in line and ":INK:" not in line)
        self.assertTrue({"WEBKIT:W4:PAINT with the pixels it counted", "the page has ink (text was drawn)"}
                        <= failed(bare))
        self.assertFalse(portjudge.passed(portjudge.judge(bare, HOST)))

    def test_the_program_failing_to_paint_fails(self) -> None:
        log = GOOD.replace("WEBKIT:W4:PASS\n", "WEBKIT:W4:FAIL:paint:nothing arrived\n")
        self.assertIn("no failure lines", failed(log))
        self.assertIn("WEBKIT:W4:PASS", failed(log))

    def test_a_missing_host_reference_fails(self) -> None:
        self.assertEqual(failed(GOOD, None), {"the host's reference run exists"})
        self.assertFalse(portjudge.passed(portjudge.judge(GOOD)))
        self.assertFalse(portjudge.passed(portjudge.judge(GOOD, "")))

    def test_each_missing_marker_fails(self) -> None:
        for marker, name in (("WEBKIT:W4:UP\n", "WEBKIT:W4:UP"),
                             ("WEBKIT:W4:WEBPROCESS:UP:12\n", "WEBKIT:W4:WEBPROCESS:UP"),
                             ("WEBKIT:W4:NETWORKPROCESS:UP:13\n", "WEBKIT:W4:NETWORKPROCESS:UP"),
                             ("WEBKIT:W4:LOAD:about:blank\n", "WEBKIT:W4:LOAD:about:blank"),
                             ("WEBKIT:W4:TITLE:hello lazyos\n", "WEBKIT:W4:TITLE:hello lazyos"),
                             ("WEBKIT:W4:PASS\n", "WEBKIT:W4:PASS")):
            self.assertIn(name, failed(GOOD.replace(marker, "")), marker)

    def test_the_descriptor_mode_is_required(self) -> None:
        fds = "WEBKIT:W4:FDS: the run's descriptors are above 1024"
        monitor = "WEBKIT:W4:MONITOR: a socket above 1024 sees its messages and its close"
        self.assertEqual(failed(GOOD.replace(FDS + "\n", "")), {fds})
        self.assertEqual(failed(GOOD.replace(MONITOR + "\n", "")), {monitor})
        # The descriptors were held, but too few: the run's IPC was below 1024.
        self.assertEqual(failed(GOOD.replace("next=1103", "next=900")), {fds})
        self.assertIn(monitor, failed(GOOD.replace("fd=1103", "fd=900")))
        self.assertIn(monitor, failed(GOOD.replace("messages=3", "messages=2")))
        # The monitor test's own failure.
        log = GOOD.replace(MONITOR, "WEBKIT:W4:FAIL:monitor:the monitor did not see the peer close within 5 s")
        self.assertEqual(failed(log), {monitor, "no failure lines"})

    def test_the_spawn_probe_is_required(self) -> None:
        spawn = "WEBKIT:W4:SPAWN: a program started during an engine launch inherits no IPC socket"
        self.assertEqual(failed(GOOD.replace(SPAWN + "\n", "")), {spawn})
        # A probe that never showed it can see a leak proves nothing.
        self.assertEqual(failed(GOOD.replace(CONTROL + "\n", "")), {spawn})
        # Probed at only one launch.
        self.assertEqual(failed(GOOD.replace("launches=2", "launches=1")), {spawn})
        # The probe child found an engine socket: a leak, even if the parent then printed PASS.
        leak = GOOD.replace(SPAWN, "WEBKIT:W4:SPAWN:LEAK:fd=1104\n" + SPAWN)
        self.assertEqual(failed(leak), {spawn})
        log = GOOD.replace(SPAWN, "WEBKIT:W4:FAIL:spawn:a program started while an engine process was launching inherited an IPC socket")
        self.assertEqual(failed(log), {spawn, "no failure lines"})

    def test_a_wrong_title_fails(self) -> None:
        self.assertIn("WEBKIT:W4:TITLE:hello lazyos", failed(GOOD.replace("TITLE:hello lazyos", "TITLE:oops")))

    def test_a_dead_process_pid_fails(self) -> None:
        self.assertIn("WEBKIT:W4:WEBPROCESS:UP", failed(GOOD.replace("WEBPROCESS:UP:12", "WEBPROCESS:UP:0")))

    def test_pixels_without_a_paint_line_fail(self) -> None:
        log = GOOD.replace("WEBKIT:W4:PAINT:800x600:0badf00d\n", "")
        self.assertIn("WEBKIT:W4:PAINT with the pixels it counted", failed(log))

    def test_a_fail_line_or_a_crash_fails_even_after_pass(self) -> None:
        self.assertIn("no failure lines", failed(GOOD + "WEBKIT:W4:FAIL:paint:oops\n"))
        self.assertIn("no failure lines", failed(GOOD + "user: task 7 killed by SIGSEGV\n"))
        self.assertIn("no failure lines", failed(GOOD + "user: code at 0x1234: 00 00\n"))

    def test_pass_must_be_a_whole_line(self) -> None:
        self.assertIn("WEBKIT:W4:PASS", failed(GOOD.replace("WEBKIT:W4:PASS\n", "WEBKIT:W4:PASS:partial\n")))

    def test_the_hosts_paint_must_match(self) -> None:
        host = HOST
        self.assertEqual({v.name for v in portjudge.judge(GOOD, host) if not v.ok}, set())
        other = host.replace("0badf00d", "00000000")
        self.assertIn("paint equals the host's", {v.name for v in portjudge.judge(GOOD, other) if not v.ok})
        shifted = host.replace("bbox=8,15,15,22", "bbox=8,12,16,20")
        self.assertIn("ink equals the host's", {v.name for v in portjudge.judge(GOOD, shifted) if not v.ok})
        self.assertIn("paint equals the host's", {v.name for v in portjudge.judge(GOOD, "garbage") if not v.ok})

    def test_a_page_without_ink_fails(self) -> None:
        blank = GOOD.replace("pixels=42:bbox=8,15,15,22", "pixels=0:bbox=800,600,-1,-1")
        self.assertIn("the page has ink (text was drawn)", failed(blank))

    def test_the_stage_reached(self) -> None:
        self.assertEqual(portjudge.reached(""), "none")
        self.assertEqual(portjudge.reached("WEBKIT:W4:UP\n"), "UP")
        self.assertEqual(portjudge.reached("WEBKIT:W4:UP\nWEBKIT:W4:LOAD:about:blank\n"), "LOAD")
        self.assertEqual(portjudge.reached(GOOD), "PASS")


if __name__ == "__main__":
    unittest.main()
