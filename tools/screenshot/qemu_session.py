#!/usr/bin/env python3
"""Drive a headless QEMU guest with scripted input and timed screenshots.

This lets an agent *interact* with LazyOS (type commands, click, scroll) and
capture the resulting pixels — the automated counterpart to a human at the
keyboard. Boots QEMU with ``-display none`` and talks to it over QMP.

Input is injected with the QMP ``input-send-event`` command. Keyboard uses a US
layout; mouse uses relative motion/buttons (PS/2) by default. For absolute
pointer positioning add ``--tablet`` (attaches ``usb-tablet``; the guest must
enumerate USB).

Script format (JSON)
--------------------
A list of steps, each with an optional ``at`` (seconds since boot) and exactly
one action. Steps without ``at`` run immediately after the previous one.

    [
      {"at": 2.0, "shot": "boot"},
      {"at": 3.0, "type": "hello world"},
      {"at": 3.5, "key": "enter"},
      {"at": 4.0, "shot": "after_enter"},
      {"at": 5.0, "mouse_move": [200, 0]},
      {"at": 5.2, "mouse_click": "left"},
      {"at": 5.5, "mouse_scroll": 3},
      {"at": 6.0, "shot": "after_click"},
      {"at": 7.0, "quit": true}
    ]

Actions: ``shot`` (name), ``type`` (string), ``key`` (name), ``keys`` (list),
``mouse_move`` ([dx, dy]), ``mouse_click`` (left|middle|right),
``mouse_scroll`` (int), ``mouse_abs`` ([x, y]), ``wait`` (seconds), ``quit``.

Usage
-----
    python tools/screenshot/qemu_session.py --image target/lazyos.img \
        --out shots --script tools/screenshot/examples/type_and_shot.json
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

from qemu_qmp import Qmp, build_qemu_command, find_qemu, free_port

_ACTIONS = {
    "shot", "type", "key", "keys", "mouse_move", "mouse_click",
    "mouse_scroll", "mouse_abs", "wait", "quit",
}


def run_steps(qmp: Qmp, steps: list[dict], out_dir: Path, started: float) -> list[str]:
    screenshots: list[str] = []
    for index, step in enumerate(steps):
        if "at" in step:
            remaining = float(step["at"]) - (time.time() - started)
            if remaining > 0:
                time.sleep(remaining)

        action = next((key for key in step if key in _ACTIONS), None)
        if action is None:
            raise SystemExit(f"step {index} has no recognised action: {step}")

        if action == "shot":
            shot = qmp.screenshot(out_dir / f"shot_{step['shot']}")
            screenshots.append(shot.name)
            print(f"captured {shot}", flush=True)
        elif action == "type":
            qmp.type_text(step["type"])
        elif action == "key":
            qmp.press_key(step["key"])
        elif action == "keys":
            for name in step["keys"]:
                qmp.press_key(name)
        elif action == "mouse_move":
            dx, dy = step["mouse_move"]
            qmp.mouse_move(dx, dy)
        elif action == "mouse_click":
            qmp.mouse_click(step["mouse_click"])
        elif action == "mouse_scroll":
            qmp.mouse_scroll(step["mouse_scroll"])
        elif action == "mouse_abs":
            x, y = step["mouse_abs"]
            qmp.mouse_abs(x, y)
        elif action == "wait":
            time.sleep(float(step["wait"]))
        elif action == "quit":
            try:
                qmp.execute("quit")
            except Exception:
                pass
            return screenshots
    return screenshots


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--image", help="raw disk image to boot (omit for firmware only)")
    parser.add_argument("--script", required=True, help="path to a JSON step script")
    parser.add_argument("--out", default="shots", help="output directory (default: shots)")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--timeout", type=float, default=180.0, help="QMP/overall timeout")
    parser.add_argument("--memory", default="256M", help="guest RAM (default: 256M)")
    parser.add_argument("--tablet", action="store_true",
                        help="attach a usb-tablet for absolute pointer positioning")
    parser.add_argument("--extra-arg", action="append", default=[], metavar="ARG",
                        help="extra QEMU argument; repeat for multiple")
    args = parser.parse_args()

    steps = json.loads(Path(args.script).read_text(encoding="utf-8"))
    if not isinstance(steps, list):
        sys.exit("--script must contain a JSON list of steps")

    qemu = find_qemu(args.qemu)
    out_dir = Path(args.out).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    serial_log = out_dir / "serial.log"

    image = None
    if args.image:
        image_path = Path(args.image).resolve()
        if not image_path.is_file():
            sys.exit(f"--image not found: {image_path}")
        image = str(image_path)

    extra = list(args.extra_arg)
    if args.tablet:
        extra += ["-device", "usb-tablet"]

    port = free_port()
    command = build_qemu_command(qemu, image, port, serial_log, args.memory, extra)
    print(f"launching: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

    screenshots: list[str] = []
    qmp: Qmp | None = None
    try:
        qmp = Qmp("127.0.0.1", port, args.timeout)
        screenshots = run_steps(qmp, steps, out_dir, time.time())
        try:
            qmp.execute("quit")
        except Exception:
            pass
    finally:
        if qmp is not None:
            qmp.close()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()

    summary = {
        "qemu": qemu,
        "image": image,
        "screenshots": screenshots,
        "serial_log": serial_log.name if serial_log.exists() else None,
        "exit_code": proc.returncode,
    }
    (out_dir / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
