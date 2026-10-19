#!/usr/bin/env python3
"""U2a spike (docs/update-plan.md decision 6): can real GRUB count LazyOS trial
boots in a GRUB environment block on LazyOS's own ext2 system volume?

Builds the same disk as ``run.py --via grub`` (``grub-mkstandalone``, a
Debian-shaped ``grub.cfg`` whose default is the "Linux" stand-in and whose
``grubenv`` carries ``saved_entry=linux``), installs with ``lazyos-install
--test-grubenv 4`` (``/releases/grubenv`` on the system volume, the snippet
before the entry in ``custom.cfg``), then boots under OVMF and judges each
boot from the host: which entry GRUB chose and what ``lazyos_boots`` reads back.

* ``custom.cfg``: 4, 3, 2, 1 boot LazyOS (counter 3, 2, 1, 0), the fifth the
  default; then corrupt values (``x``, ``5``, empty) leave the default alone.
* ``grub.cfg``: the same text spliced where ``/etc/grub.d/40_lazyos`` puts it:
  1 then 0, and a corrupt value.
* The distro's files on the ESP (its ``grub.cfg``, GRUB, ``grubenv`` with
  ``saved_entry``) stay byte-identical throughout.

A LazyOS boot ends at its first serial marker (the kernel need not finish);
GRUB's choice and the counter are the evidence.

    python tools/install/grubenv_run.py [--no-build]
    python tools/install/test_grubenv_judge.py       # the judge fails when it should
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import grubenv_judge as g  # noqa: E402
import judge  # noqa: E402
import run as harness  # noqa: E402

MEDIA = "BOOT:MEDIA:uefi"
# The stand-in's saved_entry is what `GRUB_DEFAULT=saved` distros read.
GRUB_CFG = harness.GRUB_CFG.replace(
    "   set default=0" + chr(10), "   set default=" + chr(34) + "${saved_entry}" + chr(34) + chr(10))
assert GRUB_CFG != harness.GRUB_CFG, "run.py's grub.cfg no longer has the `set default=0` fallback"


def say(text: str) -> None:
    harness.say(text)


def boot(args, name: str, disk: Path, firmware, out: Path, entry: str) -> str:
    """One boot; ends at the first marker of the entry it should reach."""
    marker = MEDIA if entry == "lazyos" else judge.LINUX_STANDIN
    other = judge.LINUX_STANDIN if entry == "lazyos" else MEDIA
    steps = [{"wait_for": marker, "timeout": args.timeout}, {"wait": 2}]
    return harness.session(args, name, steps, harness.qemu_disk_args(disk, firmware, out, name),
                           out, fail_on=other)


def set_counter(disk: Path, out: Path, value: str) -> None:
    (out / "grubenv.set").write_bytes(g.env_block({"lazyos_boots": value}))
    harness.run([harness.TESTDISK, "grubenv-put", disk, out / "grubenv.set"])


def read_counter_block(disk: Path, out: Path) -> bytes:
    harness.run([harness.TESTDISK, "grubenv-get", disk, out / "grubenv.get"])
    return (out / "grubenv.get").read_bytes()


def ladder(args, rows, label: str, disk: Path, firmware, out: Path, report: dict) -> None:
    for number, (before, entry, after) in enumerate(rows, 1):
        name = f"{label}-{number}"
        if number == 1 or label.startswith("corrupt"):
            set_counter(disk, out, before)
        say(f"{name}: lazyos_boots={before!r}, expecting {entry}, then {after!r}")
        log = boot(args, name, disk, firmware, out, entry)
        block = read_counter_block(disk, out)
        failures = g.judge_boot(log, block, entry, after)
        report["boots"].append({"name": name, "before": before, "expect": entry, "after": after,
                                "booted": g.entry_booted(log), "counter": g.counter(block),
                                "grub_errors": g.grub_errors(log)})
        report["failures"][name] = failures


def torn(args, disk: Path, firmware, out: Path, report: dict) -> None:
    """A block with a bad header (a torn write): GRUB's default, bytes untouched."""
    bad = bytearray(g.env_block({"lazyos_boots": "4"}))
    bad[2:6] = b"XXXX"
    (out / "grubenv.set").write_bytes(bytes(bad))
    harness.run([harness.TESTDISK, "grubenv-put", disk, out / "grubenv.set"])
    log = boot(args, "torn", disk, firmware, out, "linux")
    block = read_counter_block(disk, out)
    failures = g.judge_boot(log, block, "linux", None)
    failures += [] if block == bytes(bad) else ["GRUB rewrote a block with a bad header"]
    report["boots"].append({"name": "torn", "booted": g.entry_booted(log), "grub_errors": g.grub_errors(log)})
    report["failures"]["torn"] = failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--out", type=Path, default=harness.ROOT / "shots" / "install" / "grubenv")
    parser.add_argument("--system-mib", default="2048")
    parser.add_argument("--home-mib", default="256")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory", default="2G")
    parser.add_argument("--timeout", type=float, default=400.0)
    parser.add_argument("--qemu", default=None)
    parser.add_argument("--ovmf-code", default=None)
    parser.add_argument("--ovmf-vars", default=None)
    args = parser.parse_args()
    args.via = "grub"
    if not shutil.which("grub-mkstandalone"):
        raise SystemExit("grub-mkstandalone not found (Linux, grub-efi-amd64-bin)")
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    if not args.no_build:
        harness.build()
    harness.run(["cargo", "build", "-q", "-p", "lazyos-install", "--bins", "--examples"])
    qemu = harness.qemu_qmp.find_qemu(args.qemu)
    firmware = harness.ovmf(args.ovmf_code, args.ovmf_vars, qemu)
    report: dict = {"boots": [], "failures": {}}
    version = harness.run(["grub-mkstandalone", "--version"], check=False).strip()
    report["grub"] = version
    say(version)

    disk = harness.make_disk(args, out)
    (out / "grub.cfg").write_text(GRUB_CFG)
    harness.run([harness.TESTDISK, "esp-put", disk, out / "grub.cfg", "EFI/BOOT/grub.cfg"])
    (out / "grubenv").write_bytes(g.env_block({"saved_entry": "linux"}))
    harness.run([harness.TESTDISK, "esp-put", disk, out / "grubenv", "EFI/BOOT/grubenv"])
    files = harness.GRUB_FILES + ("EFI/BOOT/grubenv",)
    before = harness.esp_files(disk, out, files)
    install = harness.run([harness.INSTALLER, "--image", disk, "--stick", out / "stick", "--yes",
                           "--grub-dir", "EFI/BOOT", "--check-limit", "4096",
                           "--test-grubenv", "4"], log=out / "install.log", check=False)
    report["failures"]["install"] = judge.judge_install(install, before, harness.esp_files(disk, out, files))

    if not any(report["failures"].values()):
        ladder(args, g.COUNTDOWN, "countdown", disk, firmware, out, report)
        ladder(args, g.CORRUPT, "corrupt-custom", disk, firmware, out, report)
        # The other placement: the block moves into grub.cfg, custom.cfg empties.
        custom = harness.esp_files(disk, out, ("EFI/BOOT/custom.cfg",))["EFI/BOOT/custom.cfg"]
        cfg, custom = g.splice_grubd(GRUB_CFG, custom.decode())
        for name, text in (("grub.cfg", cfg), ("custom.cfg", custom)):
            (out / name).write_text(text)
            harness.run([harness.TESTDISK, "esp-put", disk, out / name, f"EFI/BOOT/{name}"])
        ladder(args, g.GRUBD, "grubd", disk, firmware, out, report)
        ladder(args, g.CORRUPT[:1], "corrupt-grubd", disk, firmware, out, report)
        torn(args, disk, firmware, out, report)
        after = harness.esp_files(disk, out, ("EFI/BOOT/BOOTX64.EFI", "EFI/BOOT/grubenv"))
        report["failures"]["linux-side"] = g.judge_linux_side(
            {k: before[k] for k in after}, after)

    failures = [f"{stage}: {f}" for stage, items in report["failures"].items() for f in items]
    (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    print(("FAIL: " + "; ".join(failures)) if failures else "PASS", flush=True)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
