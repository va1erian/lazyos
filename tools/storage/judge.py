#!/usr/bin/env python3
"""Judge one boot of the USB storage harness from its serial log
(docs/architecture/usb-storage.md).

    python tools/storage/judge.py serial.log --nonce N [--second] [--other]

A boot passes when:

* `usbd` served the home stick (`USBD:MSC:DISK ... id=usb<n>`) and, with
  `--other`, a second stick too, and printed no `USBD:FATAL`, `USBD:PANIC`
  or `USBD:MSC:FAIL`;
* the kernel mounted the home volume late at `/home` exactly once
  (`fs: mounted usb<n>p1 at /home (late, home volume lazyhome)`), so the other
  stick stayed unmounted, and `init` stopped waiting because it was mounted
  (`INIT:HOME mounted`);
* the console session read the nonce back from `/home/alice`
  (`cat` printed the nonce on a line of its own);
* the machine powered off in order: `INIT:SHUTDOWN:BEGIN`, then
  `power: filesystems synced`, never `power: sync failed`;
* no kernel panic; and on `--second` (the boot after a clean power-off) the
  stick's volume was not found `not cleanly unmounted` (the root volume is
  not judged: ext2 never launders a volume that arrived unclean, so one
  interrupted run leaves `/` dirty for good).

Exit status is non-zero on any failure; each failure is one line.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

DISK = re.compile(r"USBD:MSC:DISK port=\d+ slot=\d+ id=(usb\d+)")
LATE = re.compile(r"fs: mounted (usb\d+)p1 at /home \(late, home volume lazyhome\)")
FORBIDDEN = [
    ("USBD:FATAL", "usbd failed"),
    ("USBD:PANIC", "usbd panicked"),
    ("USBD:MSC:FAIL", "a stick failed"),
    ("USBD:PORT:FAIL", "a port failed"),
    ("LazyOS PANIC", "the kernel panicked"),
    ("power: sync failed", "the power-off sync failed"),
]


def judge(text: str, nonce: str, second: bool = False, other: bool = False) -> list[str]:
    failures = []
    disks = DISK.findall(text)
    want = 2 if other else 1
    if len(disks) < want:
        failures.append(f"usbd served {len(disks)} stick(s), expected {want}")
    mounts = LATE.findall(text)
    if len(mounts) != 1:
        failures.append(f"/home was mounted late {len(mounts)} time(s), expected once")
    elif mounts[0] not in disks:
        failures.append(f"/home is on {mounts[0]}, which usbd never registered")
    if "INIT:HOME mounted" not in text:
        failures.append("init did not see /home mounted (INIT:HOME mounted)")
    if "LOGIN:OK:PASS user=alice" not in text:
        failures.append("the console login failed")
    if not re.search(rf"(?m)^{re.escape(nonce)}\r?$", text):
        failures.append(f"the session did not read the nonce {nonce} back")
    begin = text.find("INIT:SHUTDOWN:BEGIN")
    synced = text.find("power: filesystems synced")
    if begin < 0:
        failures.append("no orderly shutdown (INIT:SHUTDOWN:BEGIN)")
    elif synced < begin:
        failures.append("the filesystems were not synced after the shutdown began")
    for marker, why in FORBIDDEN:
        if marker in text:
            failures.append(f"{why} ({marker})")
    if second and re.search(r"usb\d+p\d+ was not cleanly unmounted", text):
        failures.append("the stick was not cleanly unmounted after a clean power-off")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("serial_log", type=Path)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--second", action="store_true", help="the boot after a power-off")
    parser.add_argument("--other", action="store_true", help="a second, non-home stick")
    args = parser.parse_args()
    text = args.serial_log.read_text(encoding="utf-8", errors="replace")
    failures = judge(text, args.nonce, args.second, args.other)
    for failure in failures:
        print(f"FAIL: {failure}")
    print("storage judge: " + ("FAIL" if failures else "PASS"))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
