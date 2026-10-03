#!/usr/bin/env python3
"""The storage judge fails when it should (and passes a good log).

    python tools/storage/test_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from judge import judge, judge_unplug  # noqa: E402

NONCE = "a1b2c3"

GOOD = """\
fs: home volume lazyhome not found; /home is a directory on /
USBD:CRED uid=904 caps=0xd00
USBD:MSC:DISK port=0-1 slot=1 id=usb0 vendor=0x46f4 product=0x0001 blocks=67584 wp=0 burst=15 packet=1024
USBD:MSC:DISK port=0-2 slot=2 id=usb1 vendor=0x46f4 product=0x0001 blocks=67584 wp=0 burst=15 packet=1024
USBD:READY devices=0
fs: mounted usb0p1 at /home (late, home volume lazyhome),nosuid
INIT:HOME mounted
LazyOS login: user
LOGIN:OK:PASS user=user uid=1000 session=1 pid=20
$  cat /home/user/usbnote
a1b2c3
$ poweroff
INIT:SHUTDOWN:BEGIN mode=poweroff uid=1000 session=1 reason=""
power: filesystems synced
"""


class Judge(unittest.TestCase):
    def test_good_log_passes(self):
        self.assertEqual(judge(GOOD, NONCE, second=True, other=True), [])

    def test_missing_second_stick_fails_with_other(self):
        text = "\n".join(line for line in GOOD.splitlines() if "id=usb1" not in line)
        self.assertEqual(judge(text, NONCE), [])
        self.assertEqual(len(judge(text, NONCE, other=True)), 1)

    def test_no_late_mount_fails(self):
        text = GOOD.replace("fs: mounted usb0p1 at /home", "fs: skipped usb0p1")
        self.assertTrue(judge(text, NONCE))

    def test_two_late_mounts_fail(self):
        text = GOOD + "fs: mounted usb1p1 at /home (late, home volume lazyhome),nosuid\n"
        self.assertTrue(judge(text, NONCE))

    def test_typed_command_is_not_the_nonce(self):
        text = GOOD.replace("\na1b2c3\n", "\n")
        self.assertEqual(len(judge(text, NONCE)), 1)
        self.assertEqual(len(judge(GOOD, "ffffff")), 1)

    def test_unsynced_power_off_fails(self):
        text = GOOD.replace("power: filesystems synced", "power: sync failed: Io")
        self.assertEqual(len(judge(text, NONCE)), 2)

    def test_unclean_mount_fails_only_on_the_second_boot(self):
        text = GOOD + "ext2: usb0p1 was not cleanly unmounted (unclean stop)\n"
        self.assertEqual(judge(text, NONCE), [])
        self.assertEqual(len(judge(text, NONCE, second=True)), 1)

    def test_a_dirty_root_volume_is_not_the_sticks_fault(self):
        text = "ext2: virtio0p3 was not cleanly unmounted (unclean stop)\n" + GOOD
        self.assertEqual(judge(text, NONCE, second=True), [])

    def test_driver_and_kernel_failures_fail(self):
        for line in ["USBD:MSC:FAIL port=1 read", "USBD:FATAL x", "LazyOS PANIC: boom"]:
            self.assertEqual(len(judge(GOOD + line + "\n", NONCE)), 1, line)

    def test_init_must_see_the_mount(self):
        text = GOOD.replace("INIT:HOME mounted", "INIT:HOME timeout")
        self.assertEqual(len(judge(text, NONCE)), 1)


UNPLUGGED = """\
USBD:MSC:DISK port=0-1 slot=1 id=usb0 vendor=0x46f4 product=0x0001 blocks=67584 wp=0 burst=15 packet=1024
fs: mounted usb0p1 at /home (late, home volume lazyhome),nosuid
LOGIN:OK:PASS user=user uid=1000 session=1 pid=20
$  echo a1b2c3 > /home/user/unplug; echo wrote-$?
wrote-0
USBD:DETACH port=0-1 slot=1 regions=1 functions=msc (unplugged)
USBD:MSC:GONE id=usb0 (detached)
$  echo late > /home/user/late; echo after-$?
sh: can't create /home/user/late: I/O error
after-1
$  ls / > /dev/null; echo alive-$?
alive-0
INIT:SHUTDOWN:BEGIN mode=poweroff uid=1000 session=1 reason=""
power: sync failed: I/O error
"""


class Unplug(unittest.TestCase):
    def test_good_log_passes(self):
        self.assertEqual(judge_unplug(UNPLUGGED), [])

    def test_a_hung_write_fails(self):
        text = UNPLUGGED.replace("after-1\n", "")
        self.assertEqual(len(judge_unplug(text)), 1)

    def test_no_removal_report_fails(self):
        text = UNPLUGGED.replace("USBD:MSC:GONE id=usb0 (detached)", "")
        self.assertEqual(len(judge_unplug(text)), 1)

    def test_no_power_off_fails(self):
        text = UNPLUGGED.replace("power: sync failed: I/O error", "")
        self.assertEqual(len(judge_unplug(text)), 1)

    def test_a_panic_fails(self):
        self.assertEqual(len(judge_unplug(UNPLUGGED + "LazyOS PANIC: x\n")), 1)


if __name__ == "__main__":
    unittest.main()
