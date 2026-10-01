#!/usr/bin/env python3
"""Run the LazyOS in-kernel test suite (issue #62) in headless QEMU.

Builds the test image (`LAZYOS_TESTS=1 cargo build`), boots it with a QMP
socket, captures the serial log, parses the `TEST:` protocol, and writes
`docs/test/report.md` + `docs/test/report.json`.

Protocol (one line per test, emitted by `kernel/src/tests/mod.rs::run()`):

    TEST:<name>:PASS
    TEST:<name>:FAIL:<detail>
    TEST:SUMMARY:PASS=<n> FAIL=<n>
    TEST:<name>:INFO:<detail>       (informational, e.g. soak timing)
    TEST:<name>:PROGRESS:<detail>   (informational progress)

Exit status is non-zero if any test fails, the summary is missing (e.g. a
panic or a stale non-test image), or the summary disagrees with the parsed
results.

Usage
-----
    python tools/test/run.py                     # build + run, default output
    python tools/test/run.py --accel none        # force TCG (deterministic, no KVM/WHPX)
    python tools/test/run.py --no-build          # re-run the current image
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
REPORT_DIR = ROOT / "docs" / "test"

sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
from qemu_qmp import Qmp, accel_args, build_qemu_command, find_qemu, free_port  # noqa: E402

RE_SUMMARY = re.compile(r"^TEST:SUMMARY:PASS=(\d+) FAIL=(\d+)\s*$")
RE_RESULT = re.compile(r"^TEST:([^:]+):(PASS|FAIL)(?::(.*))?$")
RE_INFO = re.compile(r"^TEST:([^:]+):(INFO|PROGRESS):(.*)$")

TEST_BANNER = "kernel test mode"
# Must match `SCRATCH_SECTORS` in kernel/src/tests/virtio_suite.rs.
SCRATCH_BYTES = 16 * 1024 * 1024


def build_test_image(image: Path) -> None:
    """`cargo build` with LAZYOS_TESTS=1, which rebuilds the kernel and image."""
    env = dict(os.environ, LAZYOS_TESTS="1")
    print("building test image: LAZYOS_TESTS=1 cargo build", flush=True)
    result = subprocess.run(
        ["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True
    )
    if result.returncode != 0:
        sys.exit(f"cargo build failed:\n{result.stderr[-4000:]}")
    if not image.is_file():
        sys.exit(f"build succeeded but {image} does not exist")


def parse_serial(text: str) -> dict:
    """Parse TEST: lines into a report payload."""
    results: list[dict] = []
    info: list[str] = []
    summary: dict | None = None
    for line in text.splitlines():
        line = line.strip()
        match = RE_SUMMARY.match(line)
        if match:
            summary = {"pass": int(match.group(1)), "fail": int(match.group(2))}
            continue
        match = RE_RESULT.match(line)
        if match:
            name, status, detail = match.group(1), match.group(2), match.group(3) or ""
            results.append(
                {
                    "name": name,
                    "status": "pass" if status == "PASS" else "fail",
                    "detail": detail.strip(),
                }
            )
            continue
        if RE_INFO.match(line):
            info.append(line)
    return {"tests": results, "info": info, "reported": summary}


def follow_serial(serial_log: Path, proc: subprocess.Popen, deadline: float) -> str:
    """Poll the serial log (streaming TEST lines) until the summary or deadline."""
    consumed = 0
    text = ""
    while time.time() < deadline:
        if serial_log.is_file():
            text = serial_log.read_text(errors="replace")
            complete = text.count("\n")
            for line in text.splitlines()[consumed:complete]:
                if line.startswith("TEST:") or "PANIC" in line or "EXCEPTION" in line:
                    print(f"  {line}", flush=True)
            consumed = complete
            if "TEST:SUMMARY:" in text:
                return text
        if proc.poll() is not None:
            time.sleep(0.5)  # let the last writes reach the file
            if serial_log.is_file():
                text = serial_log.read_text(errors="replace")
            return text
        time.sleep(0.25)
    return text


def stop_qemu(proc: subprocess.Popen, qmp: Qmp | None) -> None:
    if qmp is not None:
        try:
            qmp.execute("quit")
        except Exception:
            pass
        qmp.close()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()


def write_report(payload: dict) -> Path:
    REPORT_DIR.mkdir(parents=True, exist_ok=True)
    generated = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    tests = payload["tests"]
    passed = sum(1 for test in tests if test["status"] == "pass")
    failed = sum(1 for test in tests if test["status"] == "fail")
    reported = payload["reported"]
    ok = (
        failed == 0
        and not payload["missing_summary"]
        and reported is not None
        and reported["pass"] == passed
        and reported["fail"] == failed
    )
    report = {
        "generated": generated,
        "image": payload["image"],
        "qemu": payload["qemu"],
        "accel": payload["accel"],
        "qemu_exit_code": payload["qemu_exit_code"],
        "missing_summary": payload["missing_summary"],
        "summary": {"pass": passed, "fail": failed},
        "reported_summary": reported,
        "tests": tests,
        "info": payload["info"],
        "ok": ok,
    }
    (REPORT_DIR / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")

    lines = [
        "# Kernel test report",
        "",
        "_Generated by `tools/test/run.py` (issue #62)._",
        "",
        "| Field | Value |",
        "|---|---|",
        f"| Generated | {generated} |",
        f"| Image | `{payload['image']}` |",
        f"| QEMU | `{payload['qemu']}` (accel: {payload['accel']}) |",
        f"| Result | **{passed} pass / {failed} fail** |",
        "",
    ]
    if payload["missing_summary"]:
        lines += [
            "> **TEST:SUMMARY was never printed.** The kernel panicked, hung, or a "
            "stale (non-test) image was booted.",
            "",
        ]
    lines += ["## Tests", "", "| Test | Status | Detail |", "|---|---|---|"]
    for test in tests:
        status = "PASS" if test["status"] == "pass" else "FAIL"
        lines.append(f"| `{test['name']}` | {status} | {test['detail']} |")
    if not tests:
        lines.append("| _(none parsed)_ | | |")
    if payload["info"]:
        lines += ["", "<details><summary>Soak/progress output</summary>", "", "```"]
        lines += payload["info"]
        lines += ["```", "", "</details>"]
    report_md = REPORT_DIR / "report.md"
    report_md.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return report_md


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--image", default=str(ROOT / "target" / "lazyos.img"), help="disk image to boot"
    )
    parser.add_argument(
        "--out", default="shots/kernel-tests", help="output dir for serial.log (default: %(default)s)"
    )
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument(
        "--accel",
        default="auto",
        choices=["auto", "none", "tcg", "whpx", "kvm"],
        help="QEMU accelerator (default: %(default)s)",
    )
    parser.add_argument("--memory", default="256M", help="guest RAM (default: %(default)s)")
    # The full suite already takes ~235 s under TCG (`--accel none`, and the
    # `auto` fallback when KVM is unusable), so the old 240 s default failed a
    # correct suite on any slower host. A real hang still fails, just later.
    parser.add_argument(
        "--timeout", type=float, default=600.0, help="seconds to wait for TEST:SUMMARY"
    )
    parser.add_argument("--no-build", action="store_true", help="skip the cargo build step")
    parser.add_argument(
        "--machine",
        help="QEMU machine type, e.g. q35 (default: QEMU's default, i440fx)",
    )
    parser.add_argument(
        "--ide-disk",
        action="store_true",
        help="attach the image as IDE (ATA) instead of the default legacy "
        "virtio-blk, so the ATA driver is exercised",
    )
    parser.add_argument(
        "--virtio-disk",
        action="store_true",
        help="accepted for old scripts: virtio-blk is the default (issue #283)",
    )
    parser.add_argument(
        "--nic",
        action="store_true",
        help="add a legacy virtio-net function, which enables the end-to-end "
        "device interrupt test (issue #283)",
    )
    args = parser.parse_args()

    image = Path(args.image).resolve()
    out_dir = (ROOT / args.out).resolve() if not Path(args.out).is_absolute() else Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    serial_log = out_dir / "serial.log"

    if not args.no_build:
        build_test_image(image)
    elif not image.is_file():
        sys.exit(f"--no-build given but {image} does not exist")

    qemu = find_qemu(args.qemu)
    port = free_port()
    extra = accel_args(args.accel, qemu)
    extra = list(extra or [])
    if args.machine:
        extra += ["-machine", args.machine]
    if args.nic:
        extra += [
            "-netdev", "user,id=n0",
            "-device", "virtio-net-pci,netdev=n0,disable-modern=on",
        ]
    if not args.ide_disk:
        # A blank 16 MiB virtio disk for the virtio request-path tests, which
        # write to it; the boot image is never touched. Not attached with
        # --ide-disk: a virtio disk would take the boot slot from ATA.
        scratch = out_dir / "scratch.img"
        scratch.write_bytes(b"")
        with scratch.open("r+b") as handle:
            handle.truncate(SCRATCH_BYTES)
        extra += [
            "-drive", f"if=none,id=scratch,format=raw,file={scratch.as_posix()}",
            "-device", "virtio-blk-pci,drive=scratch,disable-modern=on",
        ]
    command = build_qemu_command(
        qemu, str(image), port, serial_log, args.memory, extra, ide=args.ide_disk
    )
    print(f"launching: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

    text = ""
    qmp: Qmp | None = None
    try:
        qmp = Qmp("127.0.0.1", port, min(30.0, args.timeout))
        text = follow_serial(serial_log, proc, time.time() + args.timeout)
    except Exception as exc:  # QMP connect failure: still report what booted
        print(f"warning: QMP session failed: {exc}", file=sys.stderr)
    finally:
        stop_qemu(proc, qmp)

    if serial_log.is_file():
        text = serial_log.read_text(errors="replace")
    payload = parse_serial(text)
    payload["image"] = str(image)
    payload["qemu"] = qemu
    # Record the accelerator actually used (what `auto` resolved to).
    payload["accel"] = extra[1] if extra and extra[0] == "-accel" else "none"
    payload["qemu_exit_code"] = proc.returncode
    payload["missing_summary"] = payload["reported"] is None

    report_md = write_report(payload)
    passed = sum(1 for test in payload["tests"] if test["status"] == "pass")
    failed = sum(1 for test in payload["tests"] if test["status"] == "fail")
    print(f"\nTEST summary: PASS={passed} FAIL={failed}")
    print(f"report: {report_md}")

    if payload["missing_summary"]:
        if TEST_BANNER not in text:
            print(
                "error: no tests ran; the image does not look like a test build "
                "(run without --no-build)",
                file=sys.stderr,
            )
        else:
            print(
                "error: test build booted but never printed TEST:SUMMARY "
                "(panic or hang; see the serial log)",
                file=sys.stderr,
            )
        return 1
    if failed:
        return 1
    if payload["reported"]["pass"] != passed or payload["reported"]["fail"] != failed:
        print(
            "error: TEST:SUMMARY disagrees with the parsed results",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
