#!/usr/bin/env python3
"""Judge an orderly shutdown or reboot from a LazyOS serial log.

docs/shutdown.md. The markers are printed by `init` (`INIT:SHUTDOWN:*`), the
persistence services (`CONFD:STOP`, `LOGD:STOP`, `PKGD:STOP`) and the kernel
(`power: ...`);
this checks they all appear, in the order the sequence promises, and that
nothing went wrong on the way:

* `init` armed the kernel watchdog and it never had to fire;
* the phases ran in order: stopping, apps, services, quiesced, power;
* `confd` synced and `logd`'s hash chain verified, both inside the services
  phase, and (on a desktop boot, the default) `logd` persisted records to its
  journals in `/logs` (`LOGD:STOP ... persisted=<n>`, n > 0) and `confd`'s
  store is `/conf` (`CONFD:STOP dir=/conf`);
* when `pkgd` ran, it stopped through the lifecycle contract inside the
  services phase, synced `/logs/pkg.log` (`PKGD:STOP sync=ok|none`) and did so
  before `confd`, which it depends on;
* when the session ran the Volume applet, the apps phase sent it `Quit` on
  its lifecycle channel and it quit there (`VOLUME:QUIT:PASS`, issue #651);
* nothing was killed at a deadline, nothing restarted after the request;
* the kernel synced the filesystems and did not fall back (no "8042 reset
  ignored", no "no ACPI power-off").

    python tools/shutdown/judge.py shots/shutdown/serial.log --mode poweroff
    python tools/shutdown/judge.py serial.log --console   # no /logs requirement

Exit status is non-zero on any failure.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

#: The sequence every orderly stop prints, in order.
ORDERED = [
    "INIT:SHUTDOWN:BEGIN",
    "power: watchdog armed",
    "INIT:SHUTDOWN:PHASE stopping",
    "INIT:SHUTDOWN:PHASE apps",
    "INIT:SHUTDOWN:PHASE services",
    "INIT:SHUTDOWN:QUIESCED",
    "INIT:SHUTDOWN:PHASE power",
]

#: Printed between the services phase and the quiesced marker, in any order.
PERSIST = ["CONFD:STOP", "LOGD:STOP"]

#: Lines that must never appear once the shutdown started.
FORBIDDEN = [
    ("INIT:SHUTDOWN:FAIL", "init's power call was refused"),
    ("power: watchdog expired", "the kernel watchdog had to force the stop"),
    ("killed (stop deadline)", "a program ignored its stop and was killed"),
    ("not reaped after SIGKILL", "a killed program was never reaped"),
    ("INIT:RESTART:PASS", "a service was restarted during the shutdown"),
    ("dependency order stalled", "the stop order had to be relaxed"),
    ("power: sync failed", "the filesystems did not sync"),
    ("8042 reset ignored", "the 8042 did not reset the machine"),
    ("no ACPI power-off", "the ACPI power-off did not stop the machine"),
]


def kernel_tail(mode: str) -> list[str]:
    """The kernel's last lines for `mode` (`poweroff` or `reboot`)."""
    verb = "reboot" if mode == "reboot" else "shutdown"
    return [f"INIT:SHUTDOWN:POWER mode={mode}", f"power: {verb} requested",
            "power: filesystems synced"]


def judge(log: str, mode: str, desktop: bool = True) -> list[str]:
    """Every failure found in `log` for a stop with `mode`; empty when it passed.

    `desktop` (an image with the ext2 OS volume, where `/logs` is writable)
    also requires `logd` to have persisted records."""
    failures: list[str] = []
    begin = log.find(ORDERED[0])
    if begin < 0:
        return [f"no {ORDERED[0]}: the shutdown never started"]
    tail = log[begin:]
    position = 0
    for marker in ORDERED + kernel_tail(mode):
        found = tail.find(marker, position)
        if found < 0:
            where = "at all" if marker not in tail else "in order"
            failures.append(f"missing {marker!r} {where}")
            continue
        position = found
    services = tail.find("INIT:SHUTDOWN:PHASE services")
    quiesced = tail.find("INIT:SHUTDOWN:QUIESCED")
    persist = PERSIST + (["PKGD:STOP"] if "init: started pkgd" in log else [])
    for marker in persist:
        found = tail.find(marker)
        if found < 0:
            failures.append(f"missing {marker!r}")
        elif not services < found < quiesced:
            failures.append(f"{marker} outside the services phase")
    if "init: started pkgd" in log:
        if "init: stopping pkgd (lifecycle)" not in tail:
            failures.append("pkgd was not stopped through the lifecycle contract")
        if 0 <= tail.find("CONFD:STOP") < tail.find("PKGD:STOP"):
            failures.append("pkgd stopped after confd, which it depends on")
        if (match := re.search(r"PKGD:STOP sync=(\S+)", tail)) and match.group(1) not in ("ok", "none"):
            failures.append(f"pkgd's final sync of pkg.log failed ({match.group(1)})")
    failures += judge_volume_quit(log, tail)
    if (match := re.search(r"CONFD:STOP \S+ sync=(\S+)", tail)) and match.group(1) != "ok":
        failures.append(f"confd's final sync failed ({match.group(1)})")
    if (match := re.search(r"LOGD:STOP records=\d+ verified=(\S+)", tail)) and \
            match.group(1) != "true":
        failures.append("logd's hash chain did not verify")
    if desktop:
        if (match := re.search(r"CONFD:STOP dir=(\S+)", tail)) and match.group(1) != "/conf":
            failures.append(f"confd's store is {match.group(1)}, not /conf")
        match = re.search(r"LOGD:STOP records=\d+ verified=\S+ persisted=(\d+)", tail)
        if match is None:
            failures.append("LOGD:STOP does not report persisted=<n>")
        elif int(match.group(1)) == 0:
            failures.append("logd persisted no records to /logs")
    if (match := re.search(r"INIT:SHUTDOWN:QUIESCED killed=(\d+)", tail)) and \
            match.group(1) != "0":
        failures.append(f"{match.group(1)} program(s) had to be killed")
    for line, why in FORBIDDEN:
        if line in tail:
            failures.append(f"{why} ({line!r})")
    return failures


def judge_volume_quit(log: str, tail: str) -> list[str]:
    """When the Volume applet ran (it opens with every desktop session and
    watches its lifecycle), the apps stage must have asked it to quit and it
    must have quit inside that stage (issue #651), not been killed."""
    before = log[: len(log) - len(tail)]
    if "INIT:APP:WATCH app=os.lazy.volume" not in before:
        return []
    apps = tail.find("INIT:SHUTDOWN:PHASE apps")
    services = tail.find("INIT:SHUTDOWN:PHASE services")
    failures = []
    if "INIT:APP:QUIT:SENT app=os.lazy.volume" not in tail:
        failures.append("the Volume applet was not sent Quit on its lifecycle channel")
    quit_at = tail.find("VOLUME:QUIT:PASS")
    if quit_at < 0:
        failures.append("the Volume applet never quit (no VOLUME:QUIT:PASS)")
    elif not apps < quit_at < (services if services >= 0 else len(tail)):
        failures.append("VOLUME:QUIT:PASS outside the apps phase")
    if "INIT:APP:QUIT:TIMEOUT" in tail:
        failures.append("an app ignored its Quit and was killed at the grace")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("serial_log", type=Path)
    parser.add_argument("--mode", choices=["poweroff", "reboot"], default="poweroff")
    parser.add_argument("--console", action="store_true",
                        help="not a desktop boot: do not require logd's journals")
    args = parser.parse_args()
    failures = judge(args.serial_log.read_text(encoding="utf-8", errors="replace"), args.mode,
                     desktop=not args.console)
    for failure in failures:
        print(f"FAIL: {failure}")
    print("PASS" if not failures else f"{len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
