#!/usr/bin/env python3
"""Tests for qemu_serial.py: the serial port's file chardev and its tail.

Run: python tools/test_qemu_serial.py
"""

from __future__ import annotations

import io
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qemu_serial  # noqa: E402


def pairs(command: list[str], flag: str) -> list[str]:
    return [command[i + 1] for i, item in enumerate(command) if item == flag]


class ArgsTests(unittest.TestCase):
    def test_default_serial_is_a_file_and_the_monitor_keeps_stdio(self) -> None:
        command = qemu_serial.qemu_args(Path("target/serial.log"))
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
        command = qemu_serial.qemu_args(Path("target/serial.log"), stdio=True)
        (chardev,) = pairs(command, "-chardev")
        self.assertTrue(chardev.startswith("stdio,id=ser0,mux=on,logfile="))
        self.assertEqual(pairs(command, "-serial"), ["chardev:ser0"])
        self.assertEqual(pairs(command, "-mon"), ["chardev=ser0,mode=readline"])

    def test_a_comma_in_the_path_is_doubled_for_qemu(self) -> None:
        command = qemu_serial.qemu_args(Path("a,b") / "s.log")
        self.assertIn(",,", pairs(command, "-chardev")[0])


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


if __name__ == "__main__":
    unittest.main()
