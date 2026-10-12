#!/usr/bin/env python3
"""The Network app's Wi-Fi page against the simulated radio and the fake
access points (docs/wifi-prerequisites-plan.md, issue #777).

Builds a desktop image with the station manager and the simulated radio
(`LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_WIFI=1 LAZYOS_WIFI_SIM=1
LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=network`; `LAZYOS_WIFI=1` embeds the
radio firmware, so `python tools/firmware/fetch.py --switch wifi` first),
starts the fake access points (`fake_ap.py`, the way `sim_run.py` does), boots
QEMU with the guest's air card connected to them, and drives the app with the
screenshot session `tools/screenshot/examples/wifi_network_app.json`: open the
Wi-Fi page, join `LazyWPA2` through the password form, save and join the hidden
open `LazyHidden` through "Other network...", see a wrong password refused,
disconnect and forget.

It judges from the serial log (`NETAPP:WIFI:*` markers, no `LABEL:DENY` for the
app, the password nowhere), the AP's log (the station was authorized on the
WPA2 network) and the screenshots (`pngstats`).

    python tools/wifi/ui_run.py                 # build, boot, run, judge
    python tools/wifi/ui_run.py --no-build      # reuse target/lazyos.img
    python tools/wifi/ui_run.py --label-trace   # also LAZYOS_LABEL_TRACE=1: list LABEL:DENY
    python tools/wifi/test_ui_judge.py          # the judge fails when it should

Output: `shots/wifi_network_app/` (shot_*.png, serial.log, ap.log, air.pcap).
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools/screenshot"))

import fake_ap  # noqa: E402
import ui_judge  # noqa: E402
from fakeap import air  # noqa: E402
from sim_run import free_udp_port  # noqa: E402

IMAGE = ROOT / "target" / "lazyos.img"
SCRIPT = ROOT / "tools/screenshot/examples/wifi_network_app.json"


def build(label_trace: bool) -> bool:
    env = dict(os.environ, LAZYOS_DESKTOP="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_WIFI="1", LAZYOS_WIFI_SIM="1", LAZYOS_UI_PROBE="1",
               LAZYOS_XUI_AUTOSTART="network", LAZYOS_RESET_OS="1")
    for key in ("LAZYOS_WIFI_SIM_ARGS", "LAZYOS_NET_ARGS", "LAZYOS_AUTOLOGIN"):
        env.pop(key, None)
    if label_trace:
        env["LAZYOS_LABEL_TRACE"] = "1"
    print("building: LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_WIFI=1 LAZYOS_WIFI_SIM=1 "
          "LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=network cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env)
    return result.returncode == 0 and IMAGE.is_file()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--label-trace", action="store_true", help="build with LAZYOS_LABEL_TRACE=1")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory", default="1G")
    parser.add_argument("--qemu")
    parser.add_argument("--script", type=Path, default=SCRIPT)
    parser.add_argument("--out", type=Path, default=ROOT / "shots/wifi_network_app")
    args = parser.parse_args()
    if not args.no_build and not build(args.label_trace):
        print("FAIL: the image did not build")
        return 1
    args.out.mkdir(parents=True, exist_ok=True)
    ap_log, pcap = args.out / "ap.log", args.out / "air.pcap"
    for path in (ap_log, pcap):
        path.unlink(missing_ok=True)
    ap_port, guest_port, control = free_udp_port(), free_udp_port(), free_udp_port()
    ap = subprocess.Popen([sys.executable, str(HERE / "fake_ap.py"), "--ap-port", str(ap_port),
                           "--guest-port", str(guest_port), "--control-port", str(control),
                           "--pcap", str(pcap), "--log", str(ap_log)], stdout=subprocess.DEVNULL)
    mac = ":".join(f"{b:02x}" for b in air.GUEST_AIR_MAC)
    session = [sys.executable, str(ROOT / "tools/screenshot/qemu_session.py"),
               "--image", str(IMAGE), "--out", str(args.out), "--script", str(args.script),
               "--accel", args.accel, "--memory", args.memory,
               "--extra-arg=-netdev",
               f"--extra-arg=socket,id=air,udp=127.0.0.1:{ap_port},localaddr=127.0.0.1:{guest_port}",
               "--extra-arg=-device", f"--extra-arg=virtio-net-pci,netdev=air,mac={mac}"]
    if args.qemu:
        session += ["--qemu", args.qemu]
    try:
        code = subprocess.run(session, cwd=ROOT).returncode
    finally:
        fake_ap_quit(control)
        try:
            ap.wait(timeout=10)
        except subprocess.TimeoutExpired:
            ap.kill()
    serial = args.out / "serial.log"
    verdict = ui_judge.judge(
        serial.read_text(errors="replace") if serial.is_file() else "",
        ap_log.read_text(errors="replace") if ap_log.is_file() else "",
        sorted(args.out.glob("shot_*.png")),
        fake_ap.WPA2_PASSPHRASE,
    )
    failures = 0
    for name, problems in verdict.items():
        print(f"{'PASS' if not problems else 'FAIL'} {name}")
        for problem in problems:
            print(f"     - {problem}")
        failures += bool(problems)
    if code:
        print(f"FAIL: the session exited with {code}")
    print("WIFI:UI: PASS" if not failures and not code else f"WIFI:UI: {failures} check(s) failed")
    return 1 if failures or code else 0


def fake_ap_quit(control: int) -> None:
    import socket
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.sendto(b"quit", ("127.0.0.1", control))


if __name__ == "__main__":
    sys.exit(main())
