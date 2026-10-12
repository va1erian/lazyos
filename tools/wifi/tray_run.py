#!/usr/bin/env python3
"""The Wi-Fi tray applet and the Settings country page against the simulated
radio, end to end (issue #777).

Builds a desktop image with the network stack, `wlanmd` and the simulated
radio (`LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_WIFI=1 LAZYOS_WIFI_SIM=1`, the
UI probe and a Terminal at login), starts the fake access points
(`fake_ap.py`, the way `sim_run.py` and `run_demo.py --wifi-sim` do), boots
QEMU with the guest's air card on them and plays
`tools/screenshot/examples/wifi_tray.json`: the tray icon disconnected, its
menu of the fake networks, a click on LazyOpen (joined, bars on the icon), a
click on the secured LazyWPA2 (the Network app opens), Disconnect, and the
Settings Wi-Fi page choosing France approved on the elevd prompt as `admin`.
It judges (`tray_judge.py`) from the serial log and the AP's log, and checks
the screenshots with `pngstats.py`.

    python tools/wifi/tray_run.py                # build, boot, play, judge
    python tools/wifi/tray_run.py --no-build     # reuse target/lazyos.img
    python tools/wifi/tray_run.py --label-trace  # also list LABEL:DENY lines
    python tools/wifi/test_tray_judge.py         # the judge fails when it should

`LAZYOS_WIFI=1` needs the firmware (`python tools/firmware/fetch.py --switch
wifi`) and the desktop apps (`python tools/xui/build.py`); BusyBox comes from
`LAZYOS_BUSYBOX` or `tools/abi/busybox.py`. Output: `shots/wifi_tray/`
(serial.log, ap.log, shot_*.png).
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

import simnet  # noqa: E402
import tray_judge  # noqa: E402
from sim_judge import Run  # noqa: E402

IMAGE = ROOT / "target" / "lazyos.img"
SESSION = ROOT / "tools" / "screenshot" / "examples" / "wifi_tray.json"
NETSTATUS_ELF = ROOT / "target" / "xui" / "xui-netstatus.elf"


def build_env(label_trace: bool) -> dict[str, str]:
    env = dict(os.environ)
    env.update(simnet.build_env(), LAZYOS_DESKTOP="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_WIFI="1", LAZYOS_UI_PROBE="1", LAZYOS_XUI_AUTOSTART="term", LAZYOS_RESET_OS="1")
    if label_trace:
        env["LAZYOS_LABEL_TRACE"] = "1"
    else:
        env.pop("LAZYOS_LABEL_TRACE", None)
    return env


def build(label_trace: bool) -> bool:
    if not NETSTATUS_ELF.is_file():
        print("building the desktop apps (tools/xui/build.py)", flush=True)
        if subprocess.run([sys.executable, str(ROOT / "tools/xui/build.py"), "--no-lazyweb"],
                          cwd=ROOT).returncode != 0:
            return False
    print("building: LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_WIFI=1 LAZYOS_WIFI_SIM=1 cargo build", flush=True)
    return subprocess.run(["cargo", "build"], cwd=ROOT, env=build_env(label_trace)).returncode == 0 \
        and IMAGE.is_file()


def play(out: Path, accel: str, qemu: str | None, memory: str | None) -> int:
    log, pcap = out / "ap.log", out / "air.pcap"
    for stale in (log, pcap):
        stale.unlink(missing_ok=True)
    ap, air = simnet.start(log=log, pcap=pcap)
    command = [sys.executable, str(ROOT / "tools/screenshot/qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(SESSION), "--accel", accel]
    if qemu:
        command += ["--qemu", qemu]
    if memory:
        command += ["--memory", memory]
    command += [f"--extra-arg={arg}" for arg in air]
    print("running:", " ".join(command), flush=True)
    try:
        return subprocess.call(command, cwd=ROOT)
    finally:
        simnet.stop(ap)


def shots_ok(out: Path) -> list[str]:
    shots = sorted(out.glob("shot_*.png")) or sorted(out.glob("*.png"))
    if not shots:
        return ["no screenshots were taken"]
    result = subprocess.run([sys.executable, str(ROOT / "tools/screenshot/pngstats.py"), *map(str, shots),
                             "--min-nonblack", "0.01", "--expect-width", "1280", "--expect-height", "720"],
                            capture_output=True, text=True)
    print(result.stdout, end="")
    return [] if result.returncode == 0 else [f"pngstats failed: {result.stderr.strip() or result.stdout.strip()}"]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--label-trace", action="store_true", help="build with LAZYOS_LABEL_TRACE=1")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory")
    parser.add_argument("--qemu")
    parser.add_argument("--out", type=Path, default=ROOT / "shots" / "wifi_tray")
    args = parser.parse_args()
    if not args.no_build and not build(args.label_trace):
        print("FAIL: the image did not build")
        return 1
    args.out.mkdir(parents=True, exist_ok=True)
    session = play(args.out, args.accel, args.qemu, args.memory)
    serial = args.out / "serial.log"
    ap_log = args.out / "ap.log"
    run = Run(serial.read_text(errors="replace") if serial.is_file() else "",
              ap_log.read_text(errors="replace").splitlines() if ap_log.is_file() else [], [])
    failures = 0
    if session != 0:
        print(f"FAIL session: qemu_session.py exited {session}")
        failures += 1
    verdict = tray_judge.judge(run)
    verdict["screenshots"] = shots_ok(args.out)
    for name, problems in verdict.items():
        print(f"{'PASS' if not problems else 'FAIL'} {name}")
        for problem in problems:
            print(f"     - {problem}")
        failures += bool(problems)
    print("WIFI:TRAY: PASS" if not failures else f"WIFI:TRAY: {failures} check(s) failed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
