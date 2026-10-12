#!/usr/bin/env python3
"""The install harness's judge fails when it should (docs/grub-install-plan.md I3).

Run: python tools/install/test_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge  # noqa: E402

SYS = "11111111-2222-4333-8444-555555555555"
HOME = "66666666-7777-4888-9999-aaaaaaaaaaaa"
INSTALL = f"""plan:
  system: FORMAT disk.img#3 (partition 3, "lazyos-system", 2048 MiB, now nothing recognised) as ext2 {SYS}
  home:   FORMAT disk.img#4 (partition 4, "lazyos-home", 256 MiB, now nothing recognised) as ext2 {HOME}
system: clean (ext2 checker)
done
"""
BOOT = f"""BOOT:MEDIA:uefi
block: ahci0p1 type 0xef lba 2048 sectors 204800
fs: root {SYS} is ahci0p3
fs: home {HOME} is ahci0p4
fs: mounted fat (LAZYBOOT) at /boot (ro,nosuid,noexec)
FS:ROOT:ahci0p3
USBD:HID:KBD
PKGD:PROVISION:DONE installed=12 upgraded=0 kept=0 failed=0 free=900000000
SHELL:DESKTOP:PASS
power: filesystems synced
"""
DESKTOP = {"nonbackground_ratio": 0.9, "distinct_colors_q4": 400}
HOST = "system.check: clean (ext2 checker)\nhome.check: clean (ext2 checker)\nsystem.apps: 12\nsystem.logs: 3\n"


class JudgeTests(unittest.TestCase):
    def test_a_good_run_passes(self) -> None:
        self.assertEqual(judge.judge_install(INSTALL, {"g": b"1"}, {"g": b"1"}), [])
        self.assertEqual(judge.install_uuids(INSTALL), (SYS, HOME))
        self.assertEqual(judge.judge_boot(BOOT, DESKTOP, (SYS, HOME), True), [])
        self.assertEqual(judge.judge_host(HOST), [])
        self.assertEqual(judge.judge_linux("LINUX:STANDIN\n"), [])

    def test_install_failures(self) -> None:
        self.assertTrue(judge.judge_install(INSTALL.replace("done", "")))
        self.assertTrue(judge.judge_install(INSTALL + "lazyos-install: boom\n"))
        self.assertTrue(judge.judge_install(INSTALL, {"grub.cfg": b"a"}, {"grub.cfg": b"b"}))
        self.assertTrue(judge.judge_install(INSTALL, {"grub.cfg": b"a"}, {}))

    def test_boot_failures(self) -> None:
        cases = {
            "root elsewhere": BOOT.replace("is ahci0p3", "is ahci0p2"),
            "ram root": BOOT.replace("FS:ROOT:ahci0p3", "FS:ROOT:ram0p2"),
            "home missing": BOOT.replace(f"fs: home {HOME} is ahci0p4\n", ""),
            "no gpt": BOOT.replace("block: ahci0p1 type 0xef", "block: ahci0: GPT"),
            "no /boot": BOOT.replace("at /boot", "at /x"),
            "not synced": BOOT.replace(judge.SYNCED, ""),
            "panic": BOOT + "LazyOS PANIC: x\n",
            "rebooted": BOOT + "BOOT:MEDIA:uefi\n",
        }
        for name, log in cases.items():
            self.assertTrue(judge.judge_boot(log, DESKTOP, (SYS, HOME), True), name)
        self.assertTrue(judge.judge_boot(BOOT, {"nonbackground_ratio": 0.0, "distinct_colors_q4": 1}, (SYS, HOME), False))
        self.assertTrue(judge.judge_boot(BOOT, DESKTOP, ("0" * 8 + SYS[8:], HOME), False))

    def test_host_and_linux_failures(self) -> None:
        self.assertTrue(judge.judge_host(HOST.replace("home.check: clean", "home.check: FAIL x")))
        self.assertTrue(judge.judge_host(HOST.replace("system.apps: 12", "system.apps: 0")))
        self.assertTrue(judge.judge_host(""))
        self.assertTrue(judge.judge_linux(""))
        self.assertTrue(judge.judge_linux("LINUX:STANDIN\nBOOT:MEDIA:uefi\n"))


if __name__ == "__main__":
    unittest.main()
