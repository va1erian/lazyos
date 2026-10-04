#!/usr/bin/env python3
"""Measure LazyOS latencies (docs/performance-plan.md, stage P0).

Builds the desktop with the kernel's latency instrumentation and the
virtio-net driver (`LAZYOS_DESKTOP=1 LAZYOS_NET=1 LAZYOS_PERF=1 cargo build`),
boots it headless with a virtio-net card and a forwarded host port, and
while it measures, knocks on that port from the host: every knock makes QEMU
deliver frames to the guest, so the card raises real interrupts whose
claimant is a userspace driver (`netdrv`). It moves the PS/2 mouse through
QMP and parses the kernel's `PERF:` serial lines into `docs/perf/report.md`
(+ `report.json`).

    python tools/perf/run.py                      # build, boot, measure, report
    python tools/perf/run.py --no-build           # re-measure the current image
    python tools/perf/run.py --accel none         # force TCG (informative only)
    python tools/perf/run.py --label "P1.1"       # also append a row to docs/perf/history.md

Metrics (see `kernel/src/perf/mod.rs` for exactly where each is stamped):

    irq_wake       device/PS/2 IRQ top half -> the task it woke runs
    input_read     raw input record published -> inputd's raw-bus poll returns it
    input_present  pointer record published -> the compositor's next present returns
    irqoff         one interrupts-off stretch inside a syscall
    ipc_rt         in-kernel Messenger echo round trip (no context switch)
    sleep_1ms      a 1 ms sleep of the kernel task, request -> return

Exit status is non-zero when the image never reaches the desktop or a metric
the run must produce (`irqoff`, `ipc_rt`, `input_read`, `sleep_1ms`) is missing.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import socket
import subprocess
import sys
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
from qemu_qmp import (  # noqa: E402
    DEFAULT_MEMORY, Qmp, accel_args, build_qemu_command, find_qemu, free_port, resolve_accel,
)

REPORT_DIR = ROOT / "docs" / "perf"
METRICS = ("irq_wake", "input_read", "input_present", "irqoff", "ipc_rt", "sleep_1ms")
REQUIRED = ("irqoff", "ipc_rt", "input_read", "sleep_1ms")
RE_METRIC = re.compile(
    r"PERF:(\w+):n=(\d+) p50_us=([\d.]+) p90_us=([\d.]+) p99_us=([\d.]+) "
    r"max_us=([\d.]+) mean_us=([\d.]+)"
)
RE_WORST = re.compile(r"PERF:irqoff_worst:us=([\d.]+) syscall=(0x[0-9a-f]+)")
READY = ("XUID:UP:PASS", "INPUTD:READY")
#: The kernel runs its IPC benchmark 15 s after boot and its sleep benchmark
#: (about 1 to 2 s long) at 17 s; reports come every 2 s.
IPC_BENCH_S = 15.0
SLEEP_BENCH_S = 17.0
REPORT_PERIOD_S = 2.0
#: `netdrv` prints this once its interrupt line is armed.
NET_READY = "NETDRV:READY"
#: Host port forwarded to the guest (nothing listens there: each knock is a
#: SYN, and QEMU retransmits it, all received frames).
KNOCK_GUEST_PORT = 7
KNOCK_PERIOD_S = 0.05


def build_image() -> Path:
    env = dict(os.environ, LAZYOS_DESKTOP="1", LAZYOS_NET="1", LAZYOS_PERF="1")
    if not (ROOT / "target" / "xui" / "xui-shell.elf").is_file():
        print("building xui apps: python tools/xui/build.py", flush=True)
        subprocess.run([sys.executable, str(ROOT / "tools" / "xui" / "build.py")], cwd=ROOT, check=True)
    print("building: LAZYOS_DESKTOP=1 LAZYOS_NET=1 LAZYOS_PERF=1 cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"cargo build failed:\n{result.stderr[-4000:]}")
    image = ROOT / "target" / "lazyos.img"
    if not image.is_file():
        sys.exit(f"build succeeded but {image} does not exist")
    return image


def read_serial(path: Path) -> str:
    return path.read_text(errors="replace") if path.is_file() else ""


def wait_for(path: Path, proc: subprocess.Popen, markers: tuple[str, ...], timeout: float) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        text = read_serial(path)
        if all(marker in text for marker in markers):
            return True
        if "PANIC" in text or proc.poll() is not None:
            return False
        time.sleep(0.25)
    return False


def move_mouse(qmp: Qmp, moves: int, pause: float) -> None:
    """`moves` single-packet moves, back and forth so the cursor stays on screen.
    Each move is its own PS/2 packet and its own IRQ12 burst; the pause keeps
    them apart so each is measured on its own, not merged on the bus."""
    for index in range(moves):
        step = 6 if (index // 20) % 2 == 0 else -6
        qmp.mouse_move(step, step // 2, delay=0)
        time.sleep(pause)


def knock(port: int, stop: threading.Event) -> None:
    """Open (and drop) TCP connections to the forwarded port until `stop`."""
    while not stop.is_set():
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=KNOCK_PERIOD_S):
                pass
        except OSError:
            pass
        stop.wait(KNOCK_PERIOD_S)


def parse(text: str) -> dict:
    metrics: dict[str, dict] = {}
    for match in RE_METRIC.finditer(text):
        name = match.group(1)
        fields = ("n", "p50_us", "p90_us", "p99_us", "max_us", "mean_us")
        values = [int(match.group(2))] + [float(match.group(i)) for i in range(3, 8)]
        metrics[name] = dict(zip(fields, values))  # the last line of a metric wins
    worst = None
    for match in RE_WORST.finditer(text):
        worst = {"us": float(match.group(1)), "syscall": match.group(2)}
    return {"metrics": metrics, "irqoff_worst": worst}


def git_commit() -> str:
    result = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, capture_output=True, text=True)
    dirty = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, capture_output=True, text=True)
    return result.stdout.strip() + ("+dirty" if dirty.stdout.strip() else "")


def kernel_profile() -> str:
    """The cargo profile the image's kernel was built with, from Cargo.toml."""
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r"\[profile\.dev\]\s*\n(.*?)\n\n", text, re.S)
    opts = " ".join(line.strip() for line in match.group(1).splitlines()) if match else "?"
    return f"dev ({opts}; kernel crate inherits it)"


def write_report(payload: dict) -> Path:
    REPORT_DIR.mkdir(parents=True, exist_ok=True)
    (REPORT_DIR / "report.json").write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    meta = payload["meta"]
    lines = [
        "# Latency report",
        "",
        "Generated by `python tools/perf/run.py` (docs/performance-plan.md, P0). "
        "Do not edit by hand.",
        "",
        f"- Generated: {meta['generated']}",
        f"- Commit: `{meta['commit']}`" + (f" ({meta['label']})" if meta.get("label") else ""),
        f"- Accelerator: {meta['accel']}",
        f"- Kernel build profile: {meta['profile']}",
        f"- Mouse moves: {meta['moves']} PS/2 packets, {meta['pause_ms']} ms apart",
        "",
        "Provisional: the plan's gates are taken on the optimized (release-profile) "
        "kernel build, which is being introduced separately; numbers from the dev "
        "profile above are for comparing changes against each other."
        if meta["profile"].startswith("dev") else "Taken on the optimized build.",
        "",
        "| Metric | n | p50 µs | p90 µs | p99 µs | max µs | mean µs |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for name in METRICS:
        row = payload["metrics"].get(name)
        if row is None:
            lines.append(f"| `{name}` | 0 | - | - | - | - | - |")
            continue
        lines.append(
            f"| `{name}` | {row['n']} | {row['p50_us']:.1f} | {row['p90_us']:.1f} | "
            f"{row['p99_us']:.1f} | {row['max_us']:.1f} | {row['mean_us']:.1f} |"
        )
    worst = payload.get("irqoff_worst")
    if worst:
        lines += ["", f"Worst interrupts-off syscall stretch: {worst['us']:.1f} µs in syscall `{worst['syscall']}` "
                  "(bit 63 set: native `int 0x80` number; otherwise Linux)."]
    lines += ["", "Metric definitions are in `kernel/src/perf/mod.rs`; the history of runs is "
              "`docs/perf/history.md`.", ""]
    path = REPORT_DIR / "report.md"
    path.write_text("\n".join(lines), encoding="utf-8")
    return path


def append_history(payload: dict) -> None:
    path = REPORT_DIR / "history.md"
    if not path.is_file():
        header = [
            "# Latency history", "",
            "One row per labelled `python tools/perf/run.py --label ...` run. Microseconds.", "",
            "| Label | Commit | Accel | irq_wake p50/p99/max | input_read p50/p99/max | "
            "input_present p50/p99/max | irqoff p99/max | ipc_rt p50/p99 | sleep_1ms p50/p99/max |",
            "|---|---|---|---|---|---|---|---|---|",
        ]
        path.write_text("\n".join(header) + "\n", encoding="utf-8")
    metrics, meta = payload["metrics"], payload["meta"]

    def cell(name: str, keys: tuple[str, ...]) -> str:
        row = metrics.get(name)
        if row is None:
            return "-"
        return "/".join(f"{row[key]:.0f}" if row[key] >= 100 else f"{row[key]:.1f}" for key in keys) + f" (n={row['n']})"

    row = (
        f"| {meta['label']} | `{meta['commit']}` | {meta['accel']} | "
        f"{cell('irq_wake', ('p50_us', 'p99_us', 'max_us'))} | "
        f"{cell('input_read', ('p50_us', 'p99_us', 'max_us'))} | "
        f"{cell('input_present', ('p50_us', 'p99_us', 'max_us'))} | "
        f"{cell('irqoff', ('p99_us', 'max_us'))} | {cell('ipc_rt', ('p50_us', 'p99_us'))} | "
        f"{cell('sleep_1ms', ('p50_us', 'p99_us', 'max_us'))} |"
    )
    with path.open("a", encoding="utf-8") as handle:
        handle.write(row + "\n")


def stop_qemu(proc: subprocess.Popen, qmp: Qmp | None) -> None:
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


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", default=str(ROOT / "target" / "lazyos.img"))
    parser.add_argument("--out", default="shots/perf", help="output dir (serial.log)")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default=DEFAULT_MEMORY)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--moves", type=int, default=200, help="PS/2 mouse packets to send")
    parser.add_argument("--pause", type=float, default=0.06, help="seconds between packets")
    parser.add_argument("--boot-timeout", type=float, default=180.0)
    parser.add_argument("--label", help="append this run to docs/perf/history.md under this label")
    parser.add_argument("--no-knock", action="store_true",
                        help="no host traffic (no device interrupts; isolates the input path)")
    args = parser.parse_args()

    image = Path(args.image) if args.no_build else build_image()
    qemu = find_qemu(args.qemu)
    out = (ROOT / args.out) if not Path(args.out).is_absolute() else Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    serial_log = out / "serial.log"
    serial_log.unlink(missing_ok=True)
    accel = resolve_accel(args.accel, qemu)
    knock_port = free_port()
    extra = accel_args(args.accel, qemu) + [
        "-netdev", f"user,id=n0,hostfwd=tcp:127.0.0.1:{knock_port}-:{KNOCK_GUEST_PORT}",
        "-device", "virtio-net-pci,netdev=n0",
    ]
    port = free_port()
    command = build_qemu_command(qemu, str(image), port, serial_log, memory=args.memory, extra_args=extra)
    print(f"booting ({accel}): {' '.join(command)}", flush=True)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    qmp = None
    try:
        qmp = Qmp("127.0.0.1", port, timeout=30)
        if not wait_for(serial_log, proc, READY, args.boot_timeout):
            print(read_serial(serial_log)[-3000:])
            print("FAIL: the desktop never came up", file=sys.stderr)
            return 1
        if not wait_for(serial_log, proc, (NET_READY,), 60):
            print("warning: netdrv never came up: no device interrupts", file=sys.stderr)
        print("desktop up; knocking on the NIC, waiting for the IPC benchmark", flush=True)
        stop = threading.Event()
        knocker = threading.Thread(target=knock, args=(knock_port, stop), daemon=True)
        if not args.no_knock:
            knocker.start()
        boot = time.time()
        time.sleep(max(5.0, SLEEP_BENCH_S + 4 - (time.time() - boot)))
        print(f"moving the mouse: {args.moves} packets", flush=True)
        move_mouse(qmp, args.moves, args.pause)
        stop.set()
        if knocker.is_alive():
            knocker.join()
        time.sleep(REPORT_PERIOD_S * 2 + 1)
    finally:
        stop_qemu(proc, qmp)

    text = read_serial(serial_log)
    parsed = parse(text)
    payload = {
        "meta": {
            "generated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "commit": git_commit(),
            "label": args.label,
            "accel": accel,
            "profile": kernel_profile(),
            "moves": args.moves,
            "pause_ms": int(args.pause * 1000),
        },
        **parsed,
    }
    report = write_report(payload)
    if args.label:
        append_history(payload)
    for name in METRICS:
        row = parsed["metrics"].get(name)
        print(f"  {name:14} " + (json.dumps(row) if row else "no samples"))
    print(f"report: {report.relative_to(ROOT)}")
    missing = [name for name in REQUIRED if name not in parsed["metrics"]]
    if missing:
        print(f"FAIL: no samples for {', '.join(missing)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
