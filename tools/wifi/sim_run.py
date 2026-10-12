#!/usr/bin/env python3
"""The station stack on the simulated radio, end to end
(docs/wifi-prerequisites-plan.md WP4, docs/wifi-rtw88-plan.md R2).

Builds a console image with the network stack and the simulated radio
(`LAZYOS_SERVICES=1 LAZYOS_NETD=1 LAZYOS_WIFI_SIM=1`), starts the fake access
points (`fake_ap.py`) on a UDP port, boots QEMU with the guest's air card
(MAC 02:57:53:49:4d:00) connected to them and no other card, and judges
(`sim_judge.py`) from the AP's log, the air's pcap and the serial log:

1. boot: `netdrv` serves the air card as `air0`, `netd` leaves it alone,
   `wifisim` serves `wlan0`, a client attaches;
2. scan: the wildcard scan lists the open, WPA2 and two-BSS networks, a
   directed probe finds the hidden one;
3. a WPA2 join with the wrong passphrase is refused by the AP (message 2);
4. the open join: `netd` takes a DHCP lease on `wlan0`, the AP's pings are
   answered;
5. the WPA2 join: messages 1-4 in the capture, CCMP data both ways (the AP
   decrypts what the station sent with its own, independent CCMP), DHCP and
   pings over the protected link;
6. faults: a group rekey, beacon loss (the AP goes silent), a
   deauthentication, each reported by the radio.

**Station.** Without `wlanmd` (this tree), the radio is driven by
`wifiprobe suite` (`LAZYOS_WIFI_SIM_ARGS=probe=suite:...`), a test client of
`os.lazy.net.wifi.hw.v1`. With `--station wlanmd` (needs `wlanmd` and
`wifictl` in the tree, `LAZYOS_WIFI=1`) the image carries no probe and the
harness types `wifictl` commands on the console instead; the same AP-side
checks judge it, and the probe-only markers are skipped.

    python tools/wifi/sim_run.py                 # build, boot, run, judge
    python tools/wifi/sim_run.py --no-build      # reuse target/lazyos.img
    python tools/wifi/sim_run.py --accel none    # TCG
    python tools/wifi/test_judge.py              # the judge fails when it should

Output: `shots/wifi_sim/` (serial.log, ap.log, air.pcap).
"""

from __future__ import annotations

import argparse
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools/screenshot"))
sys.path.insert(0, str(ROOT / "tools/net"))

import fake_ap  # noqa: E402
import sim_judge  # noqa: E402
import sim_judge_wlanmd  # noqa: E402
from fakeap import air  # noqa: E402
from harness_io import stop_qemu  # noqa: E402
from qemu_qmp import DEFAULT_MEMORY, Qmp, accel_args, build_qemu_command, find_qemu, free_port  # noqa: E402

IMAGE = ROOT / "target" / "lazyos.img"
PROBE = "probe=suite:{open}:{wpa2}:{passphrase}:{wrong}:{hidden}".format(
    open=sim_judge.OPEN, wpa2=sim_judge.WPA2, passphrase=fake_ap.WPA2_PASSPHRASE,
    wrong=sim_judge.WRONG, hidden=sim_judge.HIDDEN)
#: The station manager is in the tree when its program is.
WLANMD_PRESENT = (ROOT / "user/src/bin/wlanmd.rs").is_file()


def build(station: str) -> bool:
    env = dict(os.environ, LAZYOS_SERVICES="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_WIFI_SIM="1", LAZYOS_RESET_OS="1")
    # A Wi-Fi switch exported in the shell would put `wlanmd` (and `wifid`)
    # beside the probe, which then competes for the radio.
    for key in ("LAZYOS_DESKTOP", "LAZYOS_NET_ARGS", "LAZYOS_WIFI_SIM_ARGS", "LAZYOS_WIFI"):
        env.pop(key, None)
    if station == "probe":
        env["LAZYOS_WIFI_SIM_ARGS"] = PROBE
    else:
        # `LAZYOS_WIFI=1` also embeds `wifid` and its firmware, which the
        # build refuses until `tools/firmware/fetch.py --switch wifi` ran.
        env["LAZYOS_WIFI"] = "1"
    print(f"building: LAZYOS_SERVICES=1 LAZYOS_NETD=1 LAZYOS_WIFI_SIM=1 station={station} cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env)
    return result.returncode == 0 and IMAGE.is_file()


def free_udp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


class Watch:
    """The serial log and the AP's log, read as they grow."""

    def __init__(self, serial: Path, ap_log: Path, proc: subprocess.Popen):
        self.serial, self.ap_log, self.proc = serial, ap_log, proc
        self.printed = 0

    def text(self) -> str:
        return self.serial.read_text(errors="replace") if self.serial.is_file() else ""

    def ap(self) -> list[str]:
        return self.ap_log.read_text(errors="replace").splitlines() if self.ap_log.is_file() else []

    def echo(self) -> None:
        lines = self.text().splitlines()
        for line in lines[self.printed:]:
            if line.startswith(("WIFI", "NETD:ADDR", "NETD:NIC", "NETDRV:AIR", "NETD:LINK")) or "PANIC" in line:
                print(f"  {line}", flush=True)
        self.printed = len(lines)

    def until(self, done, timeout: float) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            self.echo()
            if done(self.text(), self.ap()):
                return True
            if any(marker in self.text() for marker in sim_judge.FAIL_MARKERS) or self.proc.poll() is not None:
                return False
            time.sleep(0.25)
        return False


def fault(port: int, command: str) -> None:
    print(f"fault: {command}", flush=True)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.sendto(command.encode(), ("127.0.0.1", port))


def drive_probe(watch: Watch, control: int, scale: float) -> None:
    """The probe runs the joins itself; the harness injects the faults once
    the WPA2 link carries traffic."""
    wpa2_up = (lambda serial, ap: f"WIFIPROBE:CONNECTED ssid={sim_judge.WPA2}" in serial
               and any(line.startswith("AP:ICMP:ECHO:REPLY") and line.endswith(sim_judge.WPA2) for line in ap))
    if not watch.until(wpa2_up, 240 * scale):
        return
    fault(control, "rekey")
    watch.until(lambda serial, ap: "WIFIPROBE:EVENT:REKEY" in serial, 20 * scale)
    fault(control, "silence 3")
    watch.until(lambda serial, ap: "WIFIPROBE:EVENT:BEACONLOSS" in serial, 20 * scale)
    time.sleep(4)
    fault(control, "deauth 7")
    watch.until(lambda serial, ap: "WIFIPROBE:EVENT:DEAUTH" in serial, 20 * scale)


def _line(qmp: Qmp, text: str) -> None:
    qmp.type_text(text)
    qmp.press_key("ret")


def _connect(watch: Watch, qmp: Qmp, ssid: str, passphrase: str | None, want: str, scale: float) -> bool:
    """`wifictl connect SSID`, the passphrase typed at its no-echo prompt if
    it asks (a network remembered by an earlier boot of the same OS volume
    joins without one)."""
    seen = watch.text().count(want)
    prompt = f'Passphrase for "{ssid}"'
    asked = watch.text().count(prompt)
    _line(qmp, f"wifictl connect {ssid}")
    if passphrase is not None:
        watch.until(lambda serial, ap: serial.count(prompt) > asked or serial.count(want) > seen, 30 * scale)
        if watch.text().count(prompt) > asked:
            time.sleep(0.5)
            _line(qmp, passphrase)
    return watch.until(lambda serial, ap: serial.count(want) > seen, 90 * scale)


def _last_state(serial: str) -> str:
    """The last `WLANMD:STATE` line printed (the console may prefix it)."""
    states = [line[line.index("WLANMD:STATE"):] for line in serial.splitlines() if "WLANMD:STATE" in line]
    return states[-1] if states else ""


def _ap_seen(prefix: str, ssid: str):
    return lambda serial, ap: any(line.startswith(prefix) and line.endswith(f"ssid={ssid}") for line in ap)


def drive_wlanmd(watch: Watch, qmp: Qmp, control: int, scale: float) -> None:
    """Log in as the session user and drive the real station manager with
    `wifictl` (scan, a wrong passphrase, an open and a WPA2 join), then the
    AP's faults, waiting on evidence at each step (`sim_judge_wlanmd`)."""
    state = sim_judge_wlanmd.state
    if not watch.until(lambda serial, ap: "LazyOS login: " in serial and "WLANMD:RADIO:UP" in serial, 240 * scale):
        return
    time.sleep(1.0)
    _line(qmp, "user")
    time.sleep(0.6)
    _line(qmp, "lazy")
    if not watch.until(lambda serial, ap: "LOGIN:OK:PASS user=user" in serial, 60 * scale):
        return
    time.sleep(3.0)
    _line(qmp, "wifictl scan")
    watch.until(lambda serial, ap: "network(s)" in serial, 60 * scale)
    _connect(watch, qmp, sim_judge.WRONG, "not-the-passphrase",
             state("wlan0", "failed", sim_judge.WRONG, "bad-passphrase"), scale)
    if _connect(watch, qmp, sim_judge.OPEN, None, state("wlan0", "connected", sim_judge.OPEN), scale):
        watch.until(_ap_seen("AP:ICMP:ECHO:REPLY", sim_judge.OPEN), 60 * scale)
    if not _connect(watch, qmp, sim_judge.WPA2, fake_ap.WPA2_PASSPHRASE,
                    state("wlan0", "connected", sim_judge.WPA2), scale):
        return
    if not watch.until(_ap_seen("AP:ICMP:ECHO:REPLY", sim_judge.WPA2), 60 * scale):
        return
    _line(qmp, "wifictl status")
    fault(control, "rekey")
    watch.until(_ap_seen("AP:REKEY:G2:OK", sim_judge.WPA2), 30 * scale)
    authorized = lambda n: lambda serial, ap: sum(  # noqa: E731
        line.startswith(f"AP:AUTHORIZED ssid={sim_judge.WPA2}") for line in ap) >= n
    fault(control, "silence 3")
    watch.until(lambda serial, ap: "reason=beacon-loss" in serial, 30 * scale)
    # The AP keeps answering while silent, so wlanmd may rejoin and lose the
    # beacons again before the 3 s are over: let the silence end and the
    # link settle in `connected` before the next fault.
    time.sleep(3.5)
    watch.until(lambda serial, ap: _last_state(serial).startswith("WLANMD:STATE wlan0 connected"), 90 * scale)
    watch.until(authorized(2), 90 * scale)
    time.sleep(2.0)
    fault(control, "deauth 7")
    watch.until(lambda serial, ap: "reason=deauthenticated" in serial, 30 * scale)
    watch.until(authorized(3), 90 * scale)
    _line(qmp, "wifictl status")
    time.sleep(2.0)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory", default=DEFAULT_MEMORY)
    parser.add_argument("--qemu")
    parser.add_argument("--station", choices=("probe", "wlanmd"), default="probe")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/wifi_sim")
    args = parser.parse_args()
    if args.station == "wlanmd" and not WLANMD_PRESENT:
        print("SKIP: wlanmd is not in this tree; the station-manager flow needs user/src/bin/wlanmd.rs "
              "(run without --station to drive the radio with wifiprobe)")
        return 0
    if not args.no_build and not build(args.station):
        print("FAIL: the image did not build")
        return 1
    args.out.mkdir(parents=True, exist_ok=True)
    serial, ap_log, pcap = args.out / "serial.log", args.out / "ap.log", args.out / "air.pcap"
    for path in (serial, ap_log, pcap):
        path.unlink(missing_ok=True)
    ap_port, guest_port, control = free_udp_port(), free_udp_port(), free_udp_port()
    ap = subprocess.Popen([sys.executable, str(HERE / "fake_ap.py"), "--ap-port", str(ap_port),
                           "--guest-port", str(guest_port), "--control-port", str(control),
                           "--pcap", str(pcap), "--log", str(ap_log)], stdout=subprocess.DEVNULL)
    qemu = find_qemu(args.qemu)
    mac = ":".join(f"{b:02x}" for b in air.GUEST_AIR_MAC)
    extra = list(accel_args(args.accel, qemu) or []) + [
        "-netdev", f"socket,id=air,udp=127.0.0.1:{ap_port},localaddr=127.0.0.1:{guest_port}",
        "-device", f"virtio-net-pci,netdev=air,mac={mac}",
    ]
    qmp_port = free_port()
    command = build_qemu_command(qemu, str(IMAGE), qmp_port, serial, args.memory, extra)
    print(f"launching: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    scale = 3.0 if args.accel in ("none", "tcg") else 1.0
    qmp = None
    try:
        time.sleep(1.0)
        qmp = Qmp("127.0.0.1", qmp_port, 30.0)
        watch = Watch(serial, ap_log, proc)
        if args.station == "probe":
            drive_probe(watch, control, scale)
        else:
            drive_wlanmd(watch, qmp, control, scale)
        watch.echo()
    finally:
        stop_qemu(proc, qmp)
        fault(control, "quit")
        try:
            ap.wait(timeout=10)
        except subprocess.TimeoutExpired:
            ap.kill()
    run = sim_judge.Run(watch.text(), watch.ap(), air.read_pcap(pcap) if pcap.is_file() else [])
    verdict = (sim_judge if args.station == "probe" else sim_judge_wlanmd).judge(run)
    failures = 0
    for name, problems in verdict.items():
        print(f"{'PASS' if not problems else 'FAIL'} {name}")
        for problem in problems:
            print(f"     - {problem}")
        failures += bool(problems)
    print(f"frames on the air: {len(run.frames)}; AP log lines: {len(run.ap)}")
    print("WIFI:SIM: PASS" if not failures else f"WIFI:SIM: {failures} check(s) failed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
