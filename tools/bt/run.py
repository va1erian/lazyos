#!/usr/bin/env python3
"""Boot LazyOS against a fake USB Bluetooth dongle and judge what the guest did.

docs/bluetooth-plan.md phases B2 and B3. QEMU has no Bluetooth emulation, so
the dongle is `tools/bt/fakebt`: a host program serving a class `E0/01/01` USB
device over usbredir (`-chardev socket ... -device usb-redir`) on its own
`qemu-xhci`, answering HCI through `libs/btsim`'s simulator, which also plays
the keyboard and the mouse. The image is built with `LAZYOS_BT=1` (which needs
`LAZYOS_USB=1` and a services session) and `LAZYOS_BT_TRACE=1`, the harness
build in which `init` starts `btd` with `trace=1` and a pairing window for the
two peers; `usbd` binds the controller's interface 0 and serves it as
`os.lazy.usb.bt`, and `btd` brings it up and runs the host stack.

    python tools/bt/run.py                    # B2: build, boot, judge the bring-up
    python tools/bt/run.py --unplug           # pull the dongle out after bring-up
    python tools/bt/run.py --hotplug          # plug the dongle in 20 s after boot
    python tools/bt/run.py --pair             # pair the mouse and the keyboard, type, move
    python tools/bt/run.py --reboot           # pair, power off, boot the same image: it types at boot
    python tools/bt/run.py --boot-typing      # B6: as --reboot, plus the boot's spans and their budgets
    python tools/bt/run.py --le-boot-typing   # B6: as --le-reboot, plus the spans and budgets
    python tools/bt/run.py --refuse           # an unbonded peer pages the host and types: refused
    python tools/bt/run.py --kill             # btd dies holding a key: nothing stuck, it reattaches
    python tools/bt/run.py --le-pair          # B4: an LE keyboard (passkey) and mouse pair, type, move
    python tools/bt/run.py --le-reboot        # pair over LE, power off, boot the same image: it types at boot
    python tools/bt/run.py --le-rpa           # as --le-reboot, the devices change address each connection
    python tools/bt/run.py --le-refuse        # a Just Works LE keyboard is refused, a stranger never connected
    python tools/bt/run.py --fw intel         # B7: an Intel radio in its bootloader: btd downloads the firmware, then pairs
    python tools/bt/run.py --fw intel-recovery  # ... whose first image is rejected: recovery reset, re-enumeration, success
    python tools/bt/run.py --fw intel-tlv     # ... an AX211 (TLV version answer, both signature blocks)
    python tools/bt/run.py --fw intel-reboot   # ... that leaves the bus with the boot reset and returns running the image
    python tools/bt/run.py --fw realtek       # ... a Realtek ROM that takes a patch
    python tools/bt/run.py --no-build         # reuse target/lazyos.img
    python tools/bt/run.py --accel none       # force TCG
    python tools/bt/test_judge.py             # the judges fail when they should

The HID modes judge `inputd`'s keys against `btd`'s (`judge_hid.py`, and
`judge_le.py` for the LE modes, whose peers are `modes_le.py`'s); the
transport modes judge the bring-up (`judge.py`). Every boot runs on a copy of
the image (`<out>/disk.img`), so the bonds one run leaves never leak into
another, while `--reboot` boots the same copy twice. Exit status is non-zero on
any failure.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import btdemo  # noqa: E402
import desktop  # noqa: E402
import judge_hid  # noqa: E402
import modes  # noqa: E402
import modes_fw  # noqa: E402
import modes_le  # noqa: E402
import spans  # noqa: E402

#: The dongle is pulled this long after the first HCI command arrived, when
#: the guest has long finished its bring-up.
UNPLUG_AFTER_MS = 2000
#: `--hotplug`: the dongle appears this long after the connection, well after
#: `usbd` finished its boot scan.
HOTPLUG_AFTER_MS = 20_000


#: B6: a bonded peer that types from the moment the controller is up is what
#: the reboot modes already play (its keys wait for the link on the second
#: boot); these judge that boot's spans against the budgets as well.
BOOT_TYPING = {"boot-typing": "reboot", "le-boot-typing": "le-reboot"}


def hardware_accel(args) -> bool:
    """Whether the guest runs on WHPX/KVM (the budgets of `spans.py` apply)."""
    if args.accel == "none":
        return False
    sys.path.insert(0, str(ROOT / "tools/screenshot"))
    import qemu_qmp
    return qemu_qmp.resolve_accel(args.accel, qemu_qmp.find_qemu()) != "none"


def build(crash: bool, desktop: bool = False, reset: bool = True, in_place: bool = False) -> None:
    # The image carries the controller firmware `btd` loads (LAZYOS_BT=1).
    fetch = [sys.executable, str(ROOT / "tools/firmware/fetch.py"), "--switch", "bt"]
    if subprocess.run(fetch, cwd=ROOT).returncode != 0:
        sys.exit("fetching the Bluetooth firmware failed")
    env = dict(os.environ, LAZYOS_SERVICES="1", **({"LAZYOS_DESKTOP": "1"} if desktop else {}), **({"LAZYOS_RESET_OS": "1"} if reset else {}), **({"LAZYOS_UPDATE_DAMAGED_OS": "1"} if in_place else {}), LAZYOS_BT_TRACE="1", LAZYOS_BT_PAIR=modes.PAIR_LIST,
               LAZYOS_BT_CRASH_TEST="1" if crash else "0", **btdemo.build_env())
    print("building: " + ("LAZYOS_DESKTOP=1 " if desktop else "") + "LAZYOS_SERVICES=1 LAZYOS_USB=1 LAZYOS_BT=1 LAZYOS_BT_TRACE=1"
          + (" LAZYOS_BT_CRASH_TEST=1" if crash else "") + " cargo build", flush=True)
    if subprocess.run(["cargo", "build"], cwd=ROOT, env=env).returncode != 0:
        sys.exit("cargo build failed")


def transport_steps(unplug: bool, timeout: float) -> list[dict]:
    steps: list[dict] = [{"wait_for": "BT:CTRL:PASS", "timeout": timeout}]
    if unplug:
        steps += [{"wait_for": "USBD:BT:UNBIND", "timeout": 120},
                  {"wait_for": "BT:CTRL:GONE", "timeout": 120}]
    # Let the last lines out before QEMU quits.
    return steps + [{"wait": 2.0}, {"quit": True}]


def boot(args, out: Path, image: Path, steps: list[dict], fakebt: list[str], **fake) -> bool:
    """One QEMU boot with a fresh `fakebt`; whether the session itself passed."""
    out.mkdir(parents=True, exist_ok=True)
    script = out / "session.json"
    script.write_text(json.dumps(steps, indent=1))
    fake_proc, dongle = btdemo.start(out / "fakebt.log", extra=fakebt, **fake)
    command = [
        sys.executable, str(ROOT / "tools/screenshot/qemu_session.py"),
        "--image", str(image), "--out", str(out), "--script", str(script),
        "--accel", args.accel, "--timeout", str(args.timeout),
        "--fail-on", r"USBD:(FATAL|PANIC)|BT:PANIC",
    ] + (["--memory", args.memory] if args.memory else []) + [f"--extra-arg={arg}" for arg in dongle]
    print("booting: " + " ".join(dongle), flush=True)
    try:
        session = subprocess.run(command, cwd=ROOT)
    finally:
        btdemo.stop(fake_proc)
    if session.returncode != 0:
        print(f"FAIL: the session failed (exit {session.returncode}); see {out}/summary.json")
    return session.returncode == 0


def run_transport(args) -> bool:
    out = args.out
    fake = {"unplug_after_ms": UNPLUG_AFTER_MS if args.unplug else None,
            "connect_after_ms": HOTPLUG_AFTER_MS if args.hotplug else 0}
    ok = boot(args, out, args.image, transport_steps(args.unplug, args.timeout), [], **fake)
    log = out / "serial.log"
    if not log.exists():
        print("FAIL: no serial log")
        return False
    judge = [sys.executable, str(Path(__file__).parent / "judge.py"), str(log), "--addr", btdemo.BD_ADDR]
    if args.unplug:
        judge.append("--unplug")
    return subprocess.run(judge).returncode == 0 and ok


def run_hid(args, mode: str) -> bool:
    """The HID modes (BR/EDR and LE): boots on a copy of the image."""
    typing = mode in BOOT_TYPING
    mode = BOOT_TYPING.get(mode, mode)
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    image = out / "disk.img"
    shutil.copyfile(args.image, image)
    state = out / "peers.state"
    state.unlink(missing_ok=True)
    boots = 2 if mode == "reboot" or mode in modes_le.TWO_BOOTS else 1
    logs = []
    ok = True
    for number in range(1, boots + 1):
        where = out / f"boot{number}" if boots > 1 else out
        steps = modes.session_steps(mode, args.timeout, number)
        ok &= boot(args, where, image, steps, modes.fakebt_args(mode, state, number))
        logs.append(where / "serial.log")
        if not logs[-1].exists():
            print("FAIL: no serial log")
            return False
    script = "judge_le.py" if mode in modes_le.LE_MODES else "judge_hid.py"
    judge = [sys.executable, str(Path(__file__).parent / script), mode, *map(str, logs)]
    text = {"pair": modes.PAIR_TEXT, "reboot": modes.BOOT_TEXT, "kill": modes.RESTART_TEXT,
            "le-pair": modes.PAIR_TEXT, "le-reboot": modes.BOOT_TEXT,
            "le-rpa": modes.BOOT_TEXT + modes_le.RPA_TEXT_AFTER}.get(mode)
    if text:
        judge += ["--text", text]
    if mode == "reboot" or mode in modes_le.TWO_BOOTS:
        judge += ["--pair-text", modes.PAIR_TEXT]
    passed = subprocess.run(judge).returncode == 0 and ok
    if typing:
        relaxed = not hardware_accel(args)
        passed = not spans.report(logs[-1].read_text(errors="replace"), relaxed) and passed
        print("BT:JUDGE:" + ("PASS" if passed else "FAIL") + " boot-typing")
    return passed


def run_fw(args, kind: str) -> bool:
    """`--fw KIND`: a radio that needs firmware, then the pairing test."""
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    image = out / "disk.img"
    shutil.copyfile(args.image, image)
    state = out / "peers.state"
    state.unlink(missing_ok=True)
    ok = boot(args, out, image, modes_fw.session_steps(args.timeout), modes_fw.fakebt_args(kind, state))
    log = out / "serial.log"
    if not log.exists():
        print("FAIL: no serial log")
        return False
    judge = [sys.executable, str(Path(__file__).parent / "judge_fw.py"), kind, str(log), "--text", modes.PAIR_TEXT]
    return subprocess.run(judge).returncode == 0 and ok


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--image", type=Path, default=ROOT / "target/lazyos.img")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/bt")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory", help="guest RAM, passed to the session tool")
    parser.add_argument("--timeout", type=float, default=600.0, help="overall seconds")
    parser.add_argument("--unplug", action="store_true", help="the dongle is pulled out after the bring-up")
    parser.add_argument("--desktop", action="store_true", help="B6: with --boot-typing, boot a desktop image and look at the greeter (desktop.py)")
    parser.add_argument("--hotplug", action="store_true", help="the dongle appears well after boot")
    parser.add_argument("--fw", choices=sorted(modes_fw.KINDS), help="a radio that needs firmware (B7), then --pair")
    hid_modes = ("pair", "reboot", "refuse", "kill", *modes_le.LE_MODES, *BOOT_TYPING)
    for mode in hid_modes:
        parser.add_argument(f"--{mode}", action="store_true", help=f"the HID {mode} test (see above)")
    args = parser.parse_args()
    chosen = [m for m in hid_modes if getattr(args, m.replace("-", "_"))]
    if args.fw and (chosen or args.unplug or args.hotplug):
        parser.error("--fw is a mode of its own")
    if len(chosen) > 1 or (chosen and (args.unplug or args.hotplug)):
        parser.error("choose one of the HID modes (--pair ... --le-refuse), or the transport options")
    if args.desktop:
        if chosen != ["boot-typing"]:
            parser.error("--desktop goes with --boot-typing")
        ok = desktop.run(args, ROOT, build, boot, spans.report, hardware_accel(args))
        print("bt harness: " + ("PASS" if ok else "FAIL"))
        return 0 if ok else 1
    if not args.no_build:
        build(crash=bool(chosen and chosen[0] == "kill"))
    if args.fw:
        ok = run_fw(args, args.fw)
    else:
        ok = run_hid(args, chosen[0]) if chosen else run_transport(args)
    print("bt harness: " + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
