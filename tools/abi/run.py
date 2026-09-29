#!/usr/bin/env python3
"""Run the Linux-ABI conformance bench and write the compatibility matrix.

For each fixture: rebuild the OS image with the fixture embedded as `INIT.ELF`
(`LAZYOS_INIT=<path> cargo build`), boot it headless, capture the serial log, and
classify the result from the log:

    ABI:<fixture>:PASS            -> pass
    ABI:<fixture>:FAIL:<reason>   -> fail
    ABI:INIT:SKIP:<reason>        -> skip  (kernel recognised the fixture but
                                           cannot run Linux binaries yet)
    (no marker)                   -> not-run

Outputs `docs/compat/matrix.md` and `docs/compat/compat.json`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
FIXTURE_DIR = ROOT / "target" / "abi" / "fixtures"
SHOTS = ROOT / "shots" / "abi"
COMPAT = ROOT / "docs" / "compat"
IMAGE = ROOT / "target" / "lazyos.img"

ORDER = [
    "hello",
    "alloc",
    "hashmap",
    "file",
    "time",
    "thread",
    "syncstress",
    "fsstress",
    "memstress",
    "procstress",
    "sigstress",
    "epollstress",
    "unixstress",
    "persist",
    "busybox",
]

# Fixtures that need the persistent data disk and two boots of it. The first
# boot writes and prints `ABI:<name>:<marker>`; the second, on the same disk,
# verifies what survived and prints `ABI:<name>:PASS`.
TWO_BOOT = {"persist": "WROTE"}


def build_image(fixture_path: Path, busybox: bool = False) -> bool:
    env = dict(os.environ)
    # Never let a caller's exports leak between rows: a `BUSYBOX` embedded for
    # another fixture would shadow its `INIT.ELF`, and vice versa.
    for key in ("LAZYOS_INIT", "LAZYOS_BUSYBOX", "LAZYOS_BUSYBOX_TEST"):
        env.pop(key, None)
    if busybox:
        # The BusyBox row embeds it as the system shell and makes the kernel run
        # `sh -c "echo ABI:busybox:PASS"` (the `busybox_test` cfg), then exit.
        env["LAZYOS_BUSYBOX"] = str(fixture_path)
        env["LAZYOS_BUSYBOX_TEST"] = "1"
    else:
        env["LAZYOS_INIT"] = str(fixture_path)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"warning: image build failed for {fixture_path.name}", file=sys.stderr)
        print(result.stderr[-1500:], file=sys.stderr)
    return result.returncode == 0


def capture(name: str, at: str, accel: str = "auto", data_disk: Path | None = None) -> str:
    out = SHOTS / name
    command = [
        sys.executable,
        str(ROOT / "tools" / "screenshot" / "qemu_shot.py"),
        "--out",
        str(out),
        "--at",
        at,
        "--accel",
        accel,
        "--image",
        str(IMAGE),
    ]
    if data_disk:
        command += ["--data-disk", str(data_disk)]
    subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    log = out / "serial.log"
    return log.read_text(errors="replace") if log.is_file() else ""


def classify(name: str, serial: str, ok: str = "PASS") -> tuple[str, str]:
    """Classify one boot; `ok` is the marker that counts as success."""
    if re.search(rf"ABI:{re.escape(name)}:{ok}", serial):
        return "pass", ""
    match = re.search(rf"ABI:{re.escape(name)}:FAIL:(.*)", serial)
    if match:
        return "fail", match.group(1).strip()[:120]
    match = re.search(r"ABI:INIT:SKIP:(.*)", serial)
    if match:
        return "skip", match.group(1).strip()[:120]
    return "not-run", ""


def data_disk_tooling() -> bool:
    """Whether the host can format a data volume and attach it to QEMU."""
    return (ROOT / "tools" / "mkdisk").is_dir()


def new_data_disk(name: str) -> Path | None:
    """A freshly formatted, small ext2 data volume for one fixture, or `None`."""
    path = ROOT / "target" / "abi" / f"{name}-data.img"
    path.parent.mkdir(parents=True, exist_ok=True)
    result = subprocess.run(
        [sys.executable, "-m", "tools.mkdisk", str(path), "--size", "8M", "--force"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    return path if result.returncode == 0 else None


def run_two_boots(name: str, at: str, accel: str) -> tuple[str, str]:
    """Boot the same image twice on one data disk; the second boot must PASS.

    Boot 1 must report its marker first: a fixture that fails while writing is
    reported as such, not as a failed verification.
    """
    if not data_disk_tooling():
        return "unavailable", "needs the data-disk tooling (tools/mkdisk)"
    disk = new_data_disk(name)
    if disk is None:
        return "fail", "could not format the data disk"
    first, detail = classify(name, capture(f"{name}-boot1", at, accel, disk), TWO_BOOT[name])
    if first != "pass":
        return first, f"boot 1: {detail}".rstrip(": ")
    serial = capture(name, at, accel, disk)
    status, detail = classify(name, serial)
    if status != "pass" and re.search(rf"ABI:{re.escape(name)}:{TWO_BOOT[name]}", serial):
        return "fail", "boot 2: the file written by boot 1 was gone"  # started over
    return status, f"boot 2: {detail}" if detail else ""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--at", default="8", help="capture time in seconds (default 8)")
    parser.add_argument("--only", help="comma-separated fixture names to run")
    parser.add_argument("--accel", default="auto",
                        help="QEMU accelerator: auto (kvm/whpx if usable, else TCG), kvm, whpx, none")
    args = parser.parse_args()

    COMPAT.mkdir(parents=True, exist_ok=True)
    SHOTS.mkdir(parents=True, exist_ok=True)

    wanted = set(args.only.split(",")) if args.only else set(ORDER)
    results: list[dict] = []

    for name in ORDER:
        if name not in wanted:
            continue
        fixture = FIXTURE_DIR / f"{name}.elf"
        if not fixture.is_file():
            results.append({"fixture": name, "status": "unavailable", "detail": "fixture not built"})
            continue
        if not build_image(fixture, busybox=(name == "busybox")):
            results.append({"fixture": name, "status": "fail", "detail": "image build failed"})
            continue
        if name in TWO_BOOT:
            status, detail = run_two_boots(name, args.at, args.accel)
        else:
            status, detail = classify(name, capture(name, args.at, args.accel))
        results.append({"fixture": name, "status": status, "detail": detail})
        print(f"{name}: {status} {detail}".rstrip())

    readme = ROOT / "docs" / "compat" / "compat.json"
    readme.write_text(json.dumps(results, indent=2), encoding="utf-8")

    icons = {"pass": "PASS", "fail": "FAIL", "skip": "skip", "unavailable": "n/a", "not-run": "not-run"}
    lines = [
        "# Linux ABI compatibility matrix",
        "",
        "_Generated by `tools/abi/run.py`. See the wiki page Linux ABI Compatibility._",
        "",
        "| Fixture | Status | Detail |",
        "|---|---|---|",
    ]
    for row in results:
        status = icons.get(row["status"], row["status"])
        lines.append(f"| {row['fixture']} | {status} | {row['detail']} |")
    (COMPAT / "matrix.md").write_text("\n".join(lines) + "\n", encoding="utf-8")

    passed = sum(1 for r in results if r["status"] == "pass")
    print(f"\n{passed}/{len(results)} fixtures passing")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
