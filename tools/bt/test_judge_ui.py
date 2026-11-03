#!/usr/bin/env python3
"""The pairing-interface judges must fail when they should
(`python tools/bt/test_judge_ui.py`)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge_hid as hid  # noqa: E402
import judge_ui as ui  # noqa: E402

TEXT = "pairedui"
KBD = "xx:xx:xx:xx:01:01"
PASSKEY = "042817"


def keys(text: str) -> str:
    out = ""
    for char in text:
        code = hid.usage(char)
        out += (f"BT:KEY usage={code:#x} down t=1\nINPUTD:KEY code={code:#x} sym=0x61 mods=0x0 down\n"
                f"INPUTD:TEXT u+{ord(char):x}\n"
                f"BT:KEY usage={code:#x} up t=2\nINPUTD:KEY code={code:#x} sym=0x61 mods=0x0 up\n")
    return out


UI_GOOD = f"""BT:CRED uid=916 caps=0x400
BT:API:SERVE name=os.lazy.bt
BT:READY ms=4100
SETTINGS:UP:PASS
SETTINGS:MSG:Section(9)
SETTINGS:MSG:Bt(Event(Snapshot {{ controller: true, discovering: false, devices: [], pairing: BtPairing {{ phase: Idle, address: "", reason: None, .. }} }}))
SETTINGS:MSG:Bt(Scan)
SETTINGS:MSG:Bt(Event(Device {{ device: BtDevice {{ address: "{KBD}", name: "Sim Keyboard", .. }}, removed: false }}))
SETTINGS:MSG:Bt(Select(0))
SETTINGS:MSG:Bt(Pair)
BT:API:PAIR addr={KBD}
SETTINGS:MSG:Bt(Event(Pairing(BtPairing {{ phase: Connecting, address: "{KBD}", reason: None, .. }})))
BT:CONN addr={KBD} handle=0x0040 role=le rpa=0
BT:PAIR:START addr={KBD}
BT:PAIR:PASSKEY value={PASSKEY} addr={KBD}
SETTINGS:MSG:Bt(Event(Pairing(BtPairing {{ phase: ShowPasskey, address: "{KBD}", reason: None, .. }})))
BT:ENCRYPT addr={KBD}
BT:PAIR:PASS addr={KBD} auth=1 class=keyboard
SETTINGS:MSG:Bt(Event(Pairing(BtPairing {{ phase: Paired, address: "{KBD}", reason: None, .. }})))
BT:HID:KBD addr={KBD} auth=1
""" + keys(TEXT)

FULL = "C0:FF:EE:00:01:01"
BTCTL_GOOD = f"""BT:API:SERVE name=os.lazy.bt
BT:READY ms=4100
TERM:UP:PASS
TERM:OUT:BTCTL:LIST1:0:|
TERM:OUT:BTCTL:SCAN:0:{FULL}  found  le keyboard -50dBm  Sim Keyboard|scan: done|
BT:API:PAIR addr={KBD}
BT:CONN addr={KBD} handle=0x0040 role=le rpa=0
BT:PAIR:START addr={KBD}
BT:PAIR:PASSKEY value={PASSKEY} addr={KBD}
BT:ENCRYPT addr={KBD}
BT:PAIR:PASS addr={KBD} auth=1 class=keyboard
BT:HID:KBD addr={KBD} auth=1
TERM:OUT:BTCTL:PAIR:0:passkey: {PASSKEY}|paired: {FULL}|
""" + keys(TEXT) + f"""TERM:OUT:BTCTL:LIST2:0:{FULL}  connected  le keyboard  Sim Keyboard|
API:FORGET addr={KBD}
BT:BOND:FORGET addr={KBD}
TERM:OUT:BTCTL:FORGET:0:forgotten: {FULL}|
TERM:OUT:BTCTL:LIST3:0:{FULL}  found  le keyboard -50dBm  Sim Keyboard|
"""


class UiJudge(unittest.TestCase):
    def assertFails(self, failures, why):
        self.assertNotEqual(failures, [], "the judge passed a bad log")
        self.assertTrue(any(why in f for f in failures), f"{why!r} not in {failures}")

    def test_good_logs_pass(self):
        self.assertEqual(ui.judge_ui_pair(UI_GOOD, TEXT), [])
        self.assertEqual(ui.judge_btctl(BTCTL_GOOD, TEXT), [])

    # -- ui-pair

    def test_every_step_of_the_ui_story_is_needed(self):
        for gone in ("SETTINGS:MSG:Section(9)", "SETTINGS:MSG:Bt(Scan)", "SETTINGS:MSG:Bt(Pair)",
                     "phase: ShowPasskey", "phase: Paired", "BT:HID:KBD", "BT:PAIR:PASSKEY"):
            self.assertNotEqual(ui.judge_ui_pair(UI_GOOD.replace(gone, "NOPE"), TEXT), [], gone)

    def test_a_pairing_before_the_call_is_silent(self):
        early = f"BT:CONN addr={KBD} handle=0x0041 role=le rpa=0\n" + UI_GOOD
        self.assertFails(ui.judge_ui_pair(early, TEXT), "silent pairing")
        self.assertFails(ui.judge_ui_pair(UI_GOOD.replace("BT:API:PAIR", "BT:API:NOPE"), TEXT), "never got a Pair")

    def test_a_denied_call_or_permission_fails(self):
        self.assertFails(ui.judge_ui_pair(UI_GOOD + "BT:API:DENY method=5 uid=1000\n", TEXT), "denied")
        deny = "LABEL:DENY app=os.lazy.settings interface=os.lazy.bt.v1 method=1\n"
        self.assertFails(ui.judge_ui_pair(UI_GOOD + deny, TEXT), "denied")

    def test_the_passkey_stays_in_its_own_line(self):
        leaked = UI_GOOD + f'SETTINGS:MSG:Bt(Event(Pairing(BtPairing {{ passkey: {PASSKEY} }})))\n'
        self.assertFails(ui.judge_ui_pair(leaked, TEXT), "passkey is in a line")
        self.assertFails(ui.judge_ui_pair(UI_GOOD + f"BT:API:PAIR passkey={PASSKEY}\n", TEXT), "passkey is in a line")

    def test_the_keyboard_needs_an_authenticated_key(self):
        weak = UI_GOOD.replace("auth=1 class=keyboard", "auth=0 class=keyboard")
        self.assertNotEqual(ui.judge_ui_pair(weak, TEXT), [])

    def test_the_text_must_arrive(self):
        self.assertFails(ui.judge_ui_pair(UI_GOOD.replace(keys(TEXT), keys("pairedux")), TEXT), "published")
        self.assertFails(ui.judge_ui_pair(UI_GOOD + "BT:REFUSE reason=unbonded addr=xx:xx:xx:xx:01:09\n", TEXT), "BT:REFUSE")
        self.assertFails(ui.judge_ui_pair(UI_GOOD + f"BT:CONN addr={FULL}\n", TEXT), "full Bluetooth address")

    # -- btctl

    def test_every_btctl_command_must_report(self):
        for what in ("LIST1", "SCAN", "PAIR", "LIST2", "FORGET", "LIST3"):
            bad = "\n".join(l for l in BTCTL_GOOD.splitlines() if f"BTCTL:{what}:" not in l) + "\n"
            self.assertFails(ui.judge_btctl(bad, TEXT), what)

    def test_btctl_exit_statuses(self):
        self.assertFails(ui.judge_btctl(BTCTL_GOOD.replace("BTCTL:PAIR:0:", "BTCTL:PAIR:1:"), TEXT), "exited 1")

    def test_scan_and_pair_must_say_the_truth(self):
        self.assertFails(ui.judge_btctl(BTCTL_GOOD.replace("found  le keyboard -50dBm  Sim Keyboard|scan", "|scan"), TEXT), "did not find")
        self.assertFails(ui.judge_btctl(BTCTL_GOOD.replace(f"passkey: {PASSKEY}", "passkey: 000000"), TEXT), "passkey")
        self.assertFails(ui.judge_btctl(BTCTL_GOOD.replace(f"paired: {FULL}", "failed: Timeout"), TEXT), "did not end paired")

    def test_list_and_forget_must_follow_the_state(self):
        self.assertFails(ui.judge_btctl(BTCTL_GOOD.replace(f"{FULL}  connected  le keyboard  Sim Keyboard", "|"), TEXT), "not in the list")
        late = BTCTL_GOOD.replace(f"LIST3:0:{FULL}  found", f"LIST3:0:{FULL}  paired")
        self.assertFails(ui.judge_btctl(late, TEXT), "still paired")
        self.assertFails(ui.judge_btctl(BTCTL_GOOD.replace("forgotten", "nothing"), TEXT), "forget printed")
        self.assertFails(ui.judge_btctl(BTCTL_GOOD.replace("BT:BOND:FORGET", "BT:NOFORGET"), TEXT), "bond forgotten")

    def test_btctl_is_judged_by_the_typing_too(self):
        self.assertFails(ui.judge_btctl(BTCTL_GOOD.replace(keys(TEXT), ""), TEXT), "published")
        self.assertFails(ui.judge_btctl(BTCTL_GOOD.replace("BT:HID:KBD", "BT:NOHID"), TEXT), "never became a source")


if __name__ == "__main__":
    unittest.main()
