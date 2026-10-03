#!/usr/bin/env python3
"""The boot judge fails when it should (no QEMU needed).

    python tools/boot/test_run.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import judge  # noqa: E402

GOOD = """LazyOS: kernel entered
LazyOS: framebuffer 1280x800 Bgr
BOOT:MEDIA:uefi
block: ramdisk registered (109051904 bytes)
block: ram0p1 type 0x01 lba 2048 sectors 2048
block: ram0p2 type 0x83 lba 4096 sectors 208896
fs: root 504ac84c-145b-41ae-9670-79c880969ac3 is ram0p2
FS:ROOT:ram0p2
USBD:HID:KBD port=1 slot=1 vendor=0x0627 product=0x0001 interface=0 dci=3 regions=4
USBD:READY devices=2
SHELL:DESKTOP:PASS icons=6
"""

DESKTOP = {"nonbackground_ratio": 0.98, "distinct_colors_q4": 60}


PERSIST = GOOD.replace("SHELL:DESKTOP:PASS icons=6\n", "") + """\
USBD:MSC:DISK port=0-1 slot=1 id=usb0 sectors=356352
fs: mounted usb0p3 at /home (late, home volume lazyhome)
INIT:HOME mounted
LOGIN:OK:PASS user=user
0123abcd
INIT:SHUTDOWN:BEGIN
power: filesystems synced
"""


class PersistJudge(unittest.TestCase):
    def test_a_good_boot_passes(self):
        self.assertEqual(judge.judge_persist(PERSIST, "uefi", "0123abcd", True), [])

    def test_a_missing_late_mount_fails(self):
        log = PERSIST.replace("fs: mounted usb0p3", "fs: skipped usb0p3")
        self.assertTrue(judge.judge_persist(log, "uefi", "0123abcd", False))

    def test_a_wrong_nonce_fails(self):
        self.assertTrue(judge.judge_persist(PERSIST, "uefi", "ffff", False))

    def test_an_unclean_second_boot_fails(self):
        log = PERSIST + "ext2: usb0p3 was not cleanly unmounted\n"
        self.assertEqual(judge.judge_persist(log, "uefi", "0123abcd", False), [])
        self.assertTrue(judge.judge_persist(log, "uefi", "0123abcd", True))

    def test_power_off_before_sync_order(self):
        log = PERSIST.replace("INIT:SHUTDOWN:BEGIN\npower: filesystems synced\n",
                              "power: filesystems synced\nINIT:SHUTDOWN:BEGIN\n")
        self.assertTrue(judge.judge_persist(log, "uefi", "0123abcd", False))


class SerialJudge(unittest.TestCase):
    def test_a_good_uefi_boot_passes(self):
        self.assertEqual(judge.judge_serial(GOOD, "uefi"), [])

    def test_the_wrong_firmware_fails(self):
        failures = judge.judge_serial(GOOD, "bios")
        self.assertTrue(any("BOOT:MEDIA:uefi" in f for f in failures), failures)

    def test_a_missing_media_marker_fails(self):
        log = GOOD.replace("BOOT:MEDIA:uefi\n", "")
        self.assertTrue(judge.judge_serial(log, "uefi"))

    def test_a_reboot_loop_fails(self):
        self.assertTrue(judge.judge_serial(GOOD + GOOD, "uefi"))

    def test_a_disk_root_fails(self):
        log = GOOD.replace("FS:ROOT:ram0p2", "FS:ROOT:virtio0p3")
        failures = judge.judge_serial(log, "uefi")
        self.assertTrue(any("virtio0p3" in f for f in failures), failures)
        self.assertEqual(judge.judge_serial(log, "uefi", root=r"virtio0p3"), [])

    def test_the_legacy_layout_fails(self):
        self.assertTrue(judge.judge_serial(GOOD.replace("FS:ROOT:ram0p2", "FS:ROOT:ram0"),
                                           "uefi"))
        self.assertTrue(judge.judge_serial(GOOD.replace("FS:ROOT:ram0p2", "FS:ROOT:none"),
                                           "uefi"))

    def test_no_desktop_fails_unless_not_asked_for(self):
        log = GOOD.replace("SHELL:DESKTOP:PASS icons=6\n", "")
        self.assertTrue(judge.judge_serial(log, "uefi"))
        self.assertEqual(judge.judge_serial(log, "uefi", ready=None), [])

    def test_missing_usb_input_fails(self):
        log = "\n".join(l for l in GOOD.splitlines() if "USBD:HID:KBD" not in l)
        self.assertTrue(any("USBD:HID:KBD" in f for f in judge.judge_serial(log, "uefi")))
        self.assertEqual(judge.judge_serial(log, "uefi", usb_input=False), [])

    def test_a_usbd_fatal_fails(self):
        log = GOOD + "USBD:FATAL no xHCI capability\n"
        self.assertTrue(any("usbd error" in f for f in judge.judge_serial(log, "uefi")))

    def test_a_panic_fails(self):
        log = GOOD + "LazyOS PANIC: panicked at kernel/src/mem/mod.rs:1:1\n"
        self.assertTrue(any("panicked" in f for f in judge.judge_serial(log, "uefi")))


class PixelJudge(unittest.TestCase):
    def test_a_desktop_passes(self):
        self.assertEqual(judge.judge_pixels(DESKTOP), [])

    def test_a_black_screen_fails(self):
        self.assertTrue(judge.judge_pixels({"nonbackground_ratio": 0.0,
                                            "distinct_colors_q4": 1}))

    def test_a_text_console_fails(self):
        # The firmware or kernel console: some text on black, few colours.
        self.assertTrue(judge.judge_pixels({"nonbackground_ratio": 0.08,
                                            "distinct_colors_q4": 6}))

    def test_a_missing_screenshot_fails(self):
        self.assertTrue(judge.judge_pixels({"error": "no screenshot"}))


class Milestones(unittest.TestCase):
    def test_first_occurrence_in_seconds(self):
        stamped = [(3.0, "LazyOS: kernel entered"), (3.5, "BOOT:MEDIA:bios"),
                   (9.25, "FS:ROOT:ram0p2"), (80.0, "SHELL:DESKTOP:PASS icons=6"),
                   (99.0, "FS:ROOT:later")]
        self.assertEqual(judge.milestones(stamped),
                         {"kernel": 3.0, "media": 3.5, "root": 9.2, "ready": 80.0})

    def test_missing_milestones_are_absent(self):
        self.assertEqual(judge.milestones([(1.0, "noise")]), {})


if __name__ == "__main__":
    unittest.main()
