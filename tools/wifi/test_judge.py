#!/usr/bin/env python3
"""The simulated Wi-Fi judge (`sim_judge.py`) fails when it should: a good
run built from a real fake-AP exchange passes every check, and each check
fails when its evidence is removed or contradicted.

    python tools/wifi/test_judge.py
"""

from __future__ import annotations

import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import sim_judge  # noqa: E402
from fakeap import ccmp, dot11, wpa  # noqa: E402

GUEST = sim_judge.GUEST_MAC
AP = bytes.fromhex("02a000000002")

SERIAL = """\
NETDRV:AIR ifname=air0
WIFISIM:AIR if=air0 attached
WIFI:READY if=wlan0 hw=os.lazy.net.wifi.hw/wlan0 nic=os.lazy.net.nic/wlan0 mtu=1442 max_mgmt=1484
WIFI:HW:ATTACH if=wlan0 owner=40 uid=913
WIFIPROBE:INFO if=wlan0 mac=02:57:53:49:4d:00 bands=0x3 ciphers=0x1 max_mgmt=1484
WIFIPROBE:SCAN:BSS bssid=02:a0:00:00:00:01 ssid=LazyOpen ch=1 rssi=-40 sec=open age=12 kind=probe
WIFIPROBE:SCAN:BSS bssid=02:a0:00:00:00:02 ssid=LazyWPA2 ch=6 rssi=-45 sec=wpa2 age=8 kind=beacon
WIFIPROBE:SCAN:BSS bssid=02:a0:00:00:00:04 ssid=LazyRoam ch=1 rssi=-70 sec=wpa2 age=20 kind=beacon
WIFIPROBE:SCAN:BSS bssid=02:a0:00:00:00:05 ssid=LazyRoam ch=11 rssi=-50 sec=wpa2 age=3 kind=beacon
WIFIPROBE:SCAN:DONE count=9
WIFIPROBE:HIDDEN:FOUND ssid=LazyHidden ch=11
WIFIPROBE:WRONG:REFUSED ssid=LazyWrong (the 4-way handshake did not finish)
WIFIPROBE:CONNECTED ssid=LazyOpen security=open aid=1
NETD:ADDR 192.168.77.100/24 gw=192.168.77.1 dns=192.168.77.1 source=dhcp if=wlan0
WIFIPROBE:LEAVE ssid=LazyOpen
WIFIPROBE:KEYS:PTK
WIFIPROBE:KEYS:GTK index=1
WIFIPROBE:CONNECTED ssid=LazyWPA2 security=wpa2 aid=1
WIFIPROBE:EVENT:REKEY
WIFIPROBE:EVENT:BEACONLOSS
WIFIPROBE:EVENT:DEAUTH reason=7
"""

AP_LOG = [
    "AP:START bsses=6", "AP:PROBE ssid=LazyOpen ch=1 wildcard=1", "AP:PROBE ssid=LazyHidden ch=11 wildcard=0",
    "AP:EAPOL:M1 ssid=LazyWrong", "AP:EAPOL:M2:BADMIC ssid=LazyWrong",
    "AP:AUTHORIZED ssid=LazyOpen sta=02:57:53:49:4d:00 security=open",
    "AP:DHCP:ACK ip=192.168.77.100 ssid=LazyOpen", "AP:ICMP:ECHO:REPLY from=192.168.77.100 seq=1 ssid=LazyOpen",
    "AP:EAPOL:M1 ssid=LazyWPA2", "AP:EAPOL:M2:OK ssid=LazyWPA2", "AP:EAPOL:M3 ssid=LazyWPA2",
    "AP:EAPOL:M4:OK ssid=LazyWPA2", "AP:PTK:INSTALLED ssid=LazyWPA2", "AP:CCMP:RX:OK ssid=LazyWPA2",
    "AP:DHCP:ACK ip=192.168.77.100 ssid=LazyWPA2", "AP:ICMP:ECHO:REPLY from=192.168.77.100 seq=1 ssid=LazyWPA2",
    "AP:FAULT:REKEY ssid=LazyWPA2 index=2", "AP:REKEY:G2:OK index=2 ssid=LazyWPA2",
    "AP:FAULT:SILENCE seconds=3.0", "AP:FAULT:DEAUTH reason=7",
]


def eapol(from_ap: bool, info: int) -> bytes:
    flags = dot11.FROM_DS if from_ap else dot11.TO_DS
    a1, a2 = (GUEST, AP) if from_ap else (AP, GUEST)
    header = bytes([0x88, flags]) + b"\0\0" + a1 + a2 + AP + struct.pack("<HH", 0x10, 7)
    return header + dot11.snap(dot11.ETHERTYPE_EAPOL, wpa.build(info, 16, 1))


def frames() -> list[bytes]:
    probe = dot11.mgmt(dot11.PROBE_REQ, dot11.BROADCAST, GUEST, dot11.BROADCAST, 1, dot11.ie(0, b""))
    protected = bytes([0x88, dot11.TO_DS | dot11.PROTECTED]) + b"\0\0" + AP + GUEST + AP + bytes(4)
    protected += ccmp.ccmp_header(1, 0) + bytes(30)
    return [probe, eapol(True, wpa.MSG1), eapol(False, 0x010A), eapol(True, wpa.MSG3), eapol(False, 0x030A),
            protected]


def good() -> sim_judge.Run:
    return sim_judge.Run(SERIAL, list(AP_LOG), frames())


class JudgeTest(unittest.TestCase):
    def assert_fails(self, check: str, run: sim_judge.Run, needle: str = "") -> None:
        problems = sim_judge.judge(run)[check]
        self.assertTrue(problems, f"{check} passed a broken run")
        if needle:
            self.assertTrue(any(needle in p for p in problems), problems)

    def test_a_good_run_passes(self):
        for name, problems in sim_judge.judge(good()).items():
            self.assertEqual(problems, [], name)

    def test_boot(self):
        run = good()
        run.serial = run.serial.replace("NETDRV:AIR ifname=air0\n", "")
        self.assert_fails("boot", run, "NETDRV:AIR")
        run = good()
        run.serial += "NETD:NIC:FOUND if=air0\n"
        self.assert_fails("boot", run, "air card")
        run = good()
        run.serial += "WIFISIM:FAIL engine\n"
        self.assert_fails("boot", run, "failure marker")

    def test_scan(self):
        run = good()
        run.serial = run.serial.replace("ssid=LazyRoam ch=11", "ssid=LazyRoam ch=1")
        self.assert_fails("scan", run, "both LazyRoam")
        run = good()
        run.ap = [line for line in run.ap if "wildcard=0" not in line]
        self.assert_fails("scan", run, "hidden")
        run = good()
        run.frames = run.frames[1:]
        self.assert_fails("scan", run, "probe request")

    def test_wrong_passphrase(self):
        run = good()
        run.ap.append("AP:PTK:INSTALLED ssid=LazyWrong")
        self.assert_fails("wrong passphrase", run, "installed a key")
        run = good()
        run.ap.remove("AP:EAPOL:M2:BADMIC ssid=LazyWrong")
        self.assert_fails("wrong passphrase", run)

    def test_open_join(self):
        run = good()
        run.serial = run.serial.replace("if=wlan0\nWIFIPROBE:LEAVE", "if=eth0\nWIFIPROBE:LEAVE")
        self.assert_fails("open join, DHCP and ping over wlan0", run, "lease on wlan0")
        run = good()
        run.ap = [line for line in run.ap if not line.startswith("AP:ICMP:ECHO:REPLY") or "LazyWPA2" in line]
        self.assert_fails("open join, DHCP and ping over wlan0", run, "ECHO:REPLY")

    def test_wpa2_join(self):
        name = "WPA2 join, CCMP data, DHCP and ping"
        run = good()
        run.ap.append("AP:CCMP:REPLAY pn=4 ssid=LazyWPA2")
        self.assert_fails(name, run, "packet number")
        run = good()
        run.frames = [f for f in run.frames if not f[1] & dot11.PROTECTED]
        self.assert_fails(name, run, "protected data")
        for index, missing in ((2, "message 2"), (4, "message 4"), (3, "message 3")):
            run = good()
            del run.frames[index]
            self.assert_fails(name, run, missing)
        run = good()
        run.ap.remove("AP:CCMP:RX:OK ssid=LazyWPA2")
        self.assert_fails(name, run, "CCMP:RX:OK")

    def test_faults(self):
        for marker in ("WIFIPROBE:EVENT:REKEY", "WIFIPROBE:EVENT:BEACONLOSS", "WIFIPROBE:EVENT:DEAUTH reason=7"):
            run = good()
            run.serial = run.serial.replace(marker, "")
            self.assert_fails("rekey, beacon loss, deauth", run, marker)
        run = good()
        run.ap.remove("AP:REKEY:G2:OK index=2 ssid=LazyWPA2")
        self.assert_fails("rekey, beacon loss, deauth", run, "REKEY:G2:OK")

    def test_ap_complaints_fail_the_join_but_not_after_a_fault(self):
        name = "WPA2 join, CCMP data, DHCP and ping"
        run = good()
        run.ap.insert(1, "AP:DATA:PLAINTEXT:DROPPED")
        self.assert_fails(name, run, "PLAINTEXT")
        run = good()
        run.ap.insert(1, "AP:CCMP:NOKEY")
        self.assert_fails(name, run, "NOKEY")
        run = good()
        run.ap.append("AP:DATA:NOT_AUTHORIZED")
        self.assertEqual(sim_judge.ap_complaints(run), [])


if __name__ == "__main__":
    unittest.main()
