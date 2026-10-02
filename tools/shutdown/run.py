#!/usr/bin/env python3
"""Shut a desktop LazyOS down, boot it again and reboot it: the end-to-end
check of the orderly shutdown (docs/shutdown.md).

Two boots of one image:

1. **Shell power-off.** The Terminal writes a nonce to the session account's
   home on the OS volume (`/home/<name>`; nothing is under `/data` since F4),
   then types `shutdown`. QEMU (run with `-no-shutdown`) pauses at the
   power-off and the last frame, the shutting-down overlay, is captured.
2. **Menu reboot.** The Terminal reads the nonce back (it survived the stop)
   and finds the first boot's journal, then LazyShell's start menu
   "Restart..." is chosen and confirmed. The "Restarting..." overlay is
   captured once QEMU (`-no-reboot -no-shutdown`) pauses on the reset.

Each boot's serial log is judged by `judge.py` (which requires `logd` to have
persisted records to `/logs`, `confd` to keep its store in `/conf` and `pkgd`
to stop through the lifecycle contract before `confd`). The second boot must
also mount the volume clean (no "was not cleanly unmounted"), print the nonce
and find the first boot's records in `/logs/service.log` (its boot id, from
`LOGD:STORE:READY ... boot=<id>`): from the guest's Terminal (autostarted as
root) and, after QEMU exits, from the host (`libs/ext2fs`'s `osread`
example), which does not depend on who the Terminal runs as (`/logs` is 0750
root). Screenshots and logs land in `shots/shutdown/`.

    python tools/shutdown/run.py              # build the desktop image, boot twice, judge
    python tools/shutdown/run.py --no-build   # reuse target/lazyos.img
    python tools/shutdown/run.py --accel none # force TCG

The desktop image needs BusyBox (`target/abi/busybox/busybox`) and the xui
apps (`python tools/xui/build.py`, run here unless `--no-build`).
Exit status is non-zero on any failure.
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

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(Path(__file__).resolve().parent))

from judge import judge  # noqa: E402

IMAGE = ROOT / "target/lazyos.img"
BUSYBOX = ROOT / "target/abi/busybox/busybox"
#: The journal of `init`'s `system/events/service/*` records: every desktop
#: boot writes it (docs/architecture/userland.md, `logd`).
JOURNAL = "/logs/service.log"


def session_home() -> str:
    """The home of the desktop's session account (uid 1000) in the account file
    the build installs (`build_support/passwd`), so an account rename moves it too."""
    passwd = (ROOT / "build_support" / "passwd").read_text(encoding="utf-8")
    for row in passwd.splitlines():
        fields = row.split(":")
        if len(fields) >= 5 and fields[1] == "1000":
            return fields[4]
    raise SystemExit("build.rs embeds no uid 1000 account")


NOTE = f"{session_home()}/shutdown.txt"


def read_journal() -> str:
    """`JOURNAL` read from the image on the host: the session user may not."""
    result = subprocess.run(["cargo", "run", "-q", "-p", "ext2fs", "--example", "osread", "--",
                             str(IMAGE), "cat", JOURNAL],
                            cwd=ROOT, capture_output=True, text=True, errors="replace")
    if result.returncode != 0:
        print(result.stderr[-2000:])
    return result.stdout


def home(start: float) -> list[dict]:
    """Park the pointer in the top-left corner (moves are relative and clamped),
    from `start` seconds after the last gate."""
    return [{"at": start + 0.2 * n, "mouse_move": [-300, -300]} for n in range(6)]


def focus_terminal() -> list[dict]:
    """Wait for the desktop, then click into the Terminal's window (its
    taskbar entry would minimize it when it already has the focus)."""
    return [
        {"wait_for": "TERM:UP:PASS", "timeout": 240},
        {"wait_for": "INIT:AUTOSTART:PASS app=terminal", "timeout": 60},
        *home(2.0),
        {"at": 3.4, "mouse_move": [400, 58]},
        {"at": 3.8, "mouse_click": "left"},
        {"at": 5.0, "type": "echo ready"},
        {"at": 5.5, "key": "enter", "until": "TERM:OUT:ready", "timeout": 30, "retries": 2},
    ]


def final_frame(name: str) -> list[dict]:
    """Wait for the kernel's sync, then capture the last frame. QEMU runs with
    `-no-shutdown`, so the power-off (or, with `-no-reboot`, the reset) pauses
    the VM instead of ending it and the screen can still be read."""
    return [{"wait_for": "power: filesystems synced", "timeout": 60},
            {"at": 1.0, "shot": name}]


def command(text: str, until: str) -> list[dict]:
    return [{"at": 1.0, "type": text},
            {"at": 0.5, "key": "enter", "until": until, "timeout": 30, "retries": 2}]


def power_off_session(nonce: str) -> list[dict]:
    return [
        *focus_terminal(),
        *command(f"echo {nonce} > {NOTE}; echo wrote-$?", "TERM:OUT:wrote-0"),
        *command("shutdown", "INIT:SHUTDOWN:BEGIN"),
        {"wait_for": "XUID:POWER:OVERLAY", "timeout": 30},
        *final_frame("poweroff_overlay"),
    ]


def reboot_session(nonce: str, boot_id: str) -> list[dict]:
    # LazyShell's start button is at (44, 704); the menu is bottom-anchored on
    # the taskbar, so "Restart..." (the second-to-last row) is at (134, 648)
    # whatever the configured rows. The confirmation swaps the power rows in
    # place: "Restart now" lands under the pointer. Neither click retries, so
    # a missed marker can never turn into a second, confirming click.
    return [
        *focus_terminal(),
        *command(f"cat {NOTE}", f"TERM:OUT:{nonce}"),
        {"wait_for": f"TERM:OUT:{nonce}", "timeout": 30},
        # The first boot's records are in its journal: its boot line names it.
        # (The autostarted Terminal runs as root, session 0, so it may read
        # `/logs`, which is 0750 root; the harness also reads it from the host.)
        *command(f"grep -q id={boot_id} {JOURNAL}; echo journal-$?", "TERM:OUT:journal-"),
        *home(1.0),
        {"at": 2.4, "mouse_move": [44, 704]},
        {"at": 3.0, "mouse_click": "left", "until": "SHELL:MENU:OPEN", "timeout": 20,
         "retries": 2},
        # `wait`, not `at`: an `until` is no gate, so `at` would fire the
        # confirming click at once, and a second press that close is a double
        # click, which the confirmation row ignores on purpose.
        {"wait": 0.5},
        {"mouse_move": [90, -56]},
        {"wait": 1.0},
        {"shot": "menu_power_rows"},
        {"mouse_click": "left", "until": "SHELL:POWER:CONFIRM", "timeout": 20},
        {"wait": 1.5},
        {"shot": "menu_confirm"},
        {"mouse_click": "left", "until": "SHELL:POWER:REQUEST mode=1", "timeout": 20},
        {"wait_for": "INIT:SHUTDOWN:BEGIN", "timeout": 30},
        {"wait_for": "XUID:POWER:OVERLAY", "timeout": 30},
        *final_frame("reboot_overlay"),
    ]


def build() -> bool:
    if not BUSYBOX.is_file():
        print(f"missing {BUSYBOX}: run tools/abi/busybox.py (the Terminal needs sh)")
        return False
    steps = [[sys.executable, "tools/xui/build.py"], ["cargo", "build"]]
    env = dict(os.environ, LAZYOS_DESKTOP="1")
    return all(subprocess.run(step, cwd=ROOT, env=env).returncode == 0 for step in steps)


def boot(name: str, steps: list[dict], out: Path, accel: str) -> tuple[bool, str]:
    """Run one session; returns whether it completed and its serial log."""
    out.mkdir(parents=True, exist_ok=True)
    script = out / f"{name}.json"
    script.write_text(json.dumps(steps, indent=1), encoding="utf-8")
    session = out / name
    started = time.time()
    result = subprocess.run([sys.executable, "tools/screenshot/qemu_session.py",
                             "--image", str(IMAGE),
                             "--accel", accel, "--out", str(session), "--script", str(script),
                             "--extra-arg=-no-shutdown"],
                            cwd=ROOT, capture_output=True, text=True)
    print(f"{name}: session {'ok' if result.returncode == 0 else 'FAILED'} "
          f"in {time.time() - started:.0f} s")
    if result.returncode != 0:
        print(result.stderr[-3000:])
    log = (session / "serial.log").read_text(encoding="utf-8", errors="replace")
    return result.returncode == 0, log


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/shutdown")
    args = parser.parse_args()
    if not args.no_build and not build():
        return 1
    nonce = f"persist-{int(time.time())}"
    failures: list[str] = []

    ok, log = boot("poweroff", power_off_session(nonce), args.out, args.accel)
    failures += [] if ok else ["power-off session did not complete"]
    failures += [f"power-off: {f}" for f in judge(log, "poweroff")]

    boot_id = re.search(r"LOGD:STORE:READY \S+ boot=([0-9a-f]{16})", log)
    if boot_id is None:
        failures.append("power-off: logd's journals were not ready (no LOGD:STORE:READY)")
    boot_id = boot_id.group(1) if boot_id else "0" * 16

    ok, log = boot("reboot", reboot_session(nonce, boot_id), args.out, args.accel)
    failures += [] if ok else ["reboot session did not complete"]
    failures += [f"reboot: {f}" for f in judge(log, "reboot")]
    if "was not cleanly unmounted" in log:
        failures.append("the OS volume was not clean after the power-off")
    if f"TERM:OUT:{nonce}" not in log:
        failures.append(f"{NOTE} did not survive the power-off")
    if "TERM:OUT:journal-0" not in log:
        failures.append(f"{JOURNAL} lost the first boot's records")
    if f"id={boot_id}" not in read_journal():
        failures.append(f"{JOURNAL} read from the image lacks the first boot's records")
    if "SHELL:POWER:REQUEST mode=1 phase=" not in log:
        failures.append("LazyShell's start menu did not request the reboot")
    if "INIT:LAUNCH:PASS app=lazyshell" in log.split("INIT:SHUTDOWN:BEGIN", 1)[-1]:
        failures.append("LazyShell was started again during the shutdown")

    for failure in failures:
        print(f"FAIL: {failure}")
    print("SHUTDOWN: PASS" if not failures else f"SHUTDOWN: {len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
