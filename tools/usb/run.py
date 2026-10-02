#!/usr/bin/env python3
"""Boot LazyOS on USB input alone and judge what reached `inputd`.

docs/usb-hid-plan.md U2. QEMU runs with `qemu-xhci`, a `usb-kbd` and (unless
`--no-mouse`) a `usb-mouse`, and **without an i8042** (`-machine pc,i8042=off`),
so every key and pointer record in the trace came through `usbd`. The session
types the fixed physical key sequence of `tools/screenshot/examples/input_keys.json`,
then drives the mouse: into the corner, +40,+30, a left click, two wheel
notches. The verdict:

* `tools/input/verify_trace.py --layout us`: `inputd` typed the right text;
* `judge.py`: every key edge `inputd` saw is one `usbd` sent, the cursor moved,
  clicked and scrolled where the steps say, and the descriptors are QEMU's.

    python tools/usb/run.py                  # build (LAZYOS_SERVICES=1 LAZYOS_USB=1), boot, judge
    python tools/usb/run.py --no-build       # reuse target/lazyos.img
    python tools/usb/run.py --ps2            # keep the i8042 (PS/2 and USB side by side)
    python tools/usb/run.py --no-mouse       # keyboard only
    python tools/usb/run.py --accel none     # force TCG (paces input for a slow guest)
    python tools/usb/run.py --hotplug 200    # U3: unplug/replug cycles over QMP
    python tools/usb/run.py --tablet         # U4: a usb-tablet instead of the mouse
    python tools/usb/run.py --restart        # U5: usbd dies holding a key, init restarts it
    python tools/usb/run.py --machine q35 --virtio-disk   # U5: on q35 (virtio-blk boot disk)
    python tools/usb/run.py --hub            # H3: keyboard and mouse behind a usb-hub, then the hub unplugged
    python tools/usb/run.py --full-speed     # H3: full-speed (USB 1.1) keyboard and mouse on root ports
    python tools/usb/run.py --controllers 2  # H3: two xHCI controllers, keyboard on the second

`--hub` (docs/real-pc-boot-plan.md H3) puts QEMU's `usb-hub` (a full-speed
USB 1.1 hub) on root port 1 and the keyboard and mouse on its ports 1 and 2,
so both enumerate at full speed with a route string; after the typing and
mouse steps the hub is unplugged over QMP and both must detach with it.
`--full-speed` attaches `usb_version=1` devices straight to root ports
(endpoint 0 of 8 bytes, fixed by Evaluate Context; full-speed interval
encoding). `--controllers N` adds N `qemu-xhci` controllers and puts the
keyboard on the last one and the mouse on the first, so input works
whichever controller a port belongs to.

`--hotplug N` (docs/usb-hid-plan.md U3) unplugs and replugs the keyboard N
times (the mouse every tenth cycle) with QMP `device_del`/`device_add`. The
first cycle holds a key and a mouse button while it unplugs, so the judge can
check nothing stays stuck; the last types a few keys on the replugged
keyboard. `judge.py --hotplug N` then checks every cycle detached and
re-attached, `usbd`'s DMA allocations stayed bounded, and every key and
button ended up released.

USB input is polled, and QEMU's `usb-kbd` queues only 16 keycodes before it
drops events. Under TCG every keystroke makes the console redraw for about a
second, during which `usbd` (a Normal-class task) waits for the CPU, so a
fast typist overruns the queue; a press and its release can also land more
than the 500 ms repeat delay apart (a spurious repeat). `--settle` waits after
`USBD:READY` and `--pace` stretches every wait between keys; both default to
slow values under `--accel none`, which still cannot rule the repeat out: TCG
runs are indicative, KVM runs (CI) are the verdict.

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
KEYS_SCRIPT = ROOT / "tools/screenshot/examples/input_keys.json"

#: Mouse steps after typing. The waits are generous: under TCG the guest can
#: take seconds to drain a large motion through 127-count USB reports, and a
#: move that arrives before the corner is reached would be absorbed by it.
def mouse_steps(corner_wait: float) -> list[dict]:
    return [
    {"wait": 2.0},
    {"mouse_move": [-2000, -2000]},
    {"wait": corner_wait},
    {"mouse_move": [40, 30]},
    {"wait": 5.0},
    {"mouse_click": "left"},
    {"wait": 3.0},
    {"mouse_scroll": 2},
    {"wait": 5.0},
    ]


#: Where `--tablet` puts the cursor, in QMP absolute units (`0..=0x7fff`):
#: the corner, the far corner, then a point inside where it clicks and
#: scrolls. `judge.py --tablet` holds the same list.
TABLET_POINTS = [(0, 0), (0x7FFF, 0x7FFF), (0x4000, 0x2000)]


def tablet_steps(pace: float) -> list[dict]:
    steps: list[dict] = []
    for x, y in TABLET_POINTS:
        steps += [{"wait": max(pace, 2.0)}, {"mouse_abs": [x, y]}]
    return steps + [
        {"wait": max(pace, 2.0)}, {"mouse_click": "left"},
        {"wait": max(pace, 2.0)}, {"mouse_scroll": 2},
        {"wait": max(pace, 3.0)},
    ]


def restart_steps(pace: float) -> list[dict]:
    """Hold `x`: the crash-test build of usbd exits right after publishing it.
    Wait for init to restart usbd and the devices to come back, then type."""
    steps: list[dict] = [
        {"key_down": "x"},
        {"wait_for": "INIT:RESTART:PASS name=usbd", "timeout": 120},
        {"wait_for": "USBD:READY", "occurrence": 2, "timeout": 300},
        {"wait": max(pace, 1.0)},
        {"key_up": "x"},
    ]
    for key in HOTPLUG_KEYS:
        steps += [{"wait": max(pace, 0.5)}, {"key": key}]
    return steps + [{"wait_for": r"INPUTD:KEY code=0x6 \S+ \S+ up", "regex": True, "timeout": 120}]


#: Keys typed on the keyboard after the last replug (usages a, b, c).
HOTPLUG_KEYS = ["a", "b", "c"]


def hotplug_steps(cycles: int, pace: float) -> list[dict]:
    """Unplug and replug the keyboard `cycles` times (the mouse every tenth)."""
    kbd = {"driver": "usb-kbd", "id": "kbd"}
    mouse = {"driver": "usb-mouse", "id": "mouse"}
    steps: list[dict] = [
        # Hold a key and a button across the first unplug.
        {"key_down": "x"}, {"mouse_down": "left"}, {"wait": max(pace, 1.0)},
    ]
    detached = attached_kbd = attached_mouse = 0
    for cycle in range(cycles):
        devices = [("kbd", kbd)] + ([("mouse", mouse)] if cycle % 10 == 0 else [])
        for name, device in devices:
            steps.append({"qmp": "device_del", "args": {"id": name}})
            detached += 1
            steps.append({"wait_for": "USBD:DETACH", "occurrence": detached, "timeout": 120})
            steps.append({"qmp": "device_add", "args": device})
            if name == "kbd":
                attached_kbd += 1
                marker = "USBD:HID:KBD"
                count = attached_kbd
            else:
                attached_mouse += 1
                marker = "USBD:HID:MOUSE"
                count = attached_mouse
            steps.append({"wait_for": marker, "occurrence": count + 1, "timeout": 120})
            if cycle == 0:
                # Let QEMU forget the held input; the fresh device reports
                # nothing held, so this makes no edge in the guest.
                steps.append({"key_up": "x"} if name == "kbd" else {"mouse_up": "left"})
    for key in HOTPLUG_KEYS:
        steps += [{"wait": max(pace, 0.5)}, {"key": key}]
    # Quit only once `inputd` has the last release (slow under TCG).
    return steps + [{"wait_for": r"INPUTD:KEY code=0x6 \S+ \S+ up", "regex": True, "timeout": 120}]


def build(crash_test: bool = False) -> None:
    # LAZYOS_USB_TRACE: usbd echoes key edges for the judge (test images only).
    env = dict(os.environ, LAZYOS_SERVICES="1", LAZYOS_USB="1", LAZYOS_USB_TRACE="1")
    # Always set, so a plain build after a --restart one drops the crash.
    env["LAZYOS_USB_CRASH_TEST"] = "1" if crash_test else "0"
    print("building: LAZYOS_SERVICES=1 LAZYOS_USB=1 LAZYOS_USB_TRACE=1 "
          f"LAZYOS_USB_CRASH_TEST={int(crash_test)} cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env)
    if result.returncode != 0:
        sys.exit("cargo build failed")


#: How long the session holds `x` for `verify_trace`'s key-repeat check
#: (at least 8 repeats after the 500 ms delay). The script holds it 1.5 s of
#: wall time, but the guest's tick clock can run at 60% of wall time on a
#: loaded KVM runner (a CI trace measured 97 ticks for a 1.61 s hold), which
#: leaves too few repeats. 2 s gives enough on a slow guest clock and stays
#: under the 60-repeat cap on a true one.
REPEAT_HOLD = 2.0


def stretch_repeat_hold(steps: list[dict]) -> None:
    """Lengthen the wait right after `x` goes down to REPEAT_HOLD seconds."""
    for index, step in enumerate(steps):
        if step.get("key_down") == "x":
            for later in steps[index + 1:]:
                if "key_up" in later:
                    return
                if "wait" in later and later["wait"] >= 1.0:
                    later["wait"] = max(later["wait"], REPEAT_HOLD)
                    return


#: How `--hub` ends: unplug the hub, both devices behind it detach with it.
HUB_UNPLUG = [
    {"qmp": "device_del", "args": {"id": "hub"}},
    {"wait_for": r"USBD:DETACH port=\S+ slot=\d+ regions=\d+ functions=hub ", "regex": True,
     "timeout": 120},
]


def usb_devices(mouse: bool, tablet: bool, hub: bool, full_speed: bool,
                controllers: int) -> list[str]:
    """The QEMU `-device` arguments of the controllers and the devices."""
    names = [f"xhci{n}" for n in range(max(controllers, 1))]
    extra: list[str] = []
    for name in names:
        extra += ["-device", f"qemu-xhci,id={name}"]
    speed = ",usb_version=1" if full_speed else ""
    pointer = "usb-tablet,id=tablet" if tablet else ("usb-mouse,id=mouse" if mouse else None)
    if hub:
        extra += ["-device", f"usb-hub,id=hub,bus={names[0]}.0,port=1"]
        extra += ["-device", f"usb-kbd,id=kbd,bus={names[0]}.0,port=1.1{speed}"]
        if pointer:
            extra += ["-device", f"{pointer},bus={names[0]}.0,port=1.2{speed}"]
        return extra
    extra += ["-device", f"usb-kbd,id=kbd,bus={names[-1]}.0{speed}"]
    if pointer:
        extra += ["-device", f"{pointer},bus={names[0]}.0{speed}"]
    return extra


def session_script(mouse: bool, pace: float, settle: float, slow: bool, hotplug: int,
                   tablet: bool = False, restart: bool = False, hub: bool = False) -> list[dict]:
    if restart:
        ready = [{"wait_for": "USBD:READY", "timeout": 600}, {"wait": settle}]
        return ready + restart_steps(pace) + [{"quit": True}]
    if hotplug:
        ready = [{"wait_for": "USBD:READY", "timeout": 600}, {"wait": settle}]
        return ready + hotplug_steps(hotplug, pace) + [{"quit": True}]
    steps = json.loads(KEYS_SCRIPT.read_text())
    steps = [step for step in steps if not step.get("quit")]
    for step in steps:
        if "wait" in step:
            step["wait"] = max(step["wait"], pace)
    stretch_repeat_hold(steps)
    # Wait for the driver too, not only `inputd`, then let the boot settle.
    steps[:0] = [{"wait_for": "USBD:READY", "timeout": 600}, {"wait": settle}]
    if tablet:
        steps += tablet_steps(pace)
    elif mouse:
        steps += mouse_steps(60.0 if slow else 5.0)
    if hub:
        steps += HUB_UNPLUG
    return steps + [{"quit": True}]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--image", type=Path, default=ROOT / "target/lazyos.img")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/usb")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--ps2", action="store_true", help="keep the i8042")
    parser.add_argument("--no-mouse", action="store_true")
    parser.add_argument("--timeout", type=float, default=1200.0)
    parser.add_argument("--pace", type=float, help="shortest wait between keys, seconds")
    parser.add_argument("--settle", type=float, help="seconds to wait after USBD:READY")
    parser.add_argument("--restart", action="store_true",
                        help="usbd exits holding a key; init restarts it (U5, crash-test build)")
    parser.add_argument("--machine", help="QEMU machine type, e.g. q35 (default: pc)")
    parser.add_argument("--virtio-disk", action="store_true",
                        help="boot disk on virtio-blk (needed on q35, which has no IDE the kernel drives)")
    parser.add_argument("--tablet", action="store_true",
                        help="a usb-tablet instead of the usb-mouse (U4)")
    parser.add_argument("--hotplug", type=int, default=0, metavar="N",
                        help="unplug/replug cycles instead of the typing session (U3)")
    parser.add_argument("--hub", action="store_true",
                        help="keyboard and mouse behind a usb-hub; the hub is unplugged at the end (H3)")
    parser.add_argument("--full-speed", action="store_true",
                        help="USB 1.1 (full-speed) keyboard and mouse on root ports (H3)")
    parser.add_argument("--controllers", type=int, default=1, metavar="N",
                        help="N qemu-xhci controllers; keyboard on the last, mouse on the first (H3)")
    args = parser.parse_args()
    if args.hub and (args.hotplug or args.restart):
        parser.error("--hub runs the typing session; it does not combine with --hotplug or --restart")
    slow = args.accel in ("none", "tcg")
    pace = args.pace if args.pace is not None else (3.0 if slow else 0.1)
    settle = args.settle if args.settle is not None else (120.0 if slow else 3.0)
    mouse = not args.no_mouse or args.hotplug > 0
    if not args.no_build:
        build(crash_test=args.restart)
    args.out.mkdir(parents=True, exist_ok=True)
    script = args.out / "session.json"
    script.write_text(json.dumps(session_script(mouse, pace, settle, slow, args.hotplug, args.tablet,
                                                args.restart, args.hub), indent=1))
    extra = usb_devices(mouse, args.tablet, args.hub, args.full_speed, args.controllers)
    machine = args.machine or "pc"
    if not args.ps2:
        machine += ",i8042=off"
    extra = ["-machine", machine] + extra
    image = ["--image", str(args.image)]
    if args.virtio_disk:
        image = []
        extra += ["-drive", f"if=none,id=d0,format=raw,file={args.image.resolve().as_posix()}",
                  "-device", "virtio-blk-pci,drive=d0,disable-modern=on"]
    command = [
        sys.executable, str(ROOT / "tools/screenshot/qemu_session.py"),
        *image, "--out", str(args.out), "--script", str(script),
        "--accel", args.accel, "--timeout", str(args.timeout),
        "--fail-on", "USBD:(FATAL|PANIC)",
    ] + [f"--extra-arg={arg}" for arg in extra]
    print("booting: " + " ".join(extra), flush=True)
    session = subprocess.run(command, cwd=ROOT)
    log = args.out / "serial.log"
    if not log.exists():
        print("FAIL: no serial log")
        return 1
    verdicts = [session.returncode == 0]
    if session.returncode != 0:
        print(f"FAIL: the session failed (exit {session.returncode}); see {args.out}/summary.json")
    if not (args.hotplug or args.restart):
        trace = subprocess.run([sys.executable, str(ROOT / "tools/input/verify_trace.py"), str(log), "--layout", "us"])
        verdicts.append(trace.returncode == 0)
    judge = [sys.executable, str(Path(__file__).parent / "judge.py"), str(log)]
    if args.restart:
        judge.append("--restart")
    elif args.hotplug:
        judge += ["--hotplug", str(args.hotplug)]
    elif args.tablet:
        judge.append("--tablet")
    elif mouse:
        judge.append("--mouse")
    if args.hub:
        judge.append("--hub")
    if args.full_speed:
        judge.append("--full-speed")
    if args.controllers > 1:
        judge += ["--controllers", str(args.controllers)]
    verdicts.append(subprocess.run(judge).returncode == 0)
    ok = all(verdicts)
    print("usb harness: " + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
