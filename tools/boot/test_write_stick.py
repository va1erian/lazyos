#!/usr/bin/env python3
"""The Windows release/restore path of write_stick.py, against a fake PowerShell.

    python tools/boot/test_write_stick.py
"""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import write_stick  # noqa: E402

DISK = write_stick.Disk(r"\\.\PhysicalDrive6", 16 << 30, "USB stick", True, True, [], number=6)


def fake_powershell(fail_on=()):
    """A subprocess.run stand-in that records commands and fails those containing `fail_on`."""
    calls = []

    def run(argv, **kwargs):
        command = argv[-1]
        calls.append(command)
        if any(word in command for word in fail_on):
            return subprocess.CompletedProcess(argv, 1, "", "Access is denied.")
        return subprocess.CompletedProcess(argv, 0, "", "")

    return run, calls


class WindowsRelease(unittest.TestCase):
    def test_failure_message_carries_powershells_own_words(self):
        run, _ = fake_powershell(fail_on=("Set-Disk",))
        with mock.patch.object(subprocess, "run", run):
            with self.assertRaises(write_stick.PowerShellError) as raised:
                write_stick.windows_offline(DISK, True)
        self.assertIn("Access is denied.", str(raised.exception))
        self.assertIn("Set-Disk -Number 6", str(raised.exception))

    def test_offline_works_then_comes_back_online(self):
        run, calls = fake_powershell()
        with mock.patch.object(subprocess, "run", run):
            was_offline = write_stick.windows_release(DISK)
            write_stick.windows_restore(DISK, was_offline)
        self.assertTrue(was_offline)
        self.assertTrue("-IsOffline $true" in calls[0] and "-IsOffline $false" in calls[1])

    def test_refused_offline_falls_back_to_clearing_the_disk(self):
        run, calls = fake_powershell(fail_on=("Set-Disk",))
        with mock.patch.object(subprocess, "run", run), mock.patch("sys.stderr"):
            was_offline = write_stick.windows_release(DISK)
            write_stick.windows_restore(DISK, was_offline)
        self.assertFalse(was_offline)
        self.assertTrue(any("Clear-Disk -Number 6 -RemoveData" in c for c in calls))
        self.assertTrue(calls[-1].endswith("Update-Disk -Number 6"))
        self.assertFalse(any("-IsOffline $false" in c for c in calls))

    def test_if_clearing_fails_too_the_error_stops_the_write(self):
        run, _ = fake_powershell(fail_on=("Set-Disk", "Clear-Disk"))
        with mock.patch.object(subprocess, "run", run), mock.patch("sys.stderr"):
            with self.assertRaises(write_stick.PowerShellError):
                write_stick.windows_release(DISK)


if __name__ == "__main__":
    unittest.main()
