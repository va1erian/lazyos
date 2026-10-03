#!/usr/bin/env python3
"""Run the Linux-ABI conformance bench and write the compatibility matrix.

For each fixture: rebuild the OS image with the fixture embedded as `/system/bin/abi-init`
(`LAZYOS_INIT=<path> cargo build`), boot it headless, capture the serial log, and
classify the result from the log:

    ABI:<fixture>:PASS            -> pass
    ABI:<fixture>:FAIL:<reason>   -> fail
    ABI:INIT:SKIP:<reason>        -> skip  (kernel recognised the fixture but
                                           cannot run Linux binaries yet)
    (no marker)                   -> not-run

Outputs `docs/compat/matrix.md` and `docs/compat/compat.json`.

Every image is built first (one `cargo build` each, sequential: they share the
target directory), then the boots run `--jobs` at a time: each row waits a fixed
`--at` seconds, so on a multi-core host the guests overlap instead of queueing.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import re
import shutil
import subprocess
import sys
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
FIXTURE_DIR = ROOT / "target" / "abi" / "fixtures"
# One image per row, copied out of `target/lazyos.img` after its build, so the
# boots can run side by side while the next row's image is built.
IMAGE_DIR = ROOT / "target" / "abi" / "images"
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
    "statxio",
    "cwd",
    "fsops",
    "fdinherit",
    "statmiss",
    "compat",
    "busybox",
    # Real programs (`tools/linuxapps/build.py`): one boot of the `linuxapps`
    # fixture, one row per program (see `LINUXAPPS`).
    "dash",
    "lua",
    "sqlite3",
    "jq",
    "rg",
]

# Rows judged from one boot of the `linuxapps` fixture on an image built with
# `LAZYOS_LINUXAPPS=1` (the programs in /system/bin) and BusyBox (dash's
# pipeline runs an applet). The fixture prints `ABI:<program>:PASS|FAIL`.
LINUXAPPS = ("dash", "lua", "sqlite3", "jq", "rg")
LINUXAPPS_BIN = ROOT / "target" / "linuxapps" / "bin"

# Fixtures that need the persistent data disk and two boots of it. The first
# boot writes and prints `ABI:<name>:<marker>`; the second, on the same disk,
# verifies what survived and prints `ABI:<name>:PASS`.
TWO_BOOT = {"persist": "WROTE"}

# Fixtures that want a data disk in a single boot, and the directories they
# must report having worked in (`ABI:<name>:ROUND:<dir>`) when one is attached.
ONE_BOOT_WITH_DATA = {"cwd": ("/tmp", "/data"), "fsops": ("/tmp", "/", "/data")}

# Fixtures that need BusyBox on the image beside their `/system/bin/abi-init` (which still
# owns the boot): `statmiss` checks that a missing path is not mistaken for a
# BusyBox applet alias, which only exists when there is a BusyBox to alias.
WITH_BUSYBOX = {"statmiss"}


def build_image(
    fixture_path: Path,
    busybox: bool = False,
    extra_busybox: Path | None = None,
    linuxapps: bool = False,
) -> Path | None:
    """Build the image with the fixture embedded; its per-row copy, or `None`.

    `extra_busybox` embeds that BusyBox as well, without the bench command:
    the fixture still owns the boot."""
    env = dict(os.environ)
    # Each row builds a fresh image with its own `/system/bin/abi-init` and copies it: keep
    # the OS volume small (it only needs the fixture and BusyBox) and never
    # update a developer's persistent one in place.
    env.setdefault("LAZYOS_OS_SIZE", "128M")
    env.setdefault("LAZYOS_RESET_OS", "1")
    # Never let a caller's exports leak between rows: a `/system/bin/busybox` embedded for
    # another fixture would shadow its `/system/bin/abi-init`, and vice versa.
    for key in ("LAZYOS_INIT", "LAZYOS_BUSYBOX", "LAZYOS_BUSYBOX_TEST", "LAZYOS_LINUXAPPS"):
        env.pop(key, None)
    if linuxapps:
        env["LAZYOS_LINUXAPPS"] = "1"
        # Seven programs plus BusyBox outgrow the bench's small volume.
        env["LAZYOS_OS_SIZE"] = "256M"
    if busybox:
        # The BusyBox row embeds it as the system shell and makes the kernel run
        # `sh -c "echo ABI:busybox:PASS"` (the `busybox_test` cfg), then exit.
        env["LAZYOS_BUSYBOX"] = str(fixture_path)
        env["LAZYOS_BUSYBOX_TEST"] = "1"
    else:
        env["LAZYOS_INIT"] = str(fixture_path)
        if extra_busybox:
            env["LAZYOS_BUSYBOX"] = str(extra_busybox)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"warning: image build failed for {fixture_path.name}", file=sys.stderr)
        print(result.stderr[-1500:], file=sys.stderr)
        return None
    IMAGE_DIR.mkdir(parents=True, exist_ok=True)
    image = IMAGE_DIR / f"{fixture_path.stem}.img"
    shutil.copyfile(IMAGE, image)
    return image


# Guest RAM for every boot (`--memory`); `None` keeps qemu_shot's default.
GUEST_MEMORY: str | None = None


def capture(
    name: str, image: Path, at: str, accel: str = "auto", data_disk: Path | None = None
) -> str:
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
        str(image),
    ]
    if data_disk:
        command += ["--data-disk", str(data_disk)]
    if GUEST_MEMORY:
        command += ["--memory", GUEST_MEMORY]
    # A boot must be judged only by its own log: a stale one left by an earlier
    # run could carry the marker and pass a boot that never happened, and a
    # failed capture must not fall back to whatever is on disk.
    log = out / "serial.log"
    log.unlink(missing_ok=True)
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"warning: capture failed for {name}", file=sys.stderr)
        print(result.stderr[-500:], file=sys.stderr)
        return ""
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


def run_two_boots(name: str, image: Path, at: str, accel: str) -> tuple[str, str]:
    """Boot the same image twice on one data disk; the second boot must PASS.

    Boot 1 must report its marker first: a fixture that fails while writing is
    reported as such, not as a failed verification.
    """
    if not data_disk_tooling():
        return "unavailable", "needs the data-disk tooling (tools/mkdisk)"
    disk = new_data_disk(name)
    if disk is None:
        return "fail", "could not format the data disk"
    first, detail = classify(name, capture(f"{name}-boot1", image, at, accel, disk), TWO_BOOT[name])
    if first != "pass":
        return first, f"boot 1: {detail}".rstrip(": ")
    serial = capture(name, image, at, accel, disk)
    status, detail = classify(name, serial)
    if status != "pass" and re.search(rf"ABI:{re.escape(name)}:{TWO_BOOT[name]}", serial):
        return "fail", "boot 2: the file written by boot 1 was gone"  # started over
    return status, f"boot 2: {detail}" if detail else ""


def run_with_data_disk(name: str, image: Path, at: str, accel: str) -> tuple[str, str]:
    """A single-boot row that also exercises `/data` when the tooling exists.

    Without the data-disk tooling only the always-available directories count;
    with it, the fixture must have worked on every directory it lists.
    """
    disk = None
    if data_disk_tooling():
        disk = new_data_disk(name)
        if disk is None:
            return "fail", "could not format the data disk"
    serial = capture(name, image, at, accel, disk)
    status, detail = classify(name, serial)
    if status != "pass":
        return status, detail
    wanted = ONE_BOOT_WITH_DATA[name]
    if not disk:
        # `/data` is the last directory and needs the disk; the rest do not.
        wanted = tuple(d for d in wanted if d != "/data")
    for directory in wanted:
        if f"ABI:{name}:ROUND:{directory}" not in serial:
            return "fail", f"never worked in {directory}"
    return "pass", ""


def run_busybox(image: Path, at: str, accel: str) -> tuple[str, str]:
    """The BusyBox row: `sh` runs, and `df` and `mount` list the `/data` volume.

    The kernel's bench command is `echo ABI:busybox:PASS; df; mount`. With a
    data disk attached their output must name `/data` (they read
    `/proc/mounts`); without the disk tooling only the shell itself is judged.
    """
    disk = None
    if data_disk_tooling():
        # Same rule as the two-boot rows: tooling that is present but cannot
        # make the volume is a failure, never a silent pass without the checks.
        disk = new_data_disk("busybox")
        if disk is None:
            return "fail", "could not format the data disk"
    serial = capture("busybox", image, at, accel, disk)
    status, detail = classify("busybox", serial)
    if status != "pass" or disk is None:
        return status, detail
    # `/data` is a directory on the ext2 OS volume at `/` (F2), so `df` and
    # `mount` name `/`, the volume that holds it.
    if not re.search(r"^\s*\S+\s+\d+\s+\d+\s+\d+\s+\d+%\s+/\s*$", serial, re.M):
        return "fail", "df does not list the ext2 root /"
    if not re.search(r"\bon / type ext2 \(rw", serial):
        return "fail", "mount does not list / as ext2 (rw)"
    return check_busybox_cwd(serial)


def check_busybox_cwd(serial: str) -> tuple[str, str]:
    """`cd /data` must move the kernel's cwd, not just the shell's prompt (#365).

    The script prints `pwd -P` (which asks the kernel via `getcwd`) and `ls`
    (a forked and exec'd applet, so the directory must survive `execve`) after
    `cd /data`, then again after `cd ..`, each under its own marker line.
    """
    # `ls` colours its output because the serial console looks like a terminal.
    text = re.sub(r"\x1b?\[[0-9;]*m", "", serial)

    def section(start: str, end: str) -> str:
        pattern = rf"^ABI:busybox:{start}\r?\n(.*?)^ABI:busybox:{end}"
        found = re.search(pattern, text, re.M | re.S)
        return found.group(1) if found else ""

    if "/data" not in [line.strip() for line in section("CWD", "LS").splitlines()]:
        return "fail", "pwd -P after cd /data is not /data"
    if "cwdprobe" not in section("LS", "CWD2").split():
        return "fail", "ls after cd /data does not list the file echo wrote there"
    if "/" not in [line.strip() for line in section("CWD2", "LS2").splitlines()]:
        return "fail", "pwd -P after cd .. is not /"
    root = section("LS2", "END").split()
    if "cwdprobe" in root or "system" not in root:
        return "fail", "ls after cd .. does not list the boot volume"
    return "pass", ""


# One serial log per image: the `linuxapps` rows share one boot.
_SHARED_LOGS: dict[Path, str] = {}
_SHARED_LOCK = threading.Lock()


def run_linuxapps_row(name: str, image: Path, at: str, accel: str) -> tuple[str, str]:
    """Judge one program's row from the shared `linuxapps` boot."""
    with _SHARED_LOCK:
        if image not in _SHARED_LOGS:
            _SHARED_LOGS[image] = capture("linuxapps", image, at, accel)
        serial = _SHARED_LOGS[image]
    return classify(name, serial)


def run_row(name: str, image: Path, at: str, accel: str) -> tuple[str, str]:
    """Boot one row's image and judge it; safe to run alongside other rows
    (its own image copy, data disk, output directory and QMP port)."""
    if name in TWO_BOOT:
        return run_two_boots(name, image, at, accel)
    if name in ONE_BOOT_WITH_DATA:
        return run_with_data_disk(name, image, at, accel)
    if name == "busybox":
        return run_busybox(image, at, accel)
    if name in LINUXAPPS:
        return run_linuxapps_row(name, image, at, accel)
    return classify(name, capture(name, image, at, accel))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--at", default="8", help="capture time in seconds (default 8)")
    parser.add_argument("--only", help="comma-separated fixture names to run")
    parser.add_argument("--accel", default="auto",
                        help="QEMU accelerator: auto (kvm/whpx if usable, else TCG), kvm, whpx, none")
    parser.add_argument("--memory", help="guest RAM, passed to the session tool (default: its own, 1G)")
    parser.add_argument("--jobs", type=int, default=1,
                        help="guests to boot side by side (default 1; the images are "
                             "always built one at a time)")
    args = parser.parse_args()
    global GUEST_MEMORY
    GUEST_MEMORY = args.memory
    if args.jobs < 1:
        parser.error("--jobs must be at least 1")

    COMPAT.mkdir(parents=True, exist_ok=True)
    SHOTS.mkdir(parents=True, exist_ok=True)

    wanted = set(args.only.split(",")) if args.only else set(ORDER)
    rows = [name for name in ORDER if name in wanted]
    results: dict[str, dict] = {}

    def record(name: str, status: str, detail: str) -> None:
        results[name] = {"fixture": name, "status": status, "detail": detail}
        print(f"{name}: {status} {detail}".rstrip(), flush=True)

    # Build every image first (the builds share `target/`, so they are
    # sequential), then boot `--jobs` of them at a time.
    images: dict[str, Path] = {}
    linuxapps_image: Path | None = None
    for name in rows:
        if name in LINUXAPPS:
            if not (LINUXAPPS_BIN / name).is_file():
                record(name, "unavailable", "not built (tools/linuxapps/build.py)")
                continue
            fixture = FIXTURE_DIR / "linuxapps.elf"
            shell = FIXTURE_DIR / "busybox.elf"
            if not fixture.is_file() or not shell.is_file():
                record(name, "unavailable", "needs the linuxapps fixture and BusyBox")
                continue
            if linuxapps_image is None:
                linuxapps_image = build_image(fixture, extra_busybox=shell, linuxapps=True)
            if linuxapps_image is None:
                record(name, "fail", "image build failed")
                continue
            images[name] = linuxapps_image
            continue
        fixture = FIXTURE_DIR / f"{name}.elf"
        if not fixture.is_file():
            record(name, "unavailable", "fixture not built")
            continue
        extra = None
        if name in WITH_BUSYBOX:
            extra = FIXTURE_DIR / "busybox.elf"
            if not extra.is_file():
                record(name, "unavailable", "needs BusyBox (tools/abi/busybox.py)")
                continue
        image = build_image(fixture, busybox=(name == "busybox"), extra_busybox=extra)
        if image is None:
            record(name, "fail", "image build failed")
            continue
        images[name] = image

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = {
            pool.submit(run_row, name, image, args.at, args.accel): name
            for name, image in images.items()
        }
        for future in concurrent.futures.as_completed(futures):
            record(futures[future], *future.result())

    # The matrix keeps the fixture order however the boots finished.
    rows_out = [results[name] for name in rows]
    readme = ROOT / "docs" / "compat" / "compat.json"
    readme.write_text(json.dumps(rows_out, indent=2), encoding="utf-8")

    icons = {"pass": "PASS", "fail": "FAIL", "skip": "skip", "unavailable": "n/a", "not-run": "not-run"}
    lines = [
        "# Linux ABI compatibility matrix",
        "",
        "_Generated by `tools/abi/run.py`. See the wiki page Linux ABI Compatibility._",
        "",
        "| Fixture | Status | Detail |",
        "|---|---|---|",
    ]
    for row in rows_out:
        status = icons.get(row["status"], row["status"])
        lines.append(f"| {row['fixture']} | {status} | {row['detail']} |")
    (COMPAT / "matrix.md").write_text("\n".join(lines) + "\n", encoding="utf-8")

    passed = sum(1 for r in rows_out if r["status"] == "pass")
    print(f"\n{passed}/{len(rows_out)} fixtures passing")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
