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
        run, _ = fake_powershell(fail_on=("Clear-Disk",))
        with mock.patch.object(subprocess, "run", run):
            with self.assertRaises(write_stick.PowerShellError) as raised:
                write_stick.windows_release(DISK)
        self.assertIn("Access is denied.", str(raised.exception))
        self.assertIn("Clear-Disk -Number 6", str(raised.exception))

    def test_release_clears_the_partition_table_and_never_tries_offline(self):
        run, calls = fake_powershell()
        with mock.patch.object(subprocess, "run", run):
            write_stick.windows_release(DISK)
        self.assertEqual(len(calls), 1)
        self.assertIn("Clear-Disk -Number 6 -RemoveData", calls[0])
        self.assertNotIn("IsOffline", calls[0])

    def test_restore_rescans_the_disk(self):
        run, calls = fake_powershell()
        with mock.patch.object(subprocess, "run", run):
            write_stick.windows_restore(DISK)
        self.assertTrue(calls[0].endswith("Update-Disk -Number 6"))


if __name__ == "__main__":
    unittest.main()
