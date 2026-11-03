#!/usr/bin/env python3
"""Boot LazyOS against a fake USB Bluetooth dongle and judge what the guest did.

docs/bluetooth-plan.md phase B2. QEMU has no Bluetooth emulation, so the
dongle is `tools/bt/fakebt`: a host program serving a class `E0/01/01` USB
device over usbredir (`-chardev socket ... -device usb-redir`) on its own
`qemu-xhci`, answering HCI through a stub controller (Reset, Read_BD_ADDR,
Read_Local_Version; `libs/btsim` replaces it). The image is built with
`LAZYOS_BT=1` (which needs `LAZYOS_USB=1` and a services session): `usbd`
binds the controller's interface 0 and serves it as `os.lazy.usb.bt`, and
`btd` (as `_bt`) attaches its rings and brings the controller up with
`HCI_Reset`, `Read_Local_Version` and `Read_BD_ADDR`. The verdict, from the
serial log alone (`judge.py`): the bind, the descriptors, `btd`'s identity and
the controller's answers as `btd` read them.

    python tools/bt/run.py                    # build, boot, judge
    python tools/bt/run.py --no-build         # reuse target/lazyos.img
    python tools/bt/run.py --unplug           # pull the dongle out after bring-up
    python tools/bt/run.py --hotplug          # plug the dongle in 20 s after boot
    python tools/bt/run.py --accel none       # force TCG
    python tools/bt/test_judge.py             # the judge fails when it should

Exit status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools"))
import btdemo  # noqa: E402

#: The dongle is pulled this long after the first HCI command arrived, when
#: the guest has long finished its three-command bring-up.
UNPLUG_AFTER_MS = 2000
#: `--hotplug`: the dongle appears this long after the connection, well after
#: `usbd` finished its boot scan.
HOTPLUG_AFTER_MS = 20_000


def build() -> None:
    env = dict(os.environ, LAZYOS_SERVICES="1", **btdemo.build_env())
    print("building: LAZYOS_SERVICES=1 LAZYOS_USB=1 LAZYOS_BT=1 cargo build", flush=True)
    if subprocess.run(["cargo", "build"], cwd=ROOT, env=env).returncode != 0:
        sys.exit("cargo build failed")


def session_script(unplug: bool, timeout: float) -> list[dict]:
    steps: list[dict] = [{"wait_for": "BT:CTRL:PASS", "timeout": timeout}]
    if unplug:
        steps += [{"wait_for": "USBD:BT:UNBIND", "timeout": 120},
                  {"wait_for": "BT:CTRL:GONE", "timeout": 120}]
    # Let the last lines out before QEMU quits.
    return steps + [{"wait": 2.0}, {"quit": True}]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--image", type=Path, default=ROOT / "target/lazyos.img")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/bt")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory", help="guest RAM, passed to the session tool")
    parser.add_argument("--timeout", type=float, default=600.0, help="overall seconds")
    parser.add_argument("--unplug", action="store_true", help="the dongle is pulled out after the bring-up")
    parser.add_argument("--hotplug", action="store_true", help="the dongle appears well after boot")
    args = parser.parse_args()
    if not args.no_build:
        build()
    args.out.mkdir(parents=True, exist_ok=True)
    script = args.out / "session.json"
    script.write_text(json.dumps(session_script(args.unplug, args.timeout), indent=1))
    fakebt_log = args.out / "fakebt.log"
    fake, dongle = btdemo.start(
        fakebt_log,
        unplug_after_ms=UNPLUG_AFTER_MS if args.unplug else None,
        connect_after_ms=HOTPLUG_AFTER_MS if args.hotplug else 0,
    )
    command = [
        sys.executable, str(ROOT / "tools/screenshot/qemu_session.py"),
        "--image", str(args.image), "--out", str(args.out), "--script", str(script),
        "--accel", args.accel, "--timeout", str(args.timeout),
        "--fail-on", r"USBD:(FATAL|PANIC)|BT:PANIC",
    ] + (["--memory", args.memory] if args.memory else []) + [f"--extra-arg={arg}" for arg in dongle]
    print("booting: " + " ".join(dongle), flush=True)
    try:
        session = subprocess.run(command, cwd=ROOT)
    finally:
        btdemo.stop(fake)
    log = args.out / "serial.log"
    if not log.exists():
        print("FAIL: no serial log")
        return 1
    verdicts = [session.returncode == 0]
    if session.returncode != 0:
        print(f"FAIL: the session failed (exit {session.returncode}); see {args.out}/summary.json")
    judge = [sys.executable, str(Path(__file__).parent / "judge.py"), str(log), "--addr", btdemo.BD_ADDR]
    if args.unplug:
        judge.append("--unplug")
    verdicts.append(subprocess.run(judge).returncode == 0)
    ok = all(verdicts)
    print("bt harness: " + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
