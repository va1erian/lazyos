"""The verdict of a simulated Wi-Fi run (`sim_run.py`): what the guest
printed, what the fake access point logged, and what crossed the air (the
pcap). Pure functions, so `test_judge.py` can show each check fails when it
should.

The AP's log and the pcap are the proof; the guest's markers only say what
the guest believes. Each check returns a list of problems (empty: passed).
"""

from __future__ import annotations

import re
import struct
from dataclasses import dataclass

from fakeap import dot11

GUEST_MAC = bytes.fromhex("025753494d00")
OPEN, WPA2, WRONG, HIDDEN, ROAM = "LazyOpen", "LazyWPA2", "LazyWrong", "LazyHidden", "LazyRoam"
FAIL_MARKERS = ("WIFISIM:FAIL", "WIFIPROBE:FAIL", "PANIC", "EXCEPTION", "WIFIPROBE:WRONG:ACCEPTED")


@dataclass
class Run:
    serial: str
    ap: list[str]
    frames: list[bytes]

    def ap_has(self, prefix: str, ssid: str | None = None) -> bool:
        return any(line.startswith(prefix) and (ssid is None or line.endswith(f"ssid={ssid}"))
                   for line in self.ap)

    def ap_count(self, prefix: str, ssid: str | None = None) -> int:
        return sum(1 for line in self.ap
                   if line.startswith(prefix) and (ssid is None or line.endswith(f"ssid={ssid}")))


def _missing(run: Run, serial: tuple[str, ...] = (), ap: tuple[tuple[str, str | None], ...] = ()) -> list[str]:
    problems = [f"guest never printed {marker!r}" for marker in serial if marker not in run.serial]
    problems += [f"the AP never logged {prefix!r}" + (f" for {ssid}" if ssid else "")
                 for prefix, ssid in ap if not run.ap_has(prefix, ssid)]
    return problems


# What the fake AP logs when the station sent something it should not have:
# a frame in the clear after the pairwise key, one it cannot decrypt, a bad
# LLC header, data before the port opened, or an EAPOL frame out of turn.
# Retries can hide these from the DHCP and ping checks, so they are judged
# on their own, over the join phase (before the first injected fault, after
# which a station may legitimately send into a link it has not yet seen
# drop).
AP_COMPLAINTS = ("AP:DATA:PLAINTEXT:DROPPED", "AP:CCMP:NOKEY", "AP:DATA:BADLLC", "AP:DATA:NOT_AUTHORIZED",
                 "AP:EAPOL:STALE", "AP:EAPOL:UNEXPECTED", "AP:EAPOL:MALFORMED")


def ap_complaints(run: Run) -> list[str]:
    """The AP's complaints logged before the first `AP:FAULT:` line."""
    problems = []
    for line in run.ap:
        if line.startswith("AP:FAULT:"):
            break
        if line.startswith(AP_COMPLAINTS):
            problems.append(f"the AP logged {line!r}")
    return problems


def check_boot(run: Run) -> list[str]:
    problems = _missing(run, ("NETDRV:AIR ifname=air0", "WIFISIM:AIR if=air0", "WIFI:READY if=wlan0",
                              "WIFI:HW:ATTACH if=wlan0", "WIFIPROBE:INFO if=wlan0"))
    problems += [f"failure marker {marker!r}" for marker in FAIL_MARKERS if marker in run.serial]
    if "NETD:NIC:FOUND if=air0" in run.serial:
        problems.append("netd attached the air card (it must leave air0 to wifisim)")
    return problems


def _probes_from_guest(frames: list[bytes]) -> list[bytes]:
    return [f for f in frames if len(f) >= 24 and f[0] == dot11.PROBE_REQ << 4 and f[10:16] == GUEST_MAC]


def check_scan(run: Run) -> list[str]:
    problems = _missing(run, ("WIFIPROBE:SCAN:DONE", f"WIFIPROBE:HIDDEN:FOUND ssid={HIDDEN}"),
                        (("AP:PROBE ssid=", None),))
    for ssid in (OPEN, WPA2, ROAM):
        if not re.search(rf"WIFIPROBE:SCAN:BSS \S+ ssid={ssid} ", run.serial):
            problems.append(f"the scan did not list {ssid}")
    roam = set(re.findall(rf"WIFIPROBE:SCAN:BSS bssid=(\S+) ssid={ROAM} ch=(\d+)", run.serial))
    if len({bssid for bssid, _ in roam}) < 2 or len({ch for _, ch in roam}) < 2:
        problems.append(f"the scan did not list both {ROAM} BSSes on their two channels")
    if not any(f"AP:PROBE ssid={HIDDEN}" in line and "wildcard=0" in line for line in run.ap):
        problems.append("no directed probe for the hidden network reached the AP")
    if not _probes_from_guest(run.frames):
        problems.append("no probe request from the station in the capture")
    return problems


def check_wrong_passphrase(run: Run) -> list[str]:
    problems = _missing(run, (f"WIFIPROBE:WRONG:REFUSED ssid={WRONG}",), (("AP:EAPOL:M2:BADMIC", WRONG),))
    if run.ap_has("AP:PTK:INSTALLED", WRONG):
        problems.append(f"the AP installed a key for {WRONG} with the wrong passphrase")
    return problems


def check_open_join(run: Run) -> list[str]:
    problems = _missing(run, (f"WIFIPROBE:CONNECTED ssid={OPEN} security=open", f"WIFIPROBE:LEAVE ssid={OPEN}"),
                        (("AP:DHCP:ACK", OPEN), ("AP:ICMP:ECHO:REPLY", OPEN)))
    if not any(line.startswith(f"AP:AUTHORIZED ssid={OPEN}") for line in run.ap):
        problems.append(f"the AP never authorized the station on {OPEN}")
    if not re.search(r"NETD:ADDR 192\.168\.77\.\d+/24 .*if=wlan0", run.serial):
        problems.append("netd never took a DHCP lease on wlan0")
    return problems


def _eapol_frames(frames: list[bytes]) -> list[tuple[bool, int]]:
    """(from the AP, key info) of every unprotected EAPOL-Key frame."""
    out = []
    for frame in frames:
        h = dot11.parse_header(frame)
        if h is None or h.ftype != dot11.TYPE_DATA or h.protected:
            continue
        msdu = dot11.unsnap(frame[h.length:])
        if msdu and msdu[0] == dot11.ETHERTYPE_EAPOL and len(msdu[1]) >= 7 and msdu[1][1] == 3:
            out.append((bool(h.flags & dot11.FROM_DS), struct.unpack(">H", msdu[1][5:7])[0]))
    return out


def check_wpa2_join(run: Run) -> list[str]:
    problems = _missing(run, (f"WIFIPROBE:CONNECTED ssid={WPA2} security=wpa2", "WIFIPROBE:KEYS:PTK",
                              "WIFIPROBE:KEYS:GTK"),
                        (("AP:EAPOL:M2:OK", WPA2), ("AP:EAPOL:M4:OK", WPA2), ("AP:PTK:INSTALLED", WPA2),
                         ("AP:CCMP:RX:OK", WPA2), ("AP:DHCP:ACK", WPA2), ("AP:ICMP:ECHO:REPLY", WPA2)))
    for prefix in ("AP:CCMP:BADMIC", "AP:CCMP:REPLAY"):
        if run.ap_has(prefix, WPA2):
            problems.append(f"the AP logged {prefix} for {WPA2}: a wrong key or packet number")
    problems += ap_complaints(run)
    eapol = _eapol_frames(run.frames)
    station_msgs = [info for from_ap, info in eapol if not from_ap]
    ap_msgs = [info for from_ap, info in eapol if from_ap]
    if not any(info & 0x0080 and not info & 0x0100 for info in ap_msgs):
        problems.append("no message 1 in the capture")
    if not any(info & 0x1000 for info in ap_msgs):
        problems.append("no message 3 in the capture")
    if not any(info & 0x0100 and not info & 0x0200 for info in station_msgs):
        problems.append("no message 2 in the capture")
    if not any(info & 0x0200 and info & 0x0008 for info in station_msgs):
        problems.append("no message 4 in the capture")
    protected = [f for f in run.frames if len(f) > 24 and (f[0] >> 2) & 3 == 2 and f[1] & dot11.PROTECTED
                 and f[1] & dot11.TO_DS and f[10:16] == GUEST_MAC]
    if not protected:
        problems.append("no protected data frame from the station in the capture")
    return problems


def check_faults(run: Run) -> list[str]:
    return _missing(run, ("WIFIPROBE:EVENT:REKEY", "WIFIPROBE:EVENT:BEACONLOSS", "WIFIPROBE:EVENT:DEAUTH reason=7"),
                    (("AP:REKEY:G2:OK", WPA2), ("AP:FAULT:SILENCE", None), ("AP:FAULT:DEAUTH", None)))


CHECKS = {
    "boot": check_boot,
    "scan": check_scan,
    "wrong passphrase": check_wrong_passphrase,
    "open join, DHCP and ping over wlan0": check_open_join,
    "WPA2 join, CCMP data, DHCP and ping": check_wpa2_join,
    "rekey, beacon loss, deauth": check_faults,
}


def judge(run: Run) -> dict[str, list[str]]:
    return {name: check(run) for name, check in CHECKS.items()}
