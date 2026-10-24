#!/usr/bin/env python3
"""Run the LazyOS in-kernel test suite (issue #62) in headless QEMU.

Builds the test image (`LAZYOS_TESTS=1 cargo build`), boots it with a QMP
socket, captures the serial log, parses the `TEST:` protocol, and writes
`docs/test/report.md` + `docs/test/report.json`.

Protocol (one line per test, emitted by `kernel/src/tests/mod.rs::run()`):

    TEST:<name>:START               (before the test runs: names it if it hangs)
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
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
REPORT_DIR = ROOT / "docs" / "test"

sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from serialwatch import RE_RESULT, follow_serial, running_test  # noqa: E402
from qemu_qmp import (  # noqa: E402
    DEFAULT_MEMORY, Qmp, accel_args, build_qemu_command, find_qemu, free_port, smp_type,
)

RE_SUMMARY = re.compile(r"^TEST:SUMMARY:PASS=(\d+) FAIL=(\d+)\s*$")
RE_INFO = re.compile(r"^TEST:([^:]+):(INFO|PROGRESS):(.*)$")

TEST_BANNER = "kernel test mode"
# Must match `SCRATCH_SECTORS` in kernel/src/tests/virtio_suite.rs.
SCRATCH_BYTES = 16 * 1024 * 1024
#: The modern-only scratch disk (`virtio_modern_suite`'s `MODERN_SECTORS`).
MODERN_SCRATCH_BYTES = 24 * 1024 * 1024


def build_test_image(image: Path, soak: bool = False, chaos_seed: str | None = None) -> None:
    """`cargo build` with LAZYOS_TESTS=1, which rebuilds the kernel and image.

    `soak` adds LAZYOS_SMP_SOAK=1: the SMP suite compiles its long soaks then
    (docs/smp-plan.md S8.3+); a plain test image stays fast to build."""
    env = dict(os.environ, LAZYOS_TESTS="1")
    if soak:
        env["LAZYOS_SMP_SOAK"] = "1"
    # Either way, so an inherited seed never turns a plain run into a chaos one.
    env["LAZYOS_CHAOS_SEED"] = chaos_seed or ""
    print("building test image: LAZYOS_TESTS=1 cargo build", flush=True)
    result = subprocess.run(
        ["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True
    )
    if result.returncode != 0:
        sys.exit(f"cargo build failed:\n{result.stderr[-4000:]}")
    if not image.is_file():
        sys.exit(f"build succeeded but {image} does not exist")


# What ahci_suite prints when no scratch AHCI disk is attached.
AHCI_SKIPPED = "no scratch AHCI disk; skipped"


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
        and not payload.get("required_skipped")
    )
    report = {
        "generated": generated,
        "image": payload["image"],
        "qemu": payload["qemu"],
        "accel": payload["accel"],
        "qemu_exit_code": payload["qemu_exit_code"],
        "missing_summary": payload["missing_summary"],
        "required_skipped": bool(payload.get("required_skipped")),
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
    parser.add_argument("--memory", default=DEFAULT_MEMORY, help="guest RAM (default: %(default)s)")
    parser.add_argument(
        "--cpu",
        default="auto",
        help="QEMU -cpu model. The AVX state tests of issue #822 need XSAVE and "
        "AVX in the guest, which QEMU's own default model (qemu64) lacks (the kernel "
        "then stays on FXSAVE and they pass vacuously). 'auto' (default): max under "
        "TCG, host under KVM, and none under WHPX, whose default model already "
        "exposes the host's features (and which does not boot with max or host); "
        "'default' passes no -cpu",
    )
    # The full suite takes ~720 s under TCG (`--accel none`, and the `auto`
    # fallback when KVM is unusable) since the filesystem phases F3-F5 and the
    # block cache added their soaks, so the old 600 s default failed a correct
    # suite. A real hang still fails, just later.
    parser.add_argument(
        "--timeout", type=float, default=1200.0, help="seconds to wait for TEST:SUMMARY"
    )
    parser.add_argument(
        "--stall-seconds",
        type=float,
        default=180.0,
        help="report (once per stall, to the console and <out>/stall.txt) the test in "
        "flight, the serial tail and the guest's RIP when the serial log is silent this "
        "long; evidence only, the run still waits for --timeout (0: off; issue #787)",
    )
    parser.add_argument(
        "--stall-nmi",
        action="store_true",
        help="with a stall report, send the guest an NMI so the kernel prints its HANG: "
        "view (scheduler, tasks, locks) to the serial log; off by default because the "
        "SMP suites count NMIs",
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
    parser.add_argument(
        "--nvme",
        action="store_true",
        help="add a blank 16 MiB NVMe disk (QEMU -device nvme) for the NVMe "
        "request-path tests (docs/nvme-install-plan.md N1)",
    )
    parser.add_argument(
        "--ahci",
        action="store_true",
        help="add a blank 16 MiB disk on a QEMU AHCI controller (-device ahci) "
        "for the AHCI request-path tests (docs/ahci-plan.md A2)",
    )
    parser.add_argument(
        "--extra-arg",
        action="append",
        default=[],
        help="one more QEMU argument, repeatable (e.g. --extra-arg=-cpu --extra-arg=max)",
    )
    parser.add_argument(
        "--smp",
        type=smp_type,
        default=None,
        metavar="N",
        help="guest vCPUs (QEMU -smp; default: 1, QEMU's own). The APs stay parked in "
        "firmware until the kernel's SMP bring-up (docs/smp-plan.md), so a value above "
        "1 is safe to ask for and changes nothing until then",
    )
    parser.add_argument(
        "--soak",
        action="store_true",
        help="build with LAZYOS_SMP_SOAK=1: the SMP suite's long soaks (docs/smp-plan.md); "
        "a no-op until they land",
    )
    parser.add_argument(
        "--chaos-seed",
        metavar="N",
        help="build with LAZYOS_CHAOS_SEED=N: preemption points reschedule at seeded random "
        "(task::chaos, docs/smp-plan.md); a failure prints the seed that replays it",
    )
    args = parser.parse_args()
    if args.ahci and args.ide_disk:
        # The IDE boot disk has no bootindex, so the blank AHCI scratch disk
        # could be the one firmware boots; the two also test different drivers.
        parser.error("--ahci and --ide-disk cannot be combined")

    image = Path(args.image).resolve()
    out_dir = (ROOT / args.out).resolve() if not Path(args.out).is_absolute() else Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)
    serial_log = out_dir / "serial.log"
    (out_dir / "stall.txt").unlink(missing_ok=True)

    if not args.no_build:
        build_test_image(image, soak=args.soak, chaos_seed=args.chaos_seed)
    elif not image.is_file():
        sys.exit(f"--no-build given but {image} does not exist")

    qemu = find_qemu(args.qemu)
    port = free_port()
    extra = accel_args(args.accel, qemu)
    extra = list(extra or [])
    if args.machine:
        extra += ["-machine", args.machine]
    cpu = args.cpu
    if cpu == "auto":
        accel = extra[1] if extra and extra[0] == "-accel" else "none"
        cpu = {"none": "max", "kvm": "host"}.get(accel, "default")
    if cpu != "default" and "-cpu" not in args.extra_arg:
        extra += ["-cpu", cpu]
    extra += args.extra_arg
    if args.nic:
        extra += [
            "-netdev", "user,id=n0",
            "-device", "virtio-net-pci,netdev=n0,disable-modern=on",
        ]
    scratch: Path | None = None
    modern_scratch: Path | None = None
    if not args.ide_disk:
        # A blank 16 MiB virtio disk for the virtio request-path tests, which
        # write to it; the boot image is never touched. Not attached with
        # --ide-disk: a virtio disk would take the boot slot from ATA.
        # A fresh unique file, so `--image` can never be truncated by accident.
        with tempfile.NamedTemporaryFile(
            dir=out_dir, prefix="scratch-", suffix=".img", delete=False
        ) as handle:
            scratch = Path(handle.name).resolve()
            if scratch == image:
                sys.exit(f"scratch disk {scratch} is the boot image")
            handle.truncate(SCRATCH_BYTES)
        extra += [
            "-drive", f"if=none,id=scratch,format=raw,file={scratch.as_posix()}",
            "-device", "virtio-blk-pci,drive=scratch,disable-modern=on",
        ]
        # A second, modern-only one (issue #497): the kernel drives it through
        # the virtio 1.x transport (`virtio_modern_suite`). A different size
        # tells the two apart.
        with tempfile.NamedTemporaryFile(
            dir=out_dir, prefix="scratch-modern-", suffix=".img", delete=False
        ) as handle:
            modern_scratch = Path(handle.name).resolve()
            handle.truncate(MODERN_SCRATCH_BYTES)
        extra += [
            "-drive", f"if=none,id=mscratch,format=raw,file={modern_scratch.as_posix()}",
            "-device", "virtio-blk-pci,drive=mscratch,disable-legacy=on",
        ]
    nvme_scratch: Path | None = None
    if args.nvme:
        # A blank NVMe disk of the same size, written by nvme_suite; never
        # the boot disk.
        with tempfile.NamedTemporaryFile(
            dir=out_dir, prefix="nvme-", suffix=".img", delete=False
        ) as handle:
            nvme_scratch = Path(handle.name).resolve()
            if nvme_scratch == image:
                sys.exit(f"NVMe scratch disk {nvme_scratch} is the boot image")
            handle.truncate(SCRATCH_BYTES)
        extra += [
            "-drive", f"if=none,id=nvmescratch,format=raw,file={nvme_scratch.as_posix()}",
            "-device", "nvme,serial=lazyos-scratch,drive=nvmescratch",
        ]
    ahci_scratch: Path | None = None
    if args.ahci:
        # A blank disk on its own AHCI controller, written by ahci_suite;
        # never the boot disk. A controller of its own works on every machine
        # type (q35's built-in one stays empty).
        with tempfile.NamedTemporaryFile(
            dir=out_dir, prefix="ahci-", suffix=".img", delete=False
        ) as handle:
            ahci_scratch = Path(handle.name).resolve()
            if ahci_scratch == image:
                sys.exit(f"AHCI scratch disk {ahci_scratch} is the boot image")
            handle.truncate(SCRATCH_BYTES)
        extra += [
            "-device", "ahci,id=ahcis",
            "-drive", f"if=none,id=ahcischratch,format=raw,file={ahci_scratch.as_posix()}",
            "-device", "ide-hd,drive=ahcischratch,bus=ahcis.0",
        ]
    command = build_qemu_command(
        qemu, str(image), port, serial_log, args.memory, extra, ide=args.ide_disk,
        # The blank AHCI scratch disk must not become the disk firmware boots.
        boot_first=args.ahci, smp=args.smp,
    )
    print(f"launching: {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)

    text = ""
    qmp: Qmp | None = None
    try:
        qmp = Qmp("127.0.0.1", port, min(30.0, args.timeout))
        text = follow_serial(
            serial_log, proc, time.time() + args.timeout, qmp=qmp,
            stall_after=args.stall_seconds, stall_file=out_dir / "stall.txt",
            stall_nmi=args.stall_nmi,
        )
    except Exception as exc:  # QMP connect failure: still report what booted
        print(f"warning: QMP session failed: {exc}", file=sys.stderr)
    finally:
        stop_qemu(proc, qmp)
        if scratch is not None:
            scratch.unlink(missing_ok=True)
        if modern_scratch is not None:
            modern_scratch.unlink(missing_ok=True)
        if nvme_scratch is not None:
            nvme_scratch.unlink(missing_ok=True)
        if ahci_scratch is not None:
            ahci_scratch.unlink(missing_ok=True)

    if serial_log.is_file():
        text = serial_log.read_text(errors="replace")
    payload = parse_serial(text)
    payload["image"] = str(image)
    payload["qemu"] = qemu
    # Record the accelerator actually used (what `auto` resolved to).
    payload["accel"] = extra[1] if extra and extra[0] == "-accel" else "none"
    payload["qemu_exit_code"] = proc.returncode
    payload["missing_summary"] = payload["reported"] is None
    # The suite skips (and passes) without its scratch disk; a run that asked
    # for one must not count as ok, in the report or the exit code.
    payload["required_skipped"] = bool(
        args.ahci and any(AHCI_SKIPPED in line for line in payload["info"])
    )

    cpu_line = re.search(r"HW:CPU:fpu xsave_mask=(0x[0-9a-f]+) area=(\d+)", text)
    if cpu_line:
        avx = int(cpu_line.group(1), 16) & 4 != 0
        print(
            f"guest FPU state: xsave_mask={cpu_line.group(1)} area={cpu_line.group(2)}"
            + ("" if avx else " (no AVX: the AVX state tests passed vacuously; try --cpu max)")
        )

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
                f"(panic or hang; test in flight: {running_test(text) or 'unknown'}; "
                "see the serial log and stall.txt)",
                file=sys.stderr,
            )
        return 1
    if failed:
        return 1
    if payload["required_skipped"]:
        print(
            "error: --ahci requested but the AHCI scratch disk was not attached",
            file=sys.stderr,
        )
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
