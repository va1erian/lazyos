#!/usr/bin/env python3
"""The F3 judges (`fuse_checks.judge_steps`, `judge_tree`) fail when they
should: a log with every step right and a server holding the expected tree
pass; a wrong status, missing output, a failed mount listed under `/mnt` or a
different file on the server fail.

    python tools/smb/test_fuse_judge.py
"""

from __future__ import annotations

import hashlib
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fuse_checks as f3  # noqa: E402

MD5 = hashlib.md5(f3.BIG).hexdigest()


def good_log() -> str:
    lines = ["SMBFUSE:UP /mnt/share dialect=0x0210 signing=off", "F3:mount-share:0"]
    for step in f3.STEPS:
        lines += [step.shows, f"F3:{step.name}:{0 if step.passes else 1}"]
    lines += ["SMBFUSE:UP /mnt/signed dialect=0x0210 signing=on", "F3:mount-signed:0",
              MD5, "F3:signedcp:0",
              "SMBFUSE:FAIL SESSION_SETUP: LOGON_FAILURE", "F3:mount-bad:5",
              "SMBFUSE:FAIL TREE_CONNECT: BAD_NETWORK_NAME", "F3:mount-noshare:8",
              "/ # ls /mnt; echo F3:mounts:$?", "share   signed", "F3:mounts:0"]
    return "\n".join(lines) + "\n"


class StepTests(unittest.TestCase):
    def test_a_good_log_passes(self) -> None:
        self.assertEqual(f3.judge_steps(good_log()), [])

    def test_a_wrong_status_fails(self) -> None:
        self.assertTrue(f3.judge_steps(good_log().replace("F3:cpin:0", "F3:cpin:1")))
        self.assertTrue(f3.judge_steps(good_log().replace("F3:notempty:1", "F3:notempty:0")))
        self.assertTrue(f3.judge_steps(good_log().replace("F3:mount-bad:5", "F3:mount-bad:0")))

    def test_missing_output_fails(self) -> None:
        self.assertTrue(f3.judge_steps(good_log().replace(MD5, "0" * 32)))
        self.assertTrue(f3.judge_steps(good_log().replace("signing=on", "signing=off")))
        self.assertTrue(f3.judge_steps(good_log().replace("LOGON_FAILURE", "ACCESS_DENIED")))

    def test_a_step_that_never_ran_fails(self) -> None:
        self.assertTrue(f3.judge_steps(good_log().replace("F3:sync:0\n", "")))

    def test_a_failed_mount_left_under_mnt_fails(self) -> None:
        self.assertTrue(f3.judge_steps(good_log().replace("share   signed", "bad   share   signed")))


class TreeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = tempfile.TemporaryDirectory()
        self.root = Path(self.dir.name)
        for name, body in f3.MAIN_FILES.items():
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_bytes(body)

    def tearDown(self) -> None:
        self.dir.cleanup()

    def test_the_expected_tree_passes(self) -> None:
        self.assertEqual(f3.judge_tree(self.root, f3.MAIN_FILES, f3.MAIN_DIRS), [])

    def test_different_bytes_fail(self) -> None:
        (self.root / "patch.bin").write_bytes(f3.SEQ)
        self.assertTrue(f3.judge_tree(self.root, f3.MAIN_FILES, f3.MAIN_DIRS))

    def test_a_left_over_file_fails(self) -> None:
        (self.root / "old.txt").write_bytes(b"remove me\n")
        self.assertTrue(f3.judge_tree(self.root, f3.MAIN_FILES, f3.MAIN_DIRS))


if __name__ == "__main__":
    unittest.main()
