#!/usr/bin/env python3
"""The Network Drives judge (`ui_run.judge`) fails when it should: a log
with every marker and a server holding the written file pass, and each
missing piece, a refused permission or a leaked password fails.

    python tools/fuse/test_ui_judge.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ui_run  # noqa: E402

GOOD = "\n".join([
    "MOUNTD:READY interface=0x9d456629506ac305",
    "HEALTH:SVC:PASS os.lazy.netdrives",
    "MOUNTD:START bad kind=ftp pid=41",
    "FTPFUSE:FAIL 10.0.2.2: 530 Login incorrect",
    "NETDRIVES:MOUNT:FAIL name=bad reason=cannot connect or log in (check the host, port, user and password)",
    "NETDRIVES:UNMOUNT:PASS name=bad",
    "MOUNTD:START site kind=ftp pid=42",
    "FTPFUSE:UP /mnt/site",
    "MOUNTD:UP site",
    "NETDRIVES:MOUNT:PASS name=site path=/mnt/site",
    "TERM:OUT:hello from the host",
    "TERM:OUT:WROTE:0",
    "TERM:OUT:OWNER:1000:1000",
    "NETDRIVES:OPEN:PASS path=/mnt/site",
    "NETDRIVES:UNMOUNT:PASS name=site",
    "MOUNTD:STOP site",
    "TERM:OUT:GONE:1",
    "LABEL:DENY label=app:os.lazy.other iface=0x1 method=0x2",
]) + "\n"


class JudgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = tempfile.TemporaryDirectory()
        self.root = Path(self.dir.name)
        (self.root / "fromguest.txt").write_bytes(ui_run.WRITTEN)

    def tearDown(self) -> None:
        self.dir.cleanup()

    def problems(self, text: str = GOOD, logins: int = 2) -> list[str]:
        return ui_run.judge(text, self.root, logins)

    def test_a_good_run_passes(self) -> None:
        self.assertEqual(self.problems(), [])

    def test_each_missing_marker_fails(self) -> None:
        for pattern, why in ui_run.EXPECT:
            line = next(line for line in GOOD.splitlines() if ui_run.re.search(pattern, line))
            with self.subTest(line=line):
                self.assertIn(why, self.problems(GOOD.replace(line + "\n", "")))

    def test_files_owned_by_the_service_fail(self) -> None:
        text = GOOD.replace("OWNER:1000:1000", "OWNER:910:910")
        self.assertTrue(any("owned" in p for p in self.problems(text)))

    def test_a_mount_left_after_unmount_fails(self) -> None:
        text = GOOD.replace("GONE:1", "GONE:0")
        self.assertTrue(any("still answers" in p for p in self.problems(text)))

    def test_a_refused_call_of_the_app_fails(self) -> None:
        text = GOOD + "LABEL:DENY label=app:os.lazy.netdrives iface=0x9d456629506ac305 method=0x1\n"
        self.assertTrue(any("permission missing" in p for p in self.problems(text)))

    def test_the_file_the_server_stored_is_the_verdict(self) -> None:
        (self.root / "fromguest.txt").write_bytes(b"truncated")
        self.assertTrue(any("fromguest.txt" in p for p in self.problems()))
        (self.root / "fromguest.txt").unlink()
        self.assertTrue(any("fromguest.txt" in p for p in self.problems()))

    def test_both_logins_must_reach_the_server(self) -> None:
        self.assertTrue(any("login" in p for p in self.problems(logins=1)))

    def test_a_leaked_password_fails(self) -> None:
        text = GOOD + "spawn ftpfuse 10.0.2.2:2121 user=lazy pass=os name=site\n"
        self.assertTrue(any("password" in p for p in self.problems(text)))

    def test_an_smb_run_judges_smbfuse_and_its_password(self) -> None:
        smb = (GOOD.replace("kind=ftp", "kind=smb").replace("FTPFUSE:UP", "SMBFUSE:UP")
               .replace("FTPFUSE:FAIL 10.0.2.2: 530 Login incorrect", "SMBFUSE:FAIL SESSION_SETUP: LOGON_FAILURE"))
        judge = ui_run.judge
        self.assertEqual(judge(smb, self.root, 2, ui_run.SMB, "Secret123"), [])
        self.assertTrue(judge(GOOD, self.root, 2, ui_run.SMB, "Secret123"), "an FTP log passes as SMB")
        leaked = smb + "Secret123\n"
        self.assertTrue(any("SMB password" in p for p in judge(leaked, self.root, 2, ui_run.SMB, "Secret123")))


if __name__ == "__main__":
    unittest.main()
