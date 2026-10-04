#!/usr/bin/env python3
"""Measure LazyOS storage: throughput, exec time and interrupts-off stalls
(docs/performance-plan.md, stage P5).

Builds an image whose `abi-init` is the `diskbench` fixture
(`tools/abi/fixtures/src/diskbench.rs`) with BusyBox beside it and the
kernel's latency hooks on (`LAZYOS_PERF=1`), boots it headless on a fresh OS
volume, and parses what it prints:

    DISK:write_mbps        64 MiB in 1 MiB writes, then fsync
    DISK:read_cold_mbps    the same file read back (twice the block cache)
    DISK:read_again_mbps   a second pass
    DISK:read_binary_mbps  /system/bin/busybox read whole, five times
    DISK:exec_ms           spawn + wait of `busybox true`, 20 times
    DISK:small_files_ms    400 files of 24 KiB in 8 directories, then fsync

plus the kernel's `PERF:irqoff` histogram and `PERF:irqoff_worst` (the
longest stretch a syscall kept interrupts off, and which syscall). Every
phase checks the bytes it reads, so the bench also fails on corruption.

    python tools/perf/disk.py                    # build, boot, report
    python tools/perf/disk.py --no-build         # re-measure the current image
    python tools/perf/disk.py --label "P5.1"     # also append to docs/perf/disk.md's history

Writes `docs/perf/disk.json` and appends a row to `docs/perf/disk.md` when
labelled. Exit status is non-zero when the bench fails or never finishes.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
from qemu_qmp import (  # noqa: E402
    DEFAULT_MEMORY, Qmp, accel_args, build_qemu_command, find_qemu, free_port, resolve_accel,
)

FIXTURE = ROOT / "target" / "abi" / "fixtures" / "diskbench.elf"
BUSYBOX = ROOT / "target" / "abi" / "busybox" / "busybox"
REPORT_DIR = ROOT / "docs" / "perf"
RE_DISK = re.compile(r"^DISK:(\w+):([\d.]+) (\S+)(.*)$", re.M)
RE_METRIC = re.compile(
    r"PERF:(\w+):n=(\d+) p50_us=([\d.]+) p90_us=([\d.]+) p99_us=([\d.]+) "
    r"max_us=([\d.]+) mean_us=([\d.]+)"
)
RE_WORST = re.compile(r"PERF:irqoff_worst:us=([\d.]+) syscall=(0x[0-9a-f]+)")
ORDER = ("write_mbps", "read_cold_mbps", "read_again_mbps", "read_binary_mbps", "exec_ms",
         "small_files_ms")


def build_image() -> Path:
    print("building the fixture: python tools/abi/build.py", flush=True)
    subprocess.run([sys.executable, str(ROOT / "tools" / "abi" / "build.py")], cwd=ROOT, check=True,
                   stdout=subprocess.DEVNULL)
    if not FIXTURE.is_file():
        sys.exit(f"{FIXTURE} was not built (is the musl target installed?)")
    if not BUSYBOX.is_file():
        sys.exit(f"{BUSYBOX} is missing: run python tools/abi/busybox.py")
    env = dict(os.environ, LAZYOS_INIT=str(FIXTURE), LAZYOS_BUSYBOX=str(BUSYBOX),
               LAZYOS_PERF="1", LAZYOS_RESET_OS="1")
    for key in ("LAZYOS_DESKTOP", "LAZYOS_BUSYBOX_TEST", "LAZYOS_LINUXAPPS", "LAZYOS_TESTS"):
        env.pop(key, None)
    print("building: LAZYOS_INIT=diskbench LAZYOS_PERF=1 LAZYOS_RESET_OS=1 cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"cargo build failed:\n{result.stderr[-4000:]}")
    return ROOT / "target" / "lazyos.img"


def run(image: Path, accel: str, memory: str, qemu: str, out: Path, timeout: float) -> str:
    out.mkdir(parents=True, exist_ok=True)
    serial = out / "serial.log"
    serial.unlink(missing_ok=True)
    port = free_port()
    command = build_qemu_command(qemu, str(image), port, serial, memory=memory,
                                 extra_args=accel_args(accel, qemu))
    print(f"booting: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    qmp = None
    try:
        qmp = Qmp("127.0.0.1", port, timeout=30)
        deadline = time.time() + timeout
        while time.time() < deadline:
            text = serial.read_text(errors="replace") if serial.is_file() else ""
            if "DISK:done" in text or "ABI:diskbench:FAIL" in text or "PANIC" in text:
                break
            if proc.poll() is not None:
                break
            time.sleep(0.5)
        time.sleep(1.0)
    finally:
        if qmp is not None:
            try:
                qmp.execute("quit")
            except Exception:
                pass
            qmp.close()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
    return serial.read_text(errors="replace") if serial.is_file() else ""


def parse(text: str) -> dict:
    disk = {}
    for match in RE_DISK.finditer(text):
        disk[match.group(1)] = {"value": float(match.group(2)), "unit": match.group(3),
                                "detail": match.group(4).strip()}
    perf = {}
    for match in RE_METRIC.finditer(text):
        fields = ("n", "p50_us", "p90_us", "p99_us", "max_us", "mean_us")
        values = [int(match.group(2))] + [float(match.group(i)) for i in range(3, 8)]
        perf[match.group(1)] = dict(zip(fields, values))
    worst = None
    for match in RE_WORST.finditer(text):
        worst = {"us": float(match.group(1)), "syscall": match.group(2)}
    return {"disk": disk, "irqoff": perf.get("irqoff"), "irqoff_worst": worst}


def git_commit() -> str:
    head = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, capture_output=True, text=True)
    dirty = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, capture_output=True, text=True)
    return head.stdout.strip() + ("+dirty" if dirty.stdout.strip() else "")


def append_history(payload: dict) -> Path:
    path = REPORT_DIR / "disk.md"
    if not path.is_file():
        path.write_text("\n".join([
            "# Storage history",
            "",
            "One row per labelled `python tools/perf/disk.py --label ...` run "
            "(docs/performance-plan.md P5). MB/s, milliseconds, microseconds.",
            "",
            "| Label | Commit | Accel | write MB/s | read cold MB/s | read again MB/s | "
            "busybox read MB/s | exec ms | 400 small files ms | irqoff p99/max µs | worst syscall |",
            "|---|---|---|---:|---:|---:|---:|---:|---:|---|---|",
        ]) + "\n", encoding="utf-8")
    disk, meta = payload["disk"], payload["meta"]

    def value(name: str) -> str:
        row = disk.get(name)
        return f"{row['value']:.1f}" if row else "-"

    irqoff = payload.get("irqoff")
    irq = f"{irqoff['p99_us']:.0f}/{irqoff['max_us']:.0f}" if irqoff else "-"
    worst = payload.get("irqoff_worst")
    row = (f"| {meta['label']} | `{meta['commit']}` | {meta['accel']} | "
           + " | ".join(value(name) for name in ORDER)
           + f" | {irq} | {worst['syscall'] if worst else '-'} |")
    with path.open("a", encoding="utf-8") as handle:
        handle.write(row + "\n")
    return path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", default=str(ROOT / "target" / "lazyos.img"))
    parser.add_argument("--out", default="shots/perf_disk")
    parser.add_argument("--qemu")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default=DEFAULT_MEMORY)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--label", help="append this run to docs/perf/disk.md under this label")
    args = parser.parse_args()

    image = Path(args.image) if args.no_build else build_image()
    qemu = find_qemu(args.qemu)
    accel = resolve_accel(args.accel, qemu)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    text = run(image, args.accel, args.memory, qemu, out, args.timeout)
    parsed = parse(text)
    payload = {
        "meta": {
            "generated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "commit": git_commit(),
            "label": args.label,
            "accel": accel,
            "memory": args.memory,
        },
        **parsed,
    }
    REPORT_DIR.mkdir(parents=True, exist_ok=True)
    (REPORT_DIR / "disk.json").write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    for name in ORDER:
        row = parsed["disk"].get(name)
        print(f"  {name:18} " + (f"{row['value']} {row['unit']} {row['detail']}" if row else "missing"))
    print(f"  irqoff             {parsed['irqoff']}")
    print(f"  irqoff_worst       {parsed['irqoff_worst']}")
    if args.label:
        print(f"history: {append_history(payload).relative_to(ROOT)}")
    failure = re.search(r"ABI:diskbench:FAIL:(.*)", text)
    if failure:
        print(f"FAIL: {failure.group(1).strip()}", file=sys.stderr)
        return 1
    if "ABI:diskbench:PASS" not in text:
        print(text[-3000:])
        print("FAIL: the bench never finished", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
