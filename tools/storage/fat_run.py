#!/usr/bin/env python3
"""A stock FAT stick is mounted when it is plugged in, usable as a directory,
gone when it is pulled, and back when it is plugged in again
(docs/usb-hotplug-plan.md; the FAT counterpart of `run.py`).

QEMU runs the image on a virtio-blk boot disk with `/home` on an ext2
`lazyhome` stick, as `run.py` does, and a second USB stick that is **not**
attached at boot: a 64 MiB FAT32 disk (MBR, one partition, label
`MY STICK`) made by `libs/fatvol`'s `fatstick` with a file on it. The session:

1. logs in on the console as `user`, `/media` is empty;
2. plugs the FAT stick in over QMP (`device_add`): `fatfuse` mounts it at
   `/media/MY_STICK` and the file reads back;
3. writes a file, makes a directory, copies a file into it, lists, then
   waits for the volume to settle;
4. pulls the stick (`device_del`): the mount goes, `ls /media` is empty, the
   shell answers;
5. plugs it in again: the same files are there, including what step 3
   wrote, and the volume was clean when it was pulled (no `dirty` mark);
6. powers off.

The host then reads the stick image with the same library: both written
files are on it with the right contents, and `fsck.fat -n` (Docker, when
available) finds nothing wrong.

    python tools/storage/fat_run.py               # build, boot, judge
    python tools/storage/fat_run.py --no-build    # reuse target/lazyos.img
    python tools/storage/fat_run.py --accel none  # force TCG (slow)
"""

from __future__ import annotations

import argparse
import json
import re
import secrets
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))

import run as storage  # noqa: E402
import stick  # noqa: E402

LABEL = "MY STICK"
MOUNT = "/media/MY_STICK"
FAIL_ON = r"USBD:(FATAL|PANIC)|LazyOS PANIC|FATFUSE:FAIL panic"


def fatstick() -> list[str]:
    """The host tool, built on demand (`cargo run -p fatvol --example fatstick`)."""
    return ["cargo", "run", "-q", "-p", "fatvol", "--example", "fatstick", "--"]


def make_fat_stick(path: Path, nonce: str) -> None:
    command = fatstick() + ["make", str(path), "--size", "64M", "--label", LABEL,
                            "--file", f"hello.txt=hello-{nonce}\n",
                            "--file", "docs/readme.md=# a stock stick\n", "--dir", "docs"]
    subprocess.run(command, cwd=ROOT, check=True)


def host_cat(image: Path, name: str) -> str | None:
    result = subprocess.run(fatstick() + ["cat", str(image), name], cwd=ROOT,
                            capture_output=True, text=True)
    return result.stdout if result.returncode == 0 else None


def host_info(image: Path) -> str:
    result = subprocess.run(fatstick() + ["info", str(image)], cwd=ROOT,
                            capture_output=True, text=True)
    return (result.stdout + result.stderr).strip()


def fsck_fat(image: Path) -> tuple[bool | None, str]:
    """`fsck.fat -n` on the stick's partition through Docker; None if unavailable."""
    if not shutil.which("docker"):
        return None, "docker not installed"
    data = image.read_bytes()
    start = int.from_bytes(data[446 + 8:446 + 12], "little") * 512
    count = int.from_bytes(data[446 + 12:446 + 16], "little") * 512
    part = image.with_suffix(".part")
    part.write_bytes(data[start:start + count])
    try:
        result = subprocess.run(
            ["docker", "run", "--rm", "-v", f"{part.parent.resolve()}:/w", "alpine:3.20", "sh",
             "-c", f"apk add -q dosfstools >/dev/null 2>&1 && fsck.fat -n /w/{part.name}"],
            capture_output=True, text=True, timeout=300)
    except (subprocess.TimeoutExpired, OSError) as error:
        return None, str(error)
    finally:
        part.unlink(missing_ok=True)
    text = result.stdout + result.stderr
    if "apk" in text and "ERROR" in text and "fsck.fat" not in text:
        return None, text.strip()
    return result.returncode == 0, text.strip()


def plug(drive_id: str, device_id: str) -> dict:
    return {"qmp": "device_add",
            "args": {"driver": "usb-storage", "bus": "xhci.0", "drive": drive_id,
                     "id": device_id}}


def add_block_node(name: str, image: Path) -> dict:
    """QMP `blockdev-add` of `image` as node `name`: QEMU drops a `-drive`
    when its device is pulled, so a stick that is plugged in again needs a
    fresh node."""
    return {"qmp": "blockdev-add",
            "args": {"driver": "raw", "node-name": name,
                     "file": {"driver": "file", "filename": str(image.resolve())}}}


def session(nonce: str, written: str, slow: bool, fat: Path) -> list[dict]:
    step = 120 if slow else 30
    up = 600 if slow else 90
    steps = storage.login(slow)
    steps += storage.command("ls /media | wc -l", r"(?m)^0\r?$", step)
    steps += [{"wait": 1.0}, plug("fat0", "usbfat0"),
              {"wait_for": f"FATFUSE:UP {MOUNT}", "timeout": up}]
    steps += storage.command(f"cat {MOUNT}/hello.txt", rf"(?m)^hello-{nonce}\r?$", step)
    steps += storage.command(f"echo {written} > {MOUNT}/written.txt; echo wrote-$?",
                             r"(?m)^wrote-0", step)
    steps += storage.command(f"mkdir {MOUNT}/newdir; echo mkdir-$?", r"(?m)^mkdir-0", step)
    steps += storage.command(f"cp {MOUNT}/hello.txt {MOUNT}/newdir/copy.txt; echo cp-$?",
                             r"(?m)^cp-0", step)
    steps += storage.command(f"cat {MOUNT}/newdir/copy.txt", rf"(?m)^hello-{nonce}\r?$", step)
    steps += storage.command(f"ls {MOUNT}", r"written\.txt", step)
    # Idle: the daemon settles (clean flag, FSInfo) a second after the last
    # request. Then pull the stick.
    steps += [{"wait": 6.0 if not slow else 20.0},
              {"qmp": "device_del", "args": {"id": "usbfat0"}},
              {"wait_for": f"fuse: {MOUNT} unmounted", "timeout": step}]
    # The mount goes (the kernel reaps the dead provider within seconds), and
    # the system answers meanwhile.
    steps += storage.command("ls / > /dev/null; echo alive-$?", r"(?m)^alive-0", step)
    steps += storage.command("sleep 8; ls /media | wc -l", r"(?m)^0\r?$", step + 30)
    # Plugged in again: a new generation, mounted the same way.
    steps += [add_block_node("fat1", fat), plug("fat1", "usbfat1"),
              {"wait_for": f"FATFUSE:UP {MOUNT}", "timeout": up, "occurrence": 2}]
    steps += storage.command(f"cat {MOUNT}/written.txt", rf"(?m)^{written}\r?$", step)
    steps += storage.command(f"cat {MOUNT}/newdir/copy.txt", rf"(?m)^hello-{nonce}\r?$", step)
    steps += storage.command("poweroff", "INIT:SHUTDOWN:BEGIN", step)
    steps += [{"wait_for": "power: filesystems synced", "timeout": 600 if slow else 120},
              {"wait": 1.0}, {"quit": True}]
    return steps


def judge(log: str) -> list[str]:
    failures: list[str] = []
    ups = re.findall(rf"FATFUSE:UP {re.escape(MOUNT)} disk=usb\d+ fat=32 rw", log)
    if len(ups) != 2:
        failures.append(f"expected the FAT volume mounted twice (fat=32 rw), saw {len(ups)}")
    if "MOUNTD:VOLUME START" not in log:
        failures.append("mountd never started a volume daemon")
    ended = re.findall(r"MOUNTD:VOLUME END usb\d+ gen=\d+ (.*)", log)
    # A pulled stick ends its daemon one of two ways, whichever comes first:
    # the daemon's next request fails (`removed`), or `mountd` sees the disk
    # leave the list and stops it (`MOUNTD:VOLUME STOP`).
    if "MOUNTD:VOLUME STOP" not in log and not any("removed" in line for line in ended):
        failures.append(f"the pulled stick's daemon was neither stopped nor ended: {ended}")
    if f"fuse: {MOUNT} unmounted" not in log:
        failures.append("the pulled stick's mount was never taken out")
    # The home stick is the kernel's (`/home` is mounted through it): no
    # daemon is started for it, and it is never offered for a raw claim.
    if "MOUNTD:VOLUME START usb0 " in log:
        failures.append("a volume daemon was started for the home stick")
    # The pulled stick's slot is the next plug's: generation 2 of the same id.
    slots = re.findall(r"block: (usb\d+) registered by task \d+: \d+ sectors \(rw\), generation (\d+)", log)
    reused = [(name, int(gen)) for name, gen in slots if int(gen) >= 2]
    if not reused:
        failures.append(f"the replugged stick did not reuse its disk slot: {slots}")
    for pattern, why in [
        (r"FATFUSE:FAIL", "fatfuse failed"),
        (r"LazyOS PANIC", "a kernel panic"),
        (r"power: sync failed", "the final sync failed"),
        (r"block: usb\d+: provider stopped answering", "a disk's provider hung"),
    ]:
        match = re.search(pattern, log)
        if match:
            failures.append(f"{why}: {log[match.start():match.start() + 120]!r}")
    if not re.search(r"INIT:HOME mounted", log):
        failures.append("/home was not mounted")
    return failures


def boot(args: argparse.Namespace, home: Path, fat: Path, steps: list[dict]) -> str:
    out = args.out / "fat"
    out.mkdir(parents=True, exist_ok=True)
    script = args.out / "fat.json"
    script.write_text(json.dumps(steps, indent=1), encoding="utf-8")
    extra = ["-drive", f"if=none,id=d0,format=raw,file={args.image.resolve()}",
             "-device", "virtio-blk-pci,drive=d0,disable-modern=on",
             "-device", "qemu-xhci,id=xhci", "-no-shutdown",
             "-drive", f"if=none,id=stick0,format=raw,file={home.resolve()}",
             "-device", "usb-storage,bus=xhci.0,drive=stick0,id=usbstick0",
             # The FAT stick's drive exists from the start; its device is
             # plugged in by the session (QMP `device_add`).
             "-drive", f"if=none,id=fat0,format=raw,file={fat.resolve()}"]
    command_line = [sys.executable, str(ROOT / "tools/screenshot/qemu_session.py"),
                    "--accel", args.accel, "--timeout", str(args.timeout),
                    "--wait-timeout", str(args.timeout),
                    "--out", str(out), "--script", str(script), "--fail-on", FAIL_ON,
                    ] + [f"--extra-arg={arg}" for arg in extra]
    started = time.time()
    result = subprocess.run(command_line, cwd=ROOT, capture_output=True, text=True)
    print(f"fat: session {'ok' if result.returncode == 0 else 'FAILED'} "
          f"in {time.time() - started:.0f} s", flush=True)
    if result.returncode != 0:
        print(result.stdout[-2500:] + result.stderr[-2500:])
    log = out / "serial.log"
    return log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--image", type=Path, default=ROOT / "target/lazyos.img")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/storage")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--timeout", type=float, default=2400.0)
    args = parser.parse_args()
    slow = args.accel in ("none", "tcg")
    if not args.no_build and not storage.build():
        print("fat harness: FAIL (build)")
        return 1
    args.out.mkdir(parents=True, exist_ok=True)
    home = args.out / "stick.img"
    stick.make_stick(home)
    nonce = secrets.token_hex(6)
    written = "w" + secrets.token_hex(6)
    fat = args.out / "fat.img"
    make_fat_stick(fat, nonce)
    log = boot(args, home, fat, session(nonce, written, slow, fat))
    failures = judge(log)
    for name, want in [("written.txt", written), ("hello.txt", f"hello-{nonce}"),
                       ("newdir/copy.txt", f"hello-{nonce}")]:
        got = host_cat(fat, name)
        if got is None or got.strip() != (want if name != "hello.txt" else want):
            failures.append(f"the stick holds {got!r} in {name}, wanted {want!r}")
    info = host_info(fat)
    print(f"stick: {info}")
    if "dirty=true" in info:
        failures.append("the volume was left marked dirty")
    clean, output = fsck_fat(fat)
    if clean is False:
        failures.append("fsck.fat is not clean:\n" + output)
    elif clean is None:
        print(f"fsck.fat skipped: {output}")
    else:
        print(f"fsck.fat clean: {output.splitlines()[-1]}")
    for failure in failures:
        print(f"FAIL: {failure}")
    print("fat harness: " + ("FAIL" if failures else "PASS"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
