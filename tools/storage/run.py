#!/usr/bin/env python3
"""Keep `/home` on a USB stick across a power-off: the end-to-end check of
USB storage (docs/architecture/usb-storage.md).

QEMU runs with the image on a virtio-blk boot disk, **no** home disk, and a
`qemu-xhci` controller holding a `usb-storage` stick: an MBR disk whose one
partition is an ext2 volume labelled `lazyhome` with `/alice` on it
(`stick.py`). With `--other` (the default) a second stick, labelled
`otherdisk`, is plugged in too and must be served but never mounted.

1. **Boot 1.** `usbd` serves the stick, the kernel mounts it late at
   `/home`, `init` lets `logind` start. The harness logs in on the console as
   `alice`, writes a nonce to `/home/alice/usbnote`, reads it back and runs
   `poweroff`. QEMU (`-no-shutdown`) pauses once the kernel has synced.
2. **Boot 2.** Same stick: the volume must mount clean; the session reads the
   nonce back, writes a second file and powers off again.
3. **Host.** `e2fsck -fn` on the stick's partition is clean and `debugfs`
   finds both files with the right contents.
4. **Surprise removal** (unless `--no-unplug`), on a copy of the stick: log
   in, write, pull the stick out over QMP (`device_del`). `usbd` reports it
   gone, a write under `/home` fails instead of hanging, the shell and the
   rest of the system still answer, and `poweroff` still powers off.

    python tools/storage/run.py              # build, make the sticks, boot three times, judge
    python tools/storage/run.py --no-build   # reuse target/lazyos.img
    python tools/storage/run.py --no-other   # only the home stick
    python tools/storage/run.py --no-unplug  # skip the surprise removal
    python tools/storage/run.py --accel none # force TCG (slow: allow ~30 min)

The image is built with `LAZYOS_SERVICES=1 LAZYOS_USB=1 LAZYOS_RESET_OS=1`
and needs BusyBox for the console shell: `LAZYOS_BUSYBOX`, or
`target/abi/busybox/busybox` (`tools/abi/busybox.py`).
Exit status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import json
import os
import secrets
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))

import stick  # noqa: E402
from judge import judge, judge_unplug  # noqa: E402

BUSYBOX = ROOT / "target/abi/busybox/busybox"
NOTE = "/home/alice/usbnote"
SECOND = "/home/alice/second"
FAIL_ON = r"USBD:(FATAL|PANIC)|LazyOS PANIC"


def build() -> bool:
    busybox = os.environ.get("LAZYOS_BUSYBOX") or str(BUSYBOX)
    if not Path(busybox).is_file():
        print(f"missing {busybox}: run tools/abi/busybox.py or set LAZYOS_BUSYBOX")
        return False
    env = dict(os.environ, LAZYOS_SERVICES="1", LAZYOS_USB="1", LAZYOS_RESET_OS="1",
               LAZYOS_BUSYBOX=busybox)
    print("building: LAZYOS_SERVICES=1 LAZYOS_USB=1 LAZYOS_RESET_OS=1 cargo build", flush=True)
    return subprocess.run(["cargo", "build"], cwd=ROOT, env=env).returncode == 0


#: Characters typed at once. A guest busy under TCG drains QEMU's 16-byte
#: i8042 queue slowly, and a whole command line typed in one go loses keys.
CHUNK = 3


def typed(text: str) -> list[dict]:
    """`text` typed a few characters at a time, with pauses between."""
    steps: list[dict] = []
    for start in range(0, len(text), CHUNK):
        steps += [{"type": text[start:start + CHUNK]}, {"wait": 0.4}]
    return steps


def command(text: str, until: str, timeout: float) -> list[dict]:
    """Type a shell line and press Enter until `until` (a regex) shows up
    (a retry presses Enter on an empty line, which is harmless)."""
    return [{"wait": 3.0}, *typed(text), {"wait": 1.0},
            {"key": "enter", "until": until, "regex": True, "timeout": timeout, "retries": 1}]


def login(slow: bool) -> list[dict]:
    """Wait for `/home`, then log in on the console as `alice`."""
    late = 900 if slow else 240
    step = 120 if slow else 30
    return [
        {"wait_for": "INIT:HOME mounted", "timeout": late},
        {"wait_for": "LazyOS login: ", "timeout": late},
        {"wait": 3.0},
        {"type": "alice"}, {"wait": 0.6}, {"key": "enter"},
        {"wait": 1.0},
        {"type": "lazy"}, {"wait": 0.6},
        {"key": "enter", "until": "LOGIN:OK:PASS user=alice", "timeout": step, "retries": 1},
        # Let the boot's remaining services settle: their output repaints the
        # console, which is when typed keys get lost.
        {"wait": 30.0 if slow else 2.0},
    ]


def session(nonce: str, second: bool, slow: bool) -> list[dict]:
    step = 120 if slow else 30
    steps = login(slow)
    if second:
        steps += command(f"cat {NOTE}", rf"(?m)^{nonce}\r?$", step)
        steps += command(f"echo again > {SECOND}; echo wrote-$?", r"(?m)^wrote-0", step)
    else:
        steps += command(f"echo {nonce} > {NOTE}; echo wrote-$?", r"(?m)^wrote-0", step)
        steps += command(f"cat {NOTE}", rf"(?m)^{nonce}\r?$", step)
    steps += command("poweroff", "INIT:SHUTDOWN:BEGIN", step)
    steps += [{"wait_for": "power: filesystems synced", "timeout": 600 if slow else 120},
              {"wait": 1.0}, {"quit": True}]
    return steps


def unplug_session(nonce: str, slow: bool) -> list[dict]:
    """Write, pull the stick out, prove nothing hangs, power off."""
    step = 120 if slow else 30
    steps = login(slow)
    steps += command(f"echo {nonce} > /home/alice/unplug; echo wrote-$?", r"(?m)^wrote-0", step)
    steps += [{"wait": 5.0},
              {"qmp": "device_del", "args": {"id": "usbstick0"}},
              {"wait_for": "USBD:MSC:GONE", "timeout": step}]
    # Any status will do: the point is that the write returns.
    steps += command("echo late > /home/alice/late; echo after-$?", r"(?m)^after-\d+", step)
    steps += command("ls / > /dev/null; echo alive-$?", r"(?m)^alive-0", step)
    steps += command("poweroff", "INIT:SHUTDOWN:BEGIN", step)
    steps += [{"wait_for": "power: (filesystems synced|sync failed)", "regex": True,
               "timeout": 600 if slow else 120},
              {"wait": 1.0}, {"quit": True}]
    return steps


def boot(name: str, steps: list[dict], args: argparse.Namespace, sticks: list[Path]) -> str:
    out = args.out / name
    out.mkdir(parents=True, exist_ok=True)
    script = args.out / f"{name}.json"
    script.write_text(json.dumps(steps, indent=1), encoding="utf-8")
    extra = ["-drive", f"if=none,id=d0,format=raw,file={args.image.resolve()}",
             "-device", "virtio-blk-pci,drive=d0,disable-modern=on",
             "-device", "qemu-xhci,id=xhci", "-no-shutdown"]
    for index, path in enumerate(sticks):
        extra += ["-drive", f"if=none,id=stick{index},format=raw,file={path.resolve()}",
                  "-device", f"usb-storage,bus=xhci.0,drive=stick{index},id=usbstick{index}"]
    command_line = [sys.executable, str(ROOT / "tools/screenshot/qemu_session.py"),
                    "--accel", args.accel, "--timeout", str(args.timeout),
                    "--wait-timeout", str(args.timeout),
                    "--out", str(out), "--script", str(script), "--fail-on", FAIL_ON,
                    ] + [f"--extra-arg={arg}" for arg in extra]
    started = time.time()
    result = subprocess.run(command_line, cwd=ROOT, capture_output=True, text=True)
    print(f"{name}: session {'ok' if result.returncode == 0 else 'FAILED'} "
          f"in {time.time() - started:.0f} s", flush=True)
    if result.returncode != 0:
        print(result.stdout[-2000:] + result.stderr[-2000:])
    log = out / "serial.log"
    return log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""


def verdict(name: str, failures: list[str]) -> bool:
    for failure in failures:
        print(f"FAIL: {name}: {failure}")
    print(f"{name}: {'FAIL' if failures else 'PASS'}", flush=True)
    return not failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--image", type=Path, default=ROOT / "target/lazyos.img")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/storage")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--timeout", type=float, default=2400.0)
    parser.add_argument("--no-other", action="store_true", help="only the home stick")
    parser.add_argument("--no-unplug", action="store_true", help="skip the surprise removal")
    args = parser.parse_args()
    slow = args.accel in ("none", "tcg")
    if not args.no_build and not build():
        print("storage harness: FAIL (build)")
        return 1
    args.out.mkdir(parents=True, exist_ok=True)
    home = args.out / "stick.img"
    stick.make_stick(home)
    sticks = [home]
    if not args.no_other:
        other = args.out / "other.img"
        stick.make_stick(other, label="otherdisk")
        sticks.append(other)
    nonce = secrets.token_hex(6)
    other = not args.no_other
    ok = True
    log = boot("boot1", session(nonce, False, slow), args, sticks)
    ok &= verdict("boot1", judge(log, nonce, False, other))
    if ok:
        log = boot("boot2", session(nonce, True, slow), args, sticks)
        ok &= verdict("boot2", judge(log, nonce, True, other))
    host = []
    clean, output = stick.fsck(home)
    if not clean:
        host.append("e2fsck -fn is not clean:\n" + output.strip())
    if (stick.read_file(home, "/alice/usbnote") or "").strip() != nonce:
        host.append("the stick does not hold the nonce in /alice/usbnote")
    if ok and (stick.read_file(home, "/alice/second") or "").strip() != "again":
        host.append("the stick does not hold /alice/second")
    ok &= verdict("host", host)
    if ok and not args.no_unplug:
        # A copy: the removal leaves the volume dirty, and the host checks
        # above are about the clean power-offs.
        pulled = args.out / "unplug.img"
        pulled.write_bytes(home.read_bytes())
        log = boot("unplug", unplug_session(nonce, slow), args, [pulled])
        ok &= verdict("unplug", judge_unplug(log))
    print("storage harness: " + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
