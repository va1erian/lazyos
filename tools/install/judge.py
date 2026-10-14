"""Judge the install-beside-Linux harness (docs/grub-install-plan.md I3).

Pure functions over text, so ``test_judge.py`` shows they fail when they
should without booting anything. The verdicts:

* **install**: ``lazyos-install`` said ``done`` and reported no error; in the
  GRUB variant the "Linux" side of the ESP (its ``grub.cfg``, GRUB itself
  and ``grubenv``) is byte-identical after the install, which only adds
  ``custom.cfg`` beside them.
* **boot** (the installed disk alone on AHCI, under OVMF): the stick's boot
  judge (``tools/boot/judge.py``: one ``BOOT:MEDIA:uefi``, the desktop,
  ``usbd`` on the keyboard, no panic, the first-boot package install), with
  the root on the system partition (``FS:ROOT:ahci0p3``), named by the UUID
  the installer gave it, and ``/home`` the home partition (``ahci0p4``); the
  boot volume is the config-only ramdisk (``ram0p1``), whose ``lazyos.cfg``
  wins. A boot that is to power off must reach ``power: filesystems synced``.
* **host** (``testdisk check`` after a clean power-off): the checker finds
  both volumes clean, and LazyOS wrote to them (``/apps``, ``/logs``).
* **linux** (GRUB variant, a boot with no ``next_entry``): GRUB ran its
  default, the Linux stand-in, and LazyOS did not start.
"""

from __future__ import annotations

import sys as _sys
from pathlib import Path as _Path

_sys.path.insert(0, str(_Path(__file__).resolve().parents[1]))
import serial_integrity  # noqa: E402  (a log with SERIAL:DROPPED proves nothing)

import importlib.util
import re
from pathlib import Path


def _boot_judge():
    """`tools/boot/judge.py`, under its own name (this module is `judge` too)."""
    path = Path(__file__).resolve().parent.parent / "boot" / "judge.py"
    spec = importlib.util.spec_from_file_location("boot_judge", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


boot_judge = _boot_judge()

SYSTEM_DEVICE = "ahci0p3"
HOME_DEVICE = "ahci0p4"
SYNCED = "power: filesystems synced"
LINUX_STANDIN = "LINUX:STANDIN"
UUID = r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"


def install_uuids(log: str) -> tuple[str | None, str | None]:
    """The system and home UUIDs `lazyos-install` printed in its plan."""
    system = re.search(rf"system: FORMAT .* as ext2 ({UUID})", log)
    home = re.search(rf"home:   FORMAT .* as ext2 ({UUID})", log)
    return (system.group(1) if system else None, home.group(1) if home else None)


def judge_install(log: str, before: dict[str, bytes] | None = None,
                  after: dict[str, bytes] | None = None) -> list[str]:
    failures = serial_integrity.problems(log)
    if not re.search(r"^done\s*$", log, re.M):
        failures.append("lazyos-install did not finish (no `done`)")
    if "lazyos-install:" in log:
        failures.append("lazyos-install reported an error: "
                        + log.split("lazyos-install:", 1)[1].strip().splitlines()[0])
    if None in install_uuids(log):
        failures.append("the plan names no system/home UUID")
    for name in sorted((before or {}).keys()):
        if (after or {}).get(name) != before[name]:
            failures.append(f"the Linux side's {name} changed")
    return failures


def judge_boot(log: str, stats: dict, uuids: tuple[str | None, str | None],
               power_off: bool) -> list[str]:
    failures = boot_judge.judge_serial(log, "uefi", SYSTEM_DEVICE)
    failures += boot_judge.judge_pixels(stats)
    failures += boot_judge.judge_provision(log)
    system, home = uuids
    if system and not re.search(rf"^fs: root {system} is {SYSTEM_DEVICE}\s*$", log, re.M):
        failures.append(f"the root is not {system} on {SYSTEM_DEVICE}")
    if home and not re.search(rf"^fs: home {home} is {HOME_DEVICE}\s*$", log, re.M):
        failures.append(f"/home is not {home} on {HOME_DEVICE}")
    if not re.search(r"^fs: mounted .* at /boot", log, re.M):
        failures.append("no /boot mount (the config ramdisk)")
    if "block: ahci0p1 type 0xef" not in log:
        failures.append("the kernel did not read the disk's GPT (no ahci0p1 ESP)")
    if power_off and SYNCED not in log:
        failures.append(f"the power-off never reached `{SYNCED}`")
    return failures


def judge_host(report: str) -> list[str]:
    """`testdisk check` output: `key: value` lines."""
    values = dict(line.split(": ", 1) for line in report.splitlines() if ": " in line)
    failures = []
    for role in ("system", "home"):
        verdict = values.get(f"{role}.check", "missing")
        if not verdict.startswith("clean"):
            failures.append(f"the {role} volume is not clean: {verdict}")
    for key in ("system.apps", "system.logs"):
        if int(values.get(key, "0") or 0) <= 0:
            failures.append(f"{key} is empty: LazyOS wrote nothing there")
    return failures


def judge_linux(log: str) -> list[str]:
    failures = []
    if LINUX_STANDIN not in log:
        failures.append("GRUB did not boot its default (the Linux stand-in)")
    if boot_judge.MEDIA.search(log):
        failures.append("LazyOS started without a next_entry: the trial boot was not one-shot")
    return failures
