#!/usr/bin/env python3
"""Tests for qemu_serial.py (the serial port's file chardev and its tail) and
serial_integrity.py (what a log with dropped lines means to a judge).

Run: python tools/test_qemu_serial.py
"""

from __future__ import annotations

import argparse
import io
import os
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qemu_serial  # noqa: E402
import serial_integrity  # noqa: E402


def ns(log=None, stdio=False, image=None) -> argparse.Namespace:
    return argparse.Namespace(serial_log=log, serial_stdio=stdio, image=image)


def pairs(command: list[str], flag: str) -> list[str]:
    return [command[i + 1] for i, item in enumerate(command) if item == flag]


class ArgsTests(unittest.TestCase):
    def test_default_serial_is_a_file_and_the_monitor_keeps_stdio(self) -> None:
        command = qemu_serial.qemu_args(ns("target/serial.log"))
        chardevs = pairs(command, "-chardev")
        self.assertTrue(any(c.startswith("file,id=ser0,path=") for c in chardevs))
        self.assertEqual(pairs(command, "-serial"), ["chardev:ser0"])
        # The monitor and Ctrl-A x live on a muxed stdio, off the serial path.
        stdio = [c for c in chardevs if c.startswith("stdio")]
        self.assertEqual(len(stdio), 1)
        self.assertIn("mux=on", stdio[0])
        self.assertIn("signal=off", stdio[0])
        self.assertEqual(pairs(command, "-mon"), ["chardev=mon0,mode=readline"])
        self.assertNotIn("mon:stdio", command)

    def test_serial_stdio_is_the_old_muxed_device_with_a_log_copy(self) -> None:
        command = qemu_serial.qemu_args(ns("target/serial.log", stdio=True))
        (chardev,) = pairs(command, "-chardev")
        self.assertTrue(chardev.startswith("stdio,id=ser0,mux=on,logfile="))
        self.assertEqual(pairs(command, "-serial"), ["chardev:ser0"])
        self.assertEqual(pairs(command, "-mon"), ["chardev=ser0,mode=readline"])

    def test_a_comma_in_the_path_is_doubled_for_qemu(self) -> None:
        command = qemu_serial.qemu_args(ns(str(Path("a,b") / "s.log")))
        self.assertIn(",,", pairs(command, "-chardev")[0])


class LogNameTests(unittest.TestCase):
    def test_the_default_name_has_the_image_and_the_moment(self) -> None:
        name = qemu_serial.default_log_name("target/lazyos.img", 86400 * 365)
        self.assertRegex(name, r"^serial-lazyos-\d{8}-\d{6}\.log$")
        other = qemu_serial.default_log_name("shots/other image.img", 86400 * 365)
        self.assertIn("serial-other_image-", other)
        self.assertNotEqual(name, other)

    def test_the_default_log_is_stable_within_a_run_and_in_target(self) -> None:
        args = ns(image="target/lazyos.img")
        log = qemu_serial.resolve_log(args)
        self.assertEqual(log.parent, qemu_serial.LOG_DIR)
        self.assertEqual(qemu_serial.resolve_log(args), log)
        self.assertIn(log.name, " ".join(qemu_serial.qemu_args(args)))

    def test_an_explicit_log_is_used_as_given(self) -> None:
        self.assertEqual(qemu_serial.resolve_log(ns("x.log")), Path("x.log"))

    def test_only_the_newest_logs_are_kept(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp)
            for index in range(7):
                log = folder / f"serial-a-{index}.log"
                log.write_text("x")
                os.utime(log, (1000 + index, 1000 + index))
            (folder / "other.log").write_text("keep")
            gone = qemu_serial.prune_logs(folder, keep=3)
            self.assertEqual(sorted(p.name for p in gone),
                             [f"serial-a-{i}.log" for i in range(4)])
            self.assertTrue((folder / "other.log").exists())
            self.assertEqual(len(list(folder.glob("serial-*.log"))), 3)


class TailTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.path = Path(tmp.name) / "serial.log"

    def test_copies_a_file_that_appears_late_and_grows(self) -> None:
        out = io.BytesIO()
        tail = qemu_serial.Tail(self.path, out)
        tail.start()
        time.sleep(0.1)
        with open(self.path, "wb") as f:
            f.write(b"one\n")
            f.flush()
            time.sleep(0.15)
            f.write(b"two\n")
        tail.stop()
        self.assertEqual(out.getvalue(), b"one\ntwo\n")

    def test_stop_hands_over_the_rest(self) -> None:
        out = io.BytesIO()
        self.path.write_bytes(b"x" * (3 * qemu_serial.CHUNK + 5))
        tail = qemu_serial.Tail(self.path, out)
        tail.start()
        tail.stop()
        self.assertEqual(len(out.getvalue()), 3 * qemu_serial.CHUNK + 5)

    def test_a_console_that_stops_reading_does_not_hang_the_launcher(self) -> None:
        release = threading.Event()

        class Stuck:
            def write(self, data: bytes) -> int:
                release.wait(30)
                return len(data)

            def flush(self) -> None:
                pass

        self.path.write_bytes(b"hello\n")
        tail = qemu_serial.Tail(self.path, Stuck())
        tail.start()
        time.sleep(0.1)
        started = time.monotonic()
        tail.stop()
        took = time.monotonic() - started
        release.set()
        self.assertLess(took, qemu_serial.STOP_SECONDS + 1.0)

    def test_text_streams_get_decoded_text(self) -> None:
        out = io.StringIO()
        self.path.write_bytes("caf\xe9 ✓\n".encode("utf-8") + b"\xff\n")
        tail = qemu_serial.Tail(self.path, out)
        tail.start()
        tail.stop()
        self.assertIn("✓", out.getvalue())


class PrepareTests(unittest.TestCase):
    def test_removes_a_stale_log_and_makes_the_folder(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "new" / "serial.log"
            qemu_serial.prepare_log(log)
            self.assertTrue(log.parent.is_dir())
            log.write_bytes(b"old")
            qemu_serial.prepare_log(log)
            self.assertFalse(log.exists())


class IntegrityTests(unittest.TestCase):
    def test_a_complete_log_has_no_problem(self) -> None:
        self.assertEqual(serial_integrity.problems("WEB:UP:PASS\nSERIAL:BACKLOG bytes=5000\n"), [])

    def test_every_dropped_report_is_a_problem(self) -> None:
        text = "A\nSERIAL:DROPPED bytes=300 total=300\nB\n[12.5] SERIAL:DROPPED bytes=1 total=301\n"
        found = serial_integrity.problems(text)
        self.assertEqual(len(found), 2)
        self.assertIn("bytes=300", found[0])

    def test_the_judges_and_the_session_use_it(self) -> None:
        sys.path.insert(0, str(Path(__file__).resolve().parent / "boot"))
        import judge as boot_judge  # noqa: E402
        failures = boot_judge.judge_serial("SERIAL:DROPPED bytes=9 total=9\n", "bios")
        self.assertTrue(any("dropped" in f for f in failures))


if __name__ == "__main__":
    unittest.main()
