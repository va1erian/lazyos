#!/usr/bin/env python3
"""The Removable Drives tray applet (`os.lazy.removable`,
docs/usb-hotplug-plan.md G), on a desktop, with a stick plugged in and pulled
over QMP; the desktop counterpart of `fat_run.py`.

QEMU boots a desktop image (`LAZYOS_DESKTOP=1 LAZYOS_USB=1`, autologin `user`)
with a `qemu-xhci` controller and a 64 MiB FAT32 stick (`MY STICK`, made by
`libs/fatvol`'s `fatstick`) that is **not** attached at boot. The session:

1. waits for the applet (`REMOVABLE:UP:PASS`): no drives, a passive item;
2. plugs the stick: the tooltip says it is mounted and the item flips to
   `Attention` (read the screenshots);
3. `cd`s into the volume in the Terminal and picks **Eject** from the item's
   menu: refused, and the tooltip and menu name the Terminal's shell;
4. leaves the folder, picks **Open MY_STICK**: Files opens the volume;
5. picks **Eject** again: the daemon flushes and unmounts, the tooltip says
   `can be removed`;
6. pulls the stick (`removed`), plugs it again (`mounted`), and pulls it
   **without** ejecting: `was removed without ejecting`.

The judge reads the serial log (the applet's markers, `mountd`'s, no
`FATFUSE:FAIL`, no `LABEL:DENY` for the applet) and the stick on the host
(`fsck.fat -n` clean, not marked dirty). The screenshots in `--out` are the
verdict on what the tray looks like: read them.

    python tools/storage/removable_run.py               # build, boot, judge
    python tools/storage/removable_run.py --no-build    # reuse target/lazyos.img
    python tools/storage/removable_run.py --accel none  # force TCG (slow)

Needs the xui apps (`python tools/xui/build.py`) and BusyBox
(`LAZYOS_BUSYBOX`, or `target/abi/busybox/busybox`).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))

import fat_run as fat  # noqa: E402

APP = "os.lazy.removable"
TRAY = f"tray:{APP}"
MOUNT = fat.MOUNT
FAIL_ON = r"LazyOS PANIC|FATFUSE:FAIL|REMOVABLE:RUN:FAIL"
BUSYBOX = ROOT / "target/abi/busybox/busybox"


def build() -> bool:
    busybox = os.environ.get("LAZYOS_BUSYBOX") or str(BUSYBOX)
    if not Path(busybox).is_file():
        print(f"missing {busybox}: run tools/abi/busybox.py or set LAZYOS_BUSYBOX")
        return False
    env = dict(os.environ, LAZYOS_DESKTOP="1", LAZYOS_USB="1", LAZYOS_XUI_AUTOSTART="term",
               LAZYOS_UI_PROBE="1", LAZYOS_LABEL_TRACE="1", LAZYOS_RESET_OS="1",
               LAZYOS_BUSYBOX=busybox)
    print("building: LAZYOS_DESKTOP=1 LAZYOS_USB=1 LAZYOS_XUI_AUTOSTART=term "
          "LAZYOS_UI_PROBE=1 LAZYOS_LABEL_TRACE=1 LAZYOS_RESET_OS=1 cargo build", flush=True)
    return subprocess.run(["cargo", "build"], cwd=ROOT, env=env).returncode == 0


def right_click(until: str) -> dict:
    return {"click_at": {"target": TRAY}, "button": "right", "until": until,
            "timeout": 30, "retries": 2}


def pick(row: str, until: str) -> dict:
    return {"click_at": {"target": f"traymenu:{row}"}, "until": until, "regex": True,
            "timeout": 30, "retries": 1}


def terminal(command: str) -> list[dict]:
    return [{"click_at": {"window": "Terminal"}, "note": "focus the Terminal"},
            {"wait": 0.8}, {"type": command}, {"wait": 0.4}, {"key": "enter"}, {"wait": 1.5}]


def session(fat_img: Path, slow: bool) -> list[dict]:
    up = 600 if slow else 120
    menu = f"SHELL:TRAY:MENU:OPEN app={APP}"
    steps: list[dict] = [
        {"wait_for": "SHELL:TRAY:SERVICE:PASS", "timeout": 600 if slow else 240},
        {"wait_for": "REMOVABLE:UP:PASS", "timeout": 600 if slow else 240},
        {"wait_for": "TERM:UP:PASS", "timeout": 240},
        {"wait_for": "REMOVABLE:STATE No removable drives", "timeout": 60},
        {"wait": 2.0}, {"shot": "01_no_drives"},
        # 2. Plug, after the applet's first looks: a stick that shows up
        # within them is taken for the login's and mounts silently.
        {"wait": 20.0 if slow else 8.0},
        fat.plug("fat0", "usbfat0"),
        {"wait_for": f"REMOVABLE:STATE MY_STICK mounted at {MOUNT}", "timeout": up},
        {"wait_for": f"SHELL:TRAY:UPDATE app={APP}", "timeout": 30},
        {"wait": 1.0}, {"shot": "02_mounted_attention"},
        {"move_to": {"target": TRAY}},
        {"wait_for": f"SHELL:TRAY:TOOLTIP app={APP}", "timeout": 30},
        {"wait": 1.0}, {"shot": "03_tooltip"},
        right_click(menu), {"wait": 1.0}, {"shot": "04_menu"}, {"key": "escape"},
        # 3. A shell inside the volume holds it: refused, with the holder.
        # (Before Files opens: its window would cover the Terminal.)
        *terminal(f"cd {MOUNT}"),
        right_click(menu),
        pick("Eject MY_STICK", r"REMOVABLE:EJECT usb\d+ busy"),
        {"wait_for": "REMOVABLE:STATE Cannot eject MY_STICK", "timeout": 30},
        {"wait": 1.0}, {"shot": "05_busy_tooltip"},
        right_click(menu), {"wait": 1.0}, {"shot": "06_busy_menu"},
        {"key": "escape"},
        # 4. Out of the volume; Open shows it in Files.
        *terminal("cd /"),
        right_click(menu),
        pick("Open MY_STICK", f"REMOVABLE:OPEN {re.escape(MOUNT)} ok"),
        {"wait_for": "INIT:LAUNCH:PASS app=os.lazy.files", "timeout": 120},
        {"wait": 3.0}, {"shot": "07_files"},
        # 5. Nothing holds the volume now: ejected.
        right_click(menu),
        pick("Eject MY_STICK", r"REMOVABLE:EJECT usb\d+ ejecting"),
        {"wait_for": "REMOVABLE:STATE MY_STICK can be removed", "timeout": up},
        {"wait": 1.0}, {"shot": "08_ejected"},
        # 6. Pulled after the eject, plugged again, pulled without one.
        {"qmp": "device_del", "args": {"id": "usbfat0"}},
        {"wait_for": "REMOVABLE:STATE MY_STICK removed", "timeout": up},
        {"wait": 1.0}, {"shot": "09_removed_after_eject"},
        fat.add_block_node("fat1", fat_img), fat.plug("fat1", "usbfat1"),
        {"wait_for": f"REMOVABLE:STATE MY_STICK mounted at {MOUNT}", "timeout": up,
         "occurrence": 2},
        {"wait": 1.0},
        {"qmp": "device_del", "args": {"id": "usbfat1"}},
        {"wait_for": "REMOVABLE:STATE MY_STICK was removed without ejecting", "timeout": up},
        {"wait": 1.0}, {"shot": "10_removed_unsafely"},
        {"quit": True},
    ]
    return steps


def judge(log: str) -> list[str]:
    failures: list[str] = []
    for pattern, why in [
        (r"REMOVABLE:UP:PASS", "the applet never started"),
        (rf"SHELL:TRAY:SET app={re.escape(APP)}", "the shell never took its item"),
        (r"REMOVABLE:OPEN /media/MY_STICK ok", "Open did not reach Files"),
        (r"REMOVABLE:EJECT usb\d+ busy", "the eject from inside the volume was not refused"),
        (r"REMOVABLE:EJECT usb\d+ ejecting", "the eject never went ahead"),
        (r"MOUNTD:VOLUME END usb\d+ gen=\d+ ejected", "the daemon did not end as ejected"),
        (r"FATFUSE:STOPPED clean", "no clean stop"),
        (r"REMOVABLE:STATE Cannot eject MY_STICK: .*working directory", "no holder named"),
        (r"REMOVABLE:STATE MY_STICK can be removed", "no `can be removed` notice"),
    ]:
        if not re.search(pattern, log):
            failures.append(why)
    denied = re.findall(rf"LABEL:DENY label=app:{re.escape(APP)}[^\n]*", log)
    if denied:
        failures.append(f"the applet was refused calls: {denied[:3]}")
    for pattern, why in [(r"FATFUSE:FAIL", "fatfuse failed"), (r"LazyOS PANIC", "a kernel panic")]:
        match = re.search(pattern, log)
        if match:
            failures.append(f"{why}: {log[match.start():match.start() + 120]!r}")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--image", type=Path, default=ROOT / "target/lazyos.img")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/removable")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--timeout", type=float, default=2400.0)
    args = parser.parse_args()
    slow = args.accel in ("none", "tcg")
    if not args.no_build and not build():
        print("removable harness: FAIL (build)")
        return 1
    args.out.mkdir(parents=True, exist_ok=True)
    stick = args.out / "fat.img"
    fat.make_fat_stick(stick, "removable")
    script = args.out / "removable.json"
    script.write_text(json.dumps(session(stick, slow), indent=1), encoding="utf-8")
    extra = ["-drive", f"if=none,id=d0,format=raw,file={args.image.resolve()}",
             "-device", "virtio-blk-pci,drive=d0,disable-modern=on",
             "-device", "qemu-xhci,id=xhci", "-no-shutdown",
             "-drive", f"if=none,id=fat0,format=raw,file={stick.resolve()}"]
    out = args.out / "run"
    # Only this run's screenshots: a session's out directory keeps the last.
    for old in list(out.glob("shot_*.png")) + [out / "serial.log", out / "summary.json"]:
        old.unlink(missing_ok=True)
    command_line = [sys.executable, str(ROOT / "tools/screenshot/qemu_session.py"),
                    "--accel", args.accel, "--timeout", str(args.timeout),
                    "--wait-timeout", str(args.timeout), "--out", str(out),
                    "--script", str(script), "--fail-on", FAIL_ON,
                    ] + [f"--extra-arg={arg}" for arg in extra]
    started = time.time()
    result = subprocess.run(command_line, cwd=ROOT, capture_output=True, text=True)
    print(f"removable: session {'ok' if result.returncode == 0 else 'FAILED'} "
          f"in {time.time() - started:.0f} s", flush=True)
    if result.returncode != 0:
        print(result.stdout[-2500:] + result.stderr[-2500:])
    log_file = out / "serial.log"
    log = log_file.read_text(encoding="utf-8", errors="replace") if log_file.exists() else ""
    failures = judge(log)
    if result.returncode != 0:
        failures.append("the session did not complete")
    info = fat.host_info(stick)
    print(f"stick: {info}")
    if "dirty=true" in info:
        failures.append("the volume was left marked dirty")
    clean, output = fat.fsck_fat(stick)
    if clean is False:
        failures.append("fsck.fat is not clean:\n" + output)
    elif clean is None:
        print(f"fsck.fat skipped: {output}")
    shots = sorted(out.glob("shot_*.png"))
    print(f"screenshots: {len(shots)} in {out}")
    for failure in failures:
        print(f"FAIL: {failure}")
    print("removable harness: " + ("FAIL" if failures else "PASS"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
