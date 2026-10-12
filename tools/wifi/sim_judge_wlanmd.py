"""The verdict of a simulated Wi-Fi run driven by the real station manager
(`sim_run.py --station wlanmd`): `wlanmd` attached to `wifisim`'s radio,
commanded through `wifictl` on the console as the session user.

As in `sim_judge.py`, the AP's log and the pcap are the proof; the guest's
`WLANMD:STATE`/`WIFICTL:` lines only say what the station believes. Each
check returns a list of problems (empty: passed). `test_judge_wlanmd.py`
shows each one fails when it should.
"""

from __future__ import annotations

import re

from sim_judge import (FAIL_MARKERS, OPEN, ROAM, WPA2, WRONG, Run, _eapol_frames, _missing, _probes_from_guest,
                       ap_complaints)

WLANMD_UID = 912


def state(ifname: str, name: str, ssid: str, reason: str = "none") -> str:
    """The `WLANMD:STATE` line `wlanmd` prints on a transition."""
    return f'WLANMD:STATE {ifname} {name} reason={reason} ssid="{ssid}"'


def check_boot(run: Run) -> list[str]:
    problems = _missing(run, ("NETDRV:AIR ifname=air0", "WIFI:READY if=wlan0", "WLANMD:RADIO:UP wlan0",
                              "LOGIN:OK:PASS user=user"))
    if not re.search(rf"WIFI:HW:ATTACH if=wlan0 owner=\d+ uid={WLANMD_UID}\b", run.serial):
        problems.append("the radio's owner is not wlanmd (uid 912)")
    problems += [f"failure marker {marker!r}" for marker in FAIL_MARKERS if marker in run.serial]
    if "NETD:NIC:FOUND if=air0" in run.serial:
        problems.append("netd attached the air card (it must leave air0 to wifisim)")
    return problems


def check_scan(run: Run) -> list[str]:
    problems = _missing(run, (), (("AP:PROBE ssid=", None),))
    for ssid in (OPEN, WPA2):
        if f'WIFICTL:NET "{ssid}" ' not in run.serial:
            problems.append(f"wifictl scan did not list {ssid}")
    if not re.search(rf'WIFICTL:NET "{ROAM}" \S+ .*bss=2', run.serial):
        problems.append(f"wifictl scan did not merge both {ROAM} BSSes into one network")
    if not _probes_from_guest(run.frames):
        problems.append("no probe request from the station in the capture")
    return problems


def check_wrong_passphrase(run: Run) -> list[str]:
    problems = _missing(run, (state("wlan0", "failed", WRONG, "bad-passphrase"),),
                        (("AP:EAPOL:M2:BADMIC", WRONG),))
    if run.ap_has("AP:PTK:INSTALLED", WRONG):
        problems.append(f"the AP installed a key for {WRONG} with the wrong passphrase")
    return problems


def check_open_join(run: Run) -> list[str]:
    problems = _missing(run, (state("wlan0", "connected", OPEN),),
                        (("AP:DHCP:ACK", OPEN), ("AP:ICMP:ECHO:REPLY", OPEN)))
    if not any(line.startswith(f"AP:AUTHORIZED ssid={OPEN}") for line in run.ap):
        problems.append(f"the AP never authorized the station on {OPEN}")
    if not re.search(r"NETD:ADDR 192\.168\.77\.\d+/24 .*if=wlan0", run.serial):
        problems.append("netd never took a DHCP lease on wlan0")
    return problems


def check_wpa2_join(run: Run) -> list[str]:
    problems = _missing(run, (state("wlan0", "connected", WPA2),),
                        (("AP:EAPOL:M2:OK", WPA2), ("AP:EAPOL:M4:OK", WPA2), ("AP:PTK:INSTALLED", WPA2),
                         ("AP:CCMP:RX:OK", WPA2), ("AP:DHCP:ACK", WPA2), ("AP:ICMP:ECHO:REPLY", WPA2)))
    for prefix in ("AP:CCMP:BADMIC", "AP:CCMP:REPLAY"):
        if run.ap_has(prefix, WPA2):
            problems.append(f"the AP logged {prefix} for {WPA2}: a wrong key or packet number")
    problems += ap_complaints(run)
    station = [info for from_ap, info in _eapol_frames(run.frames) if not from_ap]
    if not any(info & 0x0100 and not info & 0x0200 for info in station):
        problems.append("no message 2 in the capture")
    if not any(info & 0x0200 and info & 0x0008 for info in station):
        problems.append("no message 4 in the capture")
    return problems


def _authorized(run: Run, ssid: str) -> int:
    return sum(1 for line in run.ap if line.startswith(f"AP:AUTHORIZED ssid={ssid}"))


def check_faults(run: Run) -> list[str]:
    """A group rekey keeps the link; beacon loss and a deauthentication each
    end it, and `wlanmd` joins again by itself (the AP authorizes the station
    three times in all)."""
    problems = _missing(run, (state("wlan0", "failed", WPA2, "beacon-loss"),
                              state("wlan0", "failed", WPA2, "deauthenticated")),
                        (("AP:REKEY:G2:OK", WPA2), ("AP:FAULT:SILENCE", None), ("AP:FAULT:DEAUTH", None)))
    if _authorized(run, WPA2) < 3:
        problems.append(f"wlanmd did not join {WPA2} again after each fault "
                        f"(the AP authorized it {_authorized(run, WPA2)} time(s), want 3)")
    return problems


CHECKS = {
    "boot (wlanmd owns the radio)": check_boot,
    "scan (wifictl scan)": check_scan,
    "wrong passphrase": check_wrong_passphrase,
    "open join, DHCP and ping over wlan0": check_open_join,
    "WPA2 join, CCMP data, DHCP and ping": check_wpa2_join,
    "rekey kept, beacon loss and deauth rejoined": check_faults,
}


def judge(run: Run) -> dict[str, list[str]]:
    return {name: check(run) for name, check in CHECKS.items()}
