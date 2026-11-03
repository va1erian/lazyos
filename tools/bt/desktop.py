"""`tools/bt/run.py --boot-typing --desktop` (docs/bluetooth-plan.md B6): keys
from a bonded keyboard, typed from the moment the controller is up, must reach
the greeter of a desktop image.

A desktop image keeps no harness arguments for `btd` (`init` drops every word
of a service's arguments that is not an identity argument outside the boot
evidence profile), so it can neither be paired by the harness nor echo keys on
serial. The test therefore pairs on the ordinary harness image first, then
lets `cargo build` update that very disk image in place into a desktop image
(the OS volume, and the bond file on it, survive) and boots it once with the
peers that remember their keys. The evidence is `btd`'s spans, the keyboard
reaching the greeter (a screenshot to read) and `inputd`'s session log.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

import modes

#: Typed by the peer when the machine boots (`modes.BOOT_TEXT`) and again
#: once the greeter is up, so a screenshot shows what a key typed too early
#: became. Fakebt's clock starts before QEMU's: this is well after the greeter.
GREETER_TEXT = "go"
GREETER_AT_MS = 25_000


def greeter_steps(timeout: float) -> list[dict]:
    wait = lambda marker: {"wait_for": marker, "timeout": timeout}  # noqa: E731
    # Tab from the console keyboard puts the focus in the name field (the
    # greeter starts with none, so keys typed before it would go nowhere).
    return [wait("BT:HID:KBD"), wait("GREETER:UP:PASS"), {"wait": 2.0}, {"key": "tab"},
            {"wait": 22.0}, {"shot": "greeter"}, {"quit": True}]


def greeter_peers(state: Path) -> list[str]:
    peers = modes.fakebt_args("reboot", state, 2)
    return peers + ["--at", f"{GREETER_AT_MS}:type={GREETER_TEXT}"]


def run(args: argparse.Namespace, root: Path, build, boot, report, hardware: bool) -> bool:
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    image = out / "disk.img"
    # 1. Pair on the harness image (the desktop image drops the pairing window).
    if not args.no_build:
        # (`build` recreates the OS volume: no bonds from an earlier run.)
        build(crash=False)
    shutil.copyfile(args.image, image)
    state = out / "peers.state"
    state.unlink(missing_ok=True)
    pair_out = out / "pair"
    ok = boot(args, pair_out, image, modes.session_steps("pair", args.timeout), modes.fakebt_args("pair", state))
    # 2. The same disk image, updated in place into a desktop image.
    shutil.copyfile(image, root / "target/lazyos.img")
    # A QEMU that was told to quit leaves the volume marked unclean; it is the
    # harness's own, so the update may go ahead.
    build(crash=False, desktop=True, reset=False, in_place=True)
    shutil.copyfile(root / "target/lazyos.img", image)
    where = out / "greeter"
    ok &= boot(args, where, image, greeter_steps(args.timeout), greeter_peers(state))
    log = (where / "serial.log").read_text(errors="replace")
    published = re.findall(r"BT:RECONNECT ms=(\d+) addr=\S+ link_at=(\d+)", log)
    print(f"BT:DESKTOP reconnects={len(published)} screenshot={where / 'shot_greeter.png'}")
    for line in re.findall(r"^(?:INPUTD:SESSION:OPEN|GREETER:UP:PASS|INIT:LAUNCH:PASS app=greeter).*$", log, re.M):
        print(f"BT:DESKTOP {line}")
    failed = report(log, not hardware)
    return ok and not failed and len(published) == 2
