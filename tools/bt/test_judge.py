#!/usr/bin/env python3
"""The Bluetooth judges must fail when they should (`python tools/bt/test_judge.py`)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge  # noqa: E402

ADDR = "00:1A:7D:DA:71:13"

# A real run's lines (tools/bt/run.py, QEMU with WHPX; the controller is
# libs/btsim behind fakebt).
GOOD = f"""init: started btd (pid 5, attempt 1)
BT:CRED uid=916 caps=0x400
BT:WAIT no Bluetooth controller yet
USBD:CRED uid=904 caps=0xd00
USBD:XHCI hc=0 vendor=0x1b36 device=0x000d version=0x100 ports=8 usb2=4 usb3=4 slots=12
USBD:PORT port=0-5 speed=High
USBD:DESC:DEVICE port=0-5 {judge.DEVICE}
USBD:DESC:CONFIG port=0-5 {judge.CONFIG}
USBD:BT:BIND port=0-5 slot=1 vendor=0x0a12 product=0x0001 interface=0 intr=3 bulk_in=5 bulk_out=4 chunk=512
USBD:READY devices=1 controllers=1
USBD:BT:SERVE name=os.lazy.usb.bt
USBD:BT:ATTACH owner=5 ring=1 slots=64
BT:LINK ring=1
BT:CTRL:PASS:hci=9 lmp=9 vendor=0xffff addr=**:**:**:**:71:13
"""

UNPLUG = GOOD + """USBD:BT:UNBIND port=0-5
USBD:BT:DETACH owner=5 ring=1 controller unplugged
USBD:DETACH port=0-5 slot=1 regions=1 functions=bt (unplugged)
BT:CTRL:GONE the controller was unplugged
BT:WAIT no Bluetooth controller yet
USBD:BT:WITHDRAW name=os.lazy.usb.bt
"""


class Judge(unittest.TestCase):
    def assertFails(self, log, unplug=False, why=""):
        failures = judge.judge(log, ADDR, unplug)
        self.assertNotEqual(failures, [], "the judge passed a bad log")
        if why:
            self.assertTrue(any(why in f for f in failures), f"{why!r} not in {failures}")

    def test_good_logs_pass(self):
        self.assertEqual(judge.judge(GOOD, ADDR), [])
        self.assertEqual(judge.judge(UNPLUG, ADDR, unplug=True), [])

    def test_an_unbind_in_a_run_that_did_not_unplug_fails(self):
        self.assertFails(UNPLUG, why="did not unplug")

    def test_nothing_bound(self):
        self.assertFails(GOOD.replace("USBD:BT:BIND", "USBD:BT:SKIP"), why="did not bind")

    def test_only_interface_zero_binds_once(self):
        bind = next(line for line in GOOD.splitlines() if "USBD:BT:BIND" in line)
        self.assertFails(GOOD + bind + "\n", why="only interface 0")
        self.assertFails(GOOD.replace("interface=0", "interface=1"), why="interface 1")

    def test_wrong_endpoints_or_device(self):
        self.assertFails(GOOD.replace("bulk_in=5", "bulk_in=7"), why="endpoints")
        self.assertFails(GOOD.replace("product=0x0001", "product=0x0002"), why="not the fake")

    def test_the_descriptors_must_be_the_fakes(self):
        self.assertFails(GOOD.replace(judge.DEVICE, judge.DEVICE[:-2] + "ff"), why="device descriptor")
        self.assertFails(GOOD.replace(judge.CONFIG, judge.CONFIG[:-2] + "ff"), why="configuration")

    def test_the_service_and_the_rings_must_have_come_up(self):
        self.assertFails(GOOD.replace("USBD:BT:SERVE", "USBD:BT:NOPE"), why="never registered")
        self.assertFails(GOOD.replace("slots=64", "slots=16"), why="rings were never attached")

    def test_btd_must_run_as_bt_with_one_capability(self):
        self.assertFails(GOOD.replace("uid=916", "uid=0"), why="ran as uid")
        self.assertFails(GOOD.replace("caps=0x400", "caps=0xd00"), why="ran as uid")
        self.assertFails(GOOD.replace("BT:CRED", "BT:CREDX"), why="never started")

    def test_the_controller_must_come_up_with_the_fakes_answers(self):
        self.assertFails(GOOD.replace("BT:CTRL:PASS", "BT:CTRL:FAIL"))
        self.assertFails(GOOD.replace("hci=9", "hci=7"), why="not the fake's")
        self.assertFails(GOOD.replace("vendor=0xffff", "vendor=0x0002"), why="not the fake's")
        self.assertFails(GOOD.replace("71:13", "00:00"), why="address ends")
        self.assertFails(GOOD.replace(" addr=**:**:**:**:71:13", ""), why="no BT:CTRL:PASS")

    def test_the_full_address_must_not_be_logged(self):
        self.assertFails(GOOD + "debug 00:1a:7d:da:71:13\n", why="full Bluetooth address")
        self.assertFails(GOOD + "debug 001A7DDA7113\n", why="full Bluetooth address")

    def test_driver_errors(self):
        self.assertFails(GOOD + "USBD:PANIC boom\n")
        self.assertFails(GOOD + "USBD:BT:FAIL port=0-5 start: timed out\n")
        self.assertFails(GOOD + "BT:PANIC panicked\n")
        self.assertFails(GOOD + "USBD:PORT:FAIL port=0-6 x\n")

    def test_order_matters(self):
        lines = GOOD.splitlines()
        ctrl = next(i for i, line in enumerate(lines) if "BT:CTRL:PASS" in line)
        swapped = lines[:]
        swapped.insert(0, swapped.pop(ctrl))
        self.assertFails("\n".join(swapped) + "\n", why="came before")

    def test_unplug_must_release_everything(self):
        self.assertFails(GOOD, unplug=True, why="UNBIND")
        self.assertFails(UNPLUG.replace("USBD:BT:WITHDRAW", "USBD:BT:XX"), unplug=True, why="withdrew")
        self.assertFails(UNPLUG.replace("functions=bt", "functions=hid"), unplug=True, why="DETACH")
        self.assertFails(UNPLUG.replace("BT:CTRL:GONE", "BT:CTRL:GON"), unplug=True, why="never saw")

    def test_unplug_order(self):
        reordered = GOOD + "BT:CTRL:GONE x\nUSBD:BT:UNBIND port=0-5\nUSBD:BT:WITHDRAW name=os.lazy.usb.bt\n"
        reordered += "USBD:DETACH port=0-5 slot=1 regions=1 functions=bt (unplugged)\n"
        self.assertFails(reordered, unplug=True, why="came before")


# ---- the HID verdicts (judge_hid.py) ----------------------------------------

import judge_hid as hid  # noqa: E402


def keys(text: str, t: int = 100) -> str:
    """`btd`'s BT:KEY lines for `text`, each followed by what `inputd` made of it."""
    out = []
    for c in text:
        code = hid.usage(c)
        out.append(f"BT:KEY usage={code:#x} down t={t}")
        out.append(f"INPUTD:KEY code={code:#x} sym={ord(c):#x} mods=0x40 down")
        out.append(f"INPUTD:TEXT u+{ord(c):04x}")
        out.append(f"BT:KEY usage={code:#x} up t={t + 1}")
        out.append(f"INPUTD:KEY code={code:#x} sym={ord(c):#x} mods=0x40 up")
        t += 5
    return "\n".join(out) + "\n"


POINTER = (
    "INPUTD:POINTER x=10 y=10 buttons=0x0 wheel=0,0\n"
    "INPUTD:POINTER x=16 y=14 buttons=0x0 wheel=0,0\n"
    "INPUTD:POINTER x=16 y=14 buttons=0x1 wheel=0,0\n"
    "INPUTD:POINTER x=16 y=14 buttons=0x0 wheel=0,0\n"
)

BOOT = "BT:CRED uid=916 caps=0x400\nBT:BOND:LOAD n={n}{file}\nBT:LINK ring=1\nBT:READY ms=9000\n" \
       "BT:CTRL:PASS:hci=9 lmp=9 vendor=0xffff addr=**:**:**:**:71:13\n"


def pairing(tail: str, kind: str) -> str:
    auth, cls, hidline = (1, "keyboard", "KBD") if kind == "kbd" else (0, "pointer", "MOUSE")
    passkey = f"BT:PAIR:PASSKEY value=553610 addr=xx:xx:xx:xx:{tail}\n" if kind == "kbd" else ""
    return (
        f"BT:CONN addr=xx:xx:xx:xx:{tail} handle=0x0040 role=initiator\n"
        f"BT:PAIR:START addr=xx:xx:xx:xx:{tail}\n{passkey}BT:BOND:SAVE bytes=150\n"
        f"BT:ENCRYPT addr=xx:xx:xx:xx:{tail}\nBT:BOND:SAVE bytes=420\n"
        f"BT:PAIR:PASS addr=xx:xx:xx:xx:{tail} auth={auth} class={cls}\n"
        f"BT:HID:{hidline} addr=xx:xx:xx:xx:{tail}" + (f" auth={auth}" if kind == "kbd" else "") + "\n"
    )


PAIR = BOOT.format(n=0, file=" file=missing") + pairing("00:01", "kbd") + pairing("00:02", "mouse") \
    + keys("hello") + POINTER


def reconnecting(tail: str, kind: str) -> str:
    hidline = "KBD" if kind == "kbd" else "MOUSE"
    auth = " auth=1" if kind == "kbd" else ""
    return (
        f"BT:CONN addr=xx:xx:xx:xx:{tail} handle=0x0040 role=acceptor\n"
        f"BT:ENCRYPT addr=xx:xx:xx:xx:{tail}\nBT:HID:{hidline} addr=xx:xx:xx:xx:{tail}{auth}\n"
        f"BT:RECONNECT ms=120 addr=xx:xx:xx:xx:{tail}\n"
    )


REBOOT = BOOT.format(n=2, file="") + reconnecting("00:01", "kbd") + reconnecting("00:02", "mouse") \
    + keys("lazy") + POINTER

REFUSE = BOOT.format(n=0, file=" file=missing") \
    + "BT:REFUSE reason=unbonded addr=xx:xx:xx:xx:00:09\n" * 3

KILL = (
    BOOT.format(n=0, file=" file=missing") + pairing("00:01", "kbd")
    + "BT:KEY usage=0x4 down t=500\nINPUTD:KEY code=0x4 sym=0x61 mods=0x40 down\n"
    + "BT:CRASH exiting with a key held\nINPUTD:KEY code=0x4 sym=0x61 mods=0x40 up\n"
    + BOOT.format(n=1, file="").replace("9000", "21000") + reconnecting("00:01", "kbd") + keys("zq")
)


class HidJudge(unittest.TestCase):
    def assertFails(self, failures, why):
        self.assertNotEqual(failures, [], "the judge passed a bad log")
        self.assertTrue(any(why in f for f in failures), f"{why!r} not in {failures}")

    def pair(self, log):
        return hid.judge_pair(log, "hello")

    def reboot(self, second, first=PAIR):
        return hid.judge_reboot(first, second, "hello", "lazy")

    # -- good logs pass

    def test_good_logs_pass(self):
        self.assertEqual(self.pair(PAIR), [])
        self.assertEqual(self.reboot(REBOOT), [])
        self.assertEqual(hid.judge_refuse(REFUSE), [])
        self.assertEqual(hid.judge_kill(KILL, "zq"), [])

    def test_the_timings_are_reported(self):
        self.assertEqual(
            hid.reboot_timings(REBOOT),
            {"ready_ms": 9000, "reconnect_kbd_ms": 120, "reconnect_mouse_ms": 120},
        )

    # -- common

    def test_driver_errors_fail_every_mode(self):
        for error in ("BT:PANIC boom\n", "BT:CTRL:FAIL x\n", "BT:BOND:SAVE:FAIL step=rename errno=13 attempt=2\n", "BT:SOURCE:FAIL x\n"):
            self.assertFails(self.pair(PAIR + error), "error")
            self.assertFails(hid.judge_refuse(REFUSE + error), "error")

    def test_a_first_failed_save_that_was_retried_is_not_an_error(self):
        retried = PAIR.replace(
            "BT:BOND:SAVE bytes=150",
            "BT:BOND:SAVE:FAIL step=sync-new errno=22 attempt=1\nBT:BOND:SAVE bytes=150",
            1,
        )
        self.assertEqual(self.pair(retried), [])

    def test_a_full_address_fails(self):
        self.assertFails(self.pair(PAIR + "BT:CONN addr=C0:FF:EE:00:00:01\n"), "full Bluetooth address")

    def test_inputd_must_see_exactly_what_btd_sent(self):
        self.assertFails(self.pair(PAIR + "INPUTD:KEY code=0x4 sym=0x61 mods=0x40 down\n"), "inputd saw")
        self.assertFails(self.pair(PAIR.replace("BT:KEY usage=0xb up", "BT:KEY usage=0xc up")), "key edge")
        # Keys inputd never decoded.
        self.assertFails(self.pair(PAIR.replace("INPUTD:KEY code=0x12 sym=0x6f mods=0x40 up\n", "")), "inputd saw")

    # -- pair

    def test_pair_needs_both_devices_in_order(self):
        for gone in ("BT:HID:KBD", "BT:HID:MOUSE", "BT:PAIR:PASS addr=xx:xx:xx:xx:00:01", "BT:ENCRYPT addr=xx:xx:xx:xx:00:02"):
            self.assertFails(self.pair(PAIR.replace(gone, "BT:NOPE")), "missing")

    def test_a_keyboard_needs_an_authenticated_key_and_a_passkey(self):
        self.assertFails(self.pair(PAIR.replace("BT:HID:KBD addr=xx:xx:xx:xx:00:01 auth=1", "BT:HID:KBD addr=xx:xx:xx:xx:00:01 auth=0")), "authenticated")
        self.assertFails(self.pair(PAIR.replace("BT:PAIR:PASSKEY", "BT:PAIR:NOKEY")), "PASSKEY")
        self.assertFails(self.pair(PAIR.replace("auth=1 class=keyboard", "auth=0 class=keyboard")), "PAIR:PASS")

    def test_pair_checks_the_bond_store(self):
        self.assertFails(self.pair(PAIR.replace("n=0 file=missing", "n=2")), "empty bond store")
        self.assertFails(self.pair(PAIR.replace("BT:BOND:SAVE bytes=420", "BT:NOSAVE")), "bond saves")

    def test_pair_checks_the_text_and_the_mouse(self):
        self.assertFails(self.pair(PAIR.replace("BT:KEY usage=0xb down", "BT:KEY usage=0xc down")), "published")
        self.assertFails(self.pair(PAIR.replace("INPUTD:TEXT u+0068", "INPUTD:TEXT u+0058")), "inputd produced")
        self.assertFails(self.pair(PAIR.replace(POINTER, "")), "no pointer state")
        self.assertFails(self.pair(PAIR.replace("buttons=0x1", "buttons=0x0")), "never went down")
        self.assertFails(self.pair(PAIR.replace("x=16 y=14", "x=10 y=10")), "never moved")

    # -- reboot

    def test_reboot_needs_the_bonds_read_first(self):
        self.assertFails(self.reboot(REBOOT.replace("n=2", "n=0 file=missing")), "bond file was lost")
        late = REBOOT.replace("BT:BOND:LOAD n=2\n", "").replace("BT:CTRL:PASS", "BT:BOND:LOAD n=2\nBT:CTRL:PASS", 1)
        self.assertFails(self.reboot(late), "after the host was ready")

    def test_reboot_must_not_pair_again(self):
        for line in ("BT:PAIR:START addr=xx:xx:xx:xx:00:01\n", "BT:PAIR:PASSKEY value=123456 addr=xx:xx:xx:xx:00:01\n",
                     "BT:REFUSE reason=unbonded addr=xx:xx:xx:xx:00:01\n"):
            self.assertFails(self.reboot(REBOOT + line), "boot 2")

    def test_reboot_needs_the_reconnect_and_the_text(self):
        self.assertFails(self.reboot(REBOOT.replace("BT:RECONNECT ms=120 addr=xx:xx:xx:xx:00:01\n", "")), "RECONNECT")
        self.assertFails(self.reboot(REBOOT.replace(keys("lazy"), keys("lazx"))), "published")
        self.assertFails(self.reboot(REBOOT.replace(keys("lazy"), "")), "published")
        self.assertFails(self.reboot(REBOOT.replace("BT:HID:KBD", "BT:NOHID")), "missing")

    def test_reboot_also_requires_the_pairing_boot_to_pass(self):
        self.assertFails(self.reboot(REBOOT, PAIR.replace("BT:HID:KBD", "BT:NOHID")), "boot 1")

    # -- refuse

    def test_refuse_must_refuse_and_type_nothing(self):
        self.assertFails(hid.judge_refuse(REFUSE.replace("reason=unbonded", "reason=other")), "not refused")
        self.assertFails(hid.judge_refuse(REFUSE + "BT:HID:KBD addr=xx:xx:xx:xx:00:09\n"), "got further")
        self.assertFails(hid.judge_refuse(REFUSE + "BT:CONN addr=xx:xx:xx:xx:00:09 handle=0x40 role=acceptor\n"), "got further")
        self.assertFails(hid.judge_refuse(REFUSE + keys("a")), "got further")
        self.assertFails(hid.judge_refuse(REFUSE + "INPUTD:KEY code=0x4 sym=0x61 mods=0x40 down\n"), "no bonded device")
        self.assertFails(hid.judge_refuse(REFUSE.replace("BT:READY ms=9000\n", "")), "never became ready")

    # -- kill

    def test_kill_needs_the_crash_the_release_and_the_return(self):
        self.assertFails(hid.judge_kill(KILL.replace("BT:CRASH", "BT:OK"), "zq"), "never crashed")
        self.assertFails(hid.judge_kill(KILL.replace("INPUTD:KEY code=0x4 sym=0x61 mods=0x40 up\n", ""), "zq"), "never released")
        self.assertFails(hid.judge_kill(KILL.replace("BT:READY ms=21000\n", ""), "zq"), "reattach")
        self.assertFails(hid.judge_kill(KILL.replace(keys("zq"), keys("zx")), "zq"), "published")
        self.assertFails(hid.judge_kill(KILL.replace(keys("zq"), ""), "zq"), "published")

    def test_kill_is_not_satisfied_by_a_release_after_the_restart(self):
        release = "INPUTD:KEY code=0x4 sym=0x61 mods=0x40 up\n"
        late = KILL.replace(release, "", 1) + release
        self.assertFails(hid.judge_kill(late, "zq"), "only after btd restarted")

    def test_kill_fails_a_key_left_held(self):
        held = KILL + "BT:KEY usage=0x4 down t=900\nINPUTD:KEY code=0x4 sym=0x61 mods=0x40 down\n"
        self.assertFails(hid.judge_kill(held, "zq"), "still held")



# ---- the LE verdicts (judge_le.py, phase B4) ---------------------------------

import judge_le as le  # noqa: E402


def le_pairing(tail: str, kind: str) -> str:
    auth, cls, hidline = (1, "keyboard", "KBD") if kind == "kbd" else (0, "pointer", "MOUSE")
    passkey = f"BT:PAIR:PASSKEY value=553610 addr=xx:xx:xx:xx:{tail}\n" if kind == "kbd" else ""
    return (
        f"BT:CONN addr=xx:xx:xx:xx:{tail} handle=0x0040 role=le rpa=0\n"
        f"BT:PAIR:START addr=xx:xx:xx:xx:{tail}\n{passkey}BT:ENCRYPT addr=xx:xx:xx:xx:{tail}\n"
        f"BT:BOND:SAVE bytes=420\n"
        f"BT:PAIR:PASS addr=xx:xx:xx:xx:{tail} auth={auth} class={cls}\n"
        f"BT:HID:{hidline} addr=xx:xx:xx:xx:{tail}" + (f" auth={auth}" if kind == "kbd" else "") + "\n"
    )


def le_reconnecting(tail: str, kind: str, rpa: int = 0) -> str:
    hidline = "KBD" if kind == "kbd" else "MOUSE"
    auth = " auth=1" if kind == "kbd" else ""
    return (
        f"BT:CONN addr=xx:xx:xx:xx:{tail} handle=0x0040 role=le rpa={rpa}\n"
        f"BT:ENCRYPT addr=xx:xx:xx:xx:{tail}\nBT:HID:{hidline} addr=xx:xx:xx:xx:{tail}{auth}\n"
        f"BT:RECONNECT ms=80 addr=xx:xx:xx:xx:{tail}\n"
    )


LE_PAIR = BOOT.format(n=0, file=" file=missing") + le_pairing("01:01", "kbd") + le_pairing("01:02", "mouse") \
    + keys("hello") + POINTER
LE_REBOOT = BOOT.format(n=2, file="") + le_reconnecting("01:01", "kbd") + le_reconnecting("01:02", "mouse") \
    + keys("lazy") + POINTER
LE_RPA = BOOT.format(n=2, file="") + le_reconnecting("01:01", "kbd", 1) + le_reconnecting("01:02", "mouse", 1) \
    + keys("lazy") + POINTER + le_reconnecting("01:01", "kbd", 1) + keys("go")
LE_REFUSE = BOOT.format(n=0, file=" file=missing") + (
    "BT:CONN addr=xx:xx:xx:xx:01:03 handle=0x0040 role=le rpa=0\nBT:PAIR:START addr=xx:xx:xx:xx:01:03\n"
    "BT:ENCRYPT addr=xx:xx:xx:xx:01:03\n"
    "BT:REFUSE reason=keyboard-unauthenticated addr=xx:xx:xx:xx:01:03\n"
)


class LeJudge(unittest.TestCase):
    def assertFails(self, failures, why):
        self.assertNotEqual(failures, [], "the judge passed a bad log")
        self.assertTrue(any(why in f for f in failures), f"{why!r} not in {failures}")

    def reboot(self, second, first=LE_PAIR, rpa=False, text="lazy"):
        return le.judge_reboot(first, second, "hello", text, rpa=rpa)

    def test_good_logs_pass(self):
        self.assertEqual(le.judge_pair(LE_PAIR, "hello"), [])
        self.assertEqual(self.reboot(LE_REBOOT), [])
        self.assertEqual(self.reboot(LE_RPA, rpa=True, text="lazygo"), [])
        self.assertEqual(le.judge_refuse(LE_REFUSE), [])

    def test_the_timings_are_reported(self):
        self.assertEqual(
            le.reboot_timings(LE_REBOOT),
            {"ready_ms": 9000, "reconnect_kbd_ms": 80, "reconnect_mouse_ms": 80},
        )

    # -- pair

    def test_pair_needs_the_le_lines_in_order(self):
        for gone in ("role=le", "BT:PAIR:START addr=xx:xx:xx:xx:01:01", "BT:ENCRYPT addr=xx:xx:xx:xx:01:02",
                     "BT:HID:MOUSE", "BT:PAIR:PASS addr=xx:xx:xx:xx:01:01"):
            self.assertFails(le.judge_pair(LE_PAIR.replace(gone, "BT:NOPE"), "hello"), "")

    def test_the_source_may_not_come_before_the_encryption(self):
        early = LE_PAIR.replace("BT:ENCRYPT addr=xx:xx:xx:xx:01:02\n", "", 1) + "BT:ENCRYPT addr=xx:xx:xx:xx:01:02\n"
        self.assertFails(le.judge_pair(early, "hello"), "too early")

    def test_a_keyboard_needs_an_authenticated_key_and_a_passkey(self):
        weak = LE_PAIR.replace("BT:HID:KBD addr=xx:xx:xx:xx:01:01 auth=1", "BT:HID:KBD addr=xx:xx:xx:xx:01:01 auth=0")
        self.assertFails(le.judge_pair(weak, "hello"), "authenticated")
        self.assertFails(le.judge_pair(LE_PAIR.replace("BT:PAIR:PASSKEY", "BT:PAIR:NOKEY"), "hello"), "PASSKEY")
        self.assertFails(le.judge_pair(LE_PAIR.replace("auth=1 class=keyboard", "auth=0 class=keyboard"), "hello"), "PAIR:PASS")

    def test_pair_checks_the_bond_store_and_the_failures(self):
        self.assertFails(le.judge_pair(LE_PAIR.replace("n=0 file=missing", "n=2"), "hello"), "empty bond store")
        self.assertFails(le.judge_pair(LE_PAIR.replace("BT:BOND:SAVE bytes=420", "BT:NOSAVE"), "hello"), "bond saves")
        for bad in ("BT:GATT:FAIL addr=xx:xx:xx:xx:01:01 reason=timeout\n", "BT:PAIR:FAIL addr=xx:xx:xx:xx:01:01 reason=smp-timeout\n",
                    "BT:ENCRYPT:FAIL addr=xx:xx:xx:xx:01:01 status=0x06\n"):
            self.assertFails(le.judge_pair(LE_PAIR + bad, "hello"), "appears")

    def test_pair_connects_under_the_identity_address(self):
        self.assertFails(le.judge_pair(LE_PAIR.replace("rpa=0", "rpa=1", 1), "hello"), "connect once")

    def test_pair_checks_the_text_and_the_mouse(self):
        self.assertFails(le.judge_pair(LE_PAIR.replace("BT:KEY usage=0xb down", "BT:KEY usage=0xc down"), "hello"), "published")
        self.assertFails(le.judge_pair(LE_PAIR.replace(POINTER, ""), "hello"), "no pointer state")
        self.assertFails(le.judge_pair(LE_PAIR + "BT:CONN addr=C0:FF:EE:00:01:01\n", "hello"), "full Bluetooth address")

    # -- reboot and rpa

    def test_reboot_needs_the_bonds_read_first_and_no_pairing(self):
        self.assertFails(self.reboot(LE_REBOOT.replace("n=2", "n=0 file=missing")), "bond file was lost")
        for line in ("BT:PAIR:START addr=xx:xx:xx:xx:01:01\n", "BT:REFUSE reason=unbonded addr=xx:xx:xx:xx:01:01\n"):
            self.assertFails(self.reboot(LE_REBOOT + line), "boot 2")

    def test_reboot_needs_the_reconnect_the_text_and_the_pairing_boot(self):
        self.assertFails(self.reboot(LE_REBOOT.replace("BT:RECONNECT ms=80 addr=xx:xx:xx:xx:01:01\n", "")), "RECONNECT")
        self.assertFails(self.reboot(LE_REBOOT.replace(keys("lazy"), keys("lazx"))), "published")
        self.assertFails(self.reboot(LE_REBOOT, LE_PAIR.replace("BT:HID:KBD", "BT:NOHID")), "boot 1")

    def test_reboot_connects_under_the_identity_address(self):
        self.assertFails(self.reboot(LE_REBOOT.replace("rpa=0", "rpa=1", 1)), "identity address")

    def test_rpa_needs_every_connection_resolved_and_the_second_one(self):
        self.assertFails(self.reboot(LE_RPA.replace("rpa=1", "rpa=0", 1), rpa=True, text="lazygo"), "resolvable private")
        once = LE_RPA.replace(le_reconnecting("01:01", "kbd", 1) + keys("go"), keys("go"))
        self.assertFails(self.reboot(once, rpa=True, text="lazygo"), "2 connection")
        self.assertFails(self.reboot(LE_RPA.replace(keys("go"), keys("gx")), rpa=True, text="lazygo"), "published")

    # -- refuse

    def test_refuse_must_refuse_once_and_register_nothing(self):
        self.assertFails(le.judge_refuse(LE_REFUSE.replace("keyboard-unauthenticated", "other")), "not refused")
        self.assertFails(le.judge_refuse(LE_REFUSE + "BT:HID:KBD addr=xx:xx:xx:xx:01:03 auth=0\n"), "got further")
        self.assertFails(le.judge_refuse(LE_REFUSE + "BT:PAIR:PASS addr=xx:xx:xx:xx:01:03 auth=0 class=keyboard\n"), "got further")
        self.assertFails(le.judge_refuse(LE_REFUSE + "BT:BOND:SAVE bytes=400\n"), "got further")
        again = le_pairing("01:03", "mouse").split("BT:PAIR:PASS")[0]
        self.assertFails(le.judge_refuse(LE_REFUSE + again), "connections")
        self.assertFails(le.judge_refuse(LE_REFUSE.replace("BT:READY ms=9000\n", "")), "never became ready")

    def test_refuse_fails_a_stranger_that_got_in_and_stray_input(self):
        stranger = "BT:CONN addr=xx:xx:xx:xx:01:09 handle=0x0041 role=le rpa=0\n"
        self.assertFails(le.judge_refuse(LE_REFUSE + stranger), "stranger")
        self.assertFails(le.judge_refuse(LE_REFUSE + "INPUTD:KEY code=0x4 sym=0x61 mods=0x40 down\n"), "inputd saw")


from test_judge_fw import FwJudge  # noqa: E402,F401  (the firmware judge's tests run with these)

if __name__ == "__main__":
    unittest.main()
