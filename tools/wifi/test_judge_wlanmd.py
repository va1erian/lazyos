#!/usr/bin/env python3
"""The station-manager judge (`sim_judge_wlanmd.py`) fails when it should: a
good run passes every check, and each check fails when its evidence is
removed or contradicted. The air capture and the AP's log are the probe
judge's (`test_judge.py`); only the guest's lines differ.

    python tools/wifi/test_judge_wlanmd.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import sim_judge  # noqa: E402
import sim_judge_wlanmd as judge  # noqa: E402
from test_judge import AP_LOG, frames  # noqa: E402

SERIAL = """\
NETDRV:AIR ifname=air0
WIFI:READY if=wlan0 hw=os.lazy.net.wifi.hw/wlan0 nic=os.lazy.net.nic/wlan0 mtu=1442 max_mgmt=1484
WIFI:HW:ATTACH if=wlan0 owner=21 uid=912
WLANMD:RADIO:UP wlan0 mac=02:57:53:49:4d:00 country=00
LOGIN:OK:PASS user=user uid=1000
WIFICTL:NET "LazyOpen" open rssi=-40 freq=2412 bss=1
WIFICTL:NET "LazyWPA2" wpa2-psk rssi=-45 freq=2437 bss=1
WIFICTL:NET "LazyRoam" wpa2-psk rssi=-50 freq=2462 bss=2
wlan0: 5 network(s)
WLANMD:STATE wlan0 failed reason=bad-passphrase ssid="LazyWrong"
WLANMD:STATE wlan0 connected reason=none ssid="LazyOpen"
NETD:ADDR 192.168.77.100/24 gw=192.168.77.1 dns=192.168.77.1 source=dhcp if=wlan0
WLANMD:STATE wlan0 connected reason=none ssid="LazyWPA2"
WLANMD:STATE wlan0 failed reason=beacon-loss ssid="LazyWPA2"
WLANMD:STATE wlan0 connected reason=none ssid="LazyWPA2"
WLANMD:STATE wlan0 failed reason=deauthenticated ssid="LazyWPA2"
WLANMD:STATE wlan0 connected reason=none ssid="LazyWPA2"
"""

REJOINS = ["AP:AUTHORIZED ssid=LazyWPA2 sta=02:57:53:49:4d:00 security=wpa2"] * 3


def good() -> sim_judge.Run:
    return sim_judge.Run(SERIAL, list(AP_LOG) + REJOINS, frames())


class WlanmdJudgeTest(unittest.TestCase):
    def assert_fails(self, check: str, run: sim_judge.Run, needle: str = "") -> None:
        problems = judge.judge(run)[check]
        self.assertTrue(problems, f"{check} passed a broken run")
        if needle:
            self.assertTrue(any(needle in p for p in problems), problems)

    def test_a_good_run_passes(self):
        for name, problems in judge.judge(good()).items():
            self.assertEqual(problems, [], name)

    def test_boot_needs_wlanmd_as_the_owner(self):
        name = "boot (wlanmd owns the radio)"
        run = good()
        run.serial = run.serial.replace("uid=912", "uid=913")
        self.assert_fails(name, run, "owner is not wlanmd")
        run = good()
        run.serial = run.serial.replace("LOGIN:OK:PASS user=user uid=1000\n", "")
        self.assert_fails(name, run, "LOGIN:OK")
        run = good()
        run.serial += "WIFISIM:FAIL engine\n"
        self.assert_fails(name, run, "failure marker")

    def test_scan(self):
        name = "scan (wifictl scan)"
        run = good()
        run.serial = run.serial.replace('"LazyRoam" wpa2-psk rssi=-50 freq=2462 bss=2', '"LazyRoam" wpa2-psk bss=1')
        self.assert_fails(name, run, "both LazyRoam")
        run = good()
        run.serial = run.serial.replace('WIFICTL:NET "LazyOpen"', "")
        self.assert_fails(name, run, "LazyOpen")
        run = good()
        run.frames = run.frames[1:]
        self.assert_fails(name, run, "probe request")

    def test_wrong_passphrase(self):
        run = good()
        run.ap.append("AP:PTK:INSTALLED ssid=LazyWrong")
        self.assert_fails("wrong passphrase", run, "installed a key")
        run = good()
        run.serial = run.serial.replace("reason=bad-passphrase", "reason=timeout")
        self.assert_fails("wrong passphrase", run, "bad-passphrase")

    def test_open_join(self):
        name = "open join, DHCP and ping over wlan0"
        run = good()
        run.serial = run.serial.replace("source=dhcp if=wlan0", "source=dhcp if=eth0")
        self.assert_fails(name, run, "lease on wlan0")
        run = good()
        run.serial = run.serial.replace('connected reason=none ssid="LazyOpen"', "")
        self.assert_fails(name, run, "LazyOpen")

    def test_wpa2_join(self):
        name = "WPA2 join, CCMP data, DHCP and ping"
        run = good()
        run.ap.append("AP:CCMP:BADMIC pn=4 ssid=LazyWPA2")
        self.assert_fails(name, run, "wrong key")
        for index, missing in ((2, "message 2"), (4, "message 4")):
            run = good()
            del run.frames[index]
            self.assert_fails(name, run, missing)

    def test_faults_need_two_rejoins(self):
        name = "rekey kept, beacon loss and deauth rejoined"
        run = good()
        run.ap = run.ap[:-1]
        self.assert_fails(name, run, "want 3")
        run = good()
        run.serial = run.serial.replace("reason=beacon-loss", "reason=timeout")
        self.assert_fails(name, run, "beacon-loss")
        run = good()
        run.ap.remove("AP:REKEY:G2:OK index=2 ssid=LazyWPA2")
        self.assert_fails(name, run, "REKEY")


if __name__ == "__main__":
    unittest.main()
